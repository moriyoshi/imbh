//! `imbhd`'s runtime networking context: what the listeners need from bridge-network discovery.
//!
//! Two implementations of one shape. With the `docker` feature this owns the discovery thread, the
//! allow-list, and the multi-address supervisor, so `IMBH_LISTEN_ADDR=auto` binds every bridge
//! gateway the daemon has and keeps up as networks come and go (see
//! `imbh_server::docker::networks`). Without it there is nothing to discover, so it is an empty
//! struct whose `serve_*` methods are the single-address calls `imbhd` has always made — same code
//! path, same errors, and not one byte of the feature compiled in.
//!
//! Keeping both variants here rather than behind `cfg!` inside `main` is deliberate: the binary's
//! wiring is exactly what this file is for, and a reader of either build sees one straight-line
//! version of it.

use std::sync::Arc;

use imbh::Db;
use imbh_server::{Limits, Shutdown};

/// `imbhd`'s default OTLP/HTTP port, used when `auto` is written without one.
pub const HTTP_PORT: u16 = 4318;
/// `imbhd`'s default OTLP/gRPC port, likewise.
#[cfg(feature = "grpc")]
pub const GRPC_PORT: u16 = 4317;

#[cfg(all(feature = "docker", unix))]
mod imp {
    use super::*;
    use std::time::Duration;

    use imbh_server::docker::addr::{Access, AllowFrom, BindSpec};
    use imbh_server::docker::networks::{Api, Networks, refresh_interval};
    use imbh_server::docker::serve::serve_supervised_until;

    pub struct Net {
        networks: Arc<Networks>,
        access: Arc<Access>,
        refresh: Duration,
    }

    impl Net {
        /// Start discovery and resolve the access rule against what it found.
        ///
        /// The first scan happens here, synchronously, so the listeners started a moment later
        /// already have addresses to bind rather than coming up empty and filling in a tick later.
        pub fn new(shutdown: &Arc<Shutdown>) -> Result<Net, Box<dyn std::error::Error>> {
            let api = Api::parse(&std::env::var("IMBH_DOCKER_API").unwrap_or_default());
            let refresh = refresh_interval(std::env::var("IMBH_DOCKER_NETWORK_REFRESH").ok())?;
            let networks = Networks::new(api);
            networks.spawn(refresh, shutdown);

            let from = AllowFrom::parse(&std::env::var("IMBH_ALLOW_FROM").unwrap_or_default())
                .map_err(|e| format!("IMBH_ALLOW_FROM: {e}"))?;
            let access = Access::new(from, &networks.snapshot().subnets());
            // The allow-list follows the networks: a `docker` rule has to widen when a compose
            // project creates a network, or the containers on it are refused.
            let following = Arc::clone(&access);
            networks.on_change(move |snapshot| following.refresh(&snapshot.subnets()));

            Ok(Net {
                networks,
                access,
                refresh,
            })
        }

        /// How the peer filter currently reads, for the startup banner.
        pub fn describe_access(&self) -> Option<String> {
            self.access.is_filtering().then(|| self.access.describe())
        }

        /// What `auto` resolves to right now, for the startup banner.
        pub fn describe_addr(&self, addr: &str, default_port: u16) -> String {
            let Ok(spec) = BindSpec::parse(addr, default_port) else {
                return addr.to_owned();
            };
            match spec.is_dynamic() {
                false => addr.to_owned(),
                true => {
                    let resolved = spec.resolve(&self.gateways());
                    match resolved.is_empty() {
                        true => format!("{addr} (no bridge network found yet)"),
                        false => format!("{addr} → {}", resolved.join(", ")),
                    }
                }
            }
        }

        /// The discovery handle the log-driver plugin should stamp `container.network.*` from, or
        /// `None` when the operator turned the attributes off.
        ///
        /// `IMBH_DOCKER_NETWORK_ATTRS`: `on` (the default) or `off`. Handing the plugin the handle
        /// rather than a snapshot is what lets a container that started between two scans pick its
        /// networks up on the next one — the plugin must never ask the daemon itself.
        pub fn container_networks(
            &self,
            setting: Option<&str>,
        ) -> Result<Option<Arc<Networks>>, Box<dyn std::error::Error>> {
            match setting.unwrap_or("").trim() {
                "" | "on" | "true" => Ok(Some(Arc::clone(&self.networks))),
                "off" | "false" => Ok(None),
                other => Err(format!(
                    "IMBH_DOCKER_NETWORK_ATTRS: expected `on` or `off`, got `{other}`"
                )
                .into()),
            }
        }

        fn gateways(&self) -> Vec<std::net::IpAddr> {
            use imbh_server::docker::addr::Discovery;
            self.networks.gateways()
        }

        pub fn serve_http(
            &self,
            db: Arc<Db>,
            addr: &str,
            limits: Limits,
            shutdown: Arc<Shutdown>,
        ) -> Result<(), String> {
            let spec = BindSpec::parse(addr, HTTP_PORT).map_err(|e| format!("{addr}: {e}"))?;
            serve_supervised_until(
                db,
                &spec,
                limits,
                Arc::clone(&self.access),
                Some(Arc::clone(&self.networks) as Arc<_>),
                self.refresh,
                shutdown,
            )
            .map_err(|e| format!("HTTP server error on {addr}: {e}"))
        }

        #[cfg(feature = "grpc")]
        pub fn serve_grpc(
            &self,
            db: Arc<Db>,
            addr: &str,
            shutdown: Arc<Shutdown>,
        ) -> Result<(), String> {
            let spec = BindSpec::parse(addr, GRPC_PORT).map_err(|e| format!("{addr}: {e}"))?;
            imbh_server::docker::serve::serve_grpc_supervised_until(
                db,
                &spec,
                Arc::clone(&self.access),
                Some(Arc::clone(&self.networks) as Arc<_>),
                self.refresh,
                shutdown,
            )
            .map_err(|e| format!("gRPC server error on {addr}: {e}"))
        }
    }
}

#[cfg(not(all(feature = "docker", unix)))]
mod imp {
    use super::*;

    /// Nothing to discover, so nothing to hold.
    pub struct Net;

    impl Net {
        pub fn new(_shutdown: &Arc<Shutdown>) -> Result<Net, Box<dyn std::error::Error>> {
            Ok(Net)
        }

        pub fn describe_access(&self) -> Option<String> {
            None
        }

        pub fn describe_addr(&self, addr: &str, _default_port: u16) -> String {
            addr.to_owned()
        }

        pub fn serve_http(
            &self,
            db: Arc<Db>,
            addr: &str,
            limits: Limits,
            shutdown: Arc<Shutdown>,
        ) -> Result<(), String> {
            imbh_server::serve_with_limits_until(db, addr, limits, shutdown)
                .map_err(|e| format!("HTTP server error on {addr}: {e}"))
        }

        #[cfg(feature = "grpc")]
        pub fn serve_grpc(
            &self,
            db: Arc<Db>,
            addr: &str,
            shutdown: Arc<Shutdown>,
        ) -> Result<(), String> {
            imbh_server::grpc::serve_grpc_blocking_until(db, addr, shutdown)
                .map_err(|e| format!("gRPC server error on {addr}: {e}"))
        }
    }
}

pub use imp::Net;
