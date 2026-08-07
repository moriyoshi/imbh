//! Invariants of the shipped managed-plugin `config.json` (docs/DOCKER_LOG_DRIVER.md).
//!
//! These are cheap to assert and expensive to get wrong: the file is not exercised by any build --
//! `docker plugin create` consumes it at package time, and a mistake surfaces as a plugin that
//! refuses to enable on a user's machine, with the real error in the Docker daemon log and no
//! `docker plugin logs` to read it with.
//!
//! Deliberately ungated by the `docker` feature: the file ships regardless, so `cargo test
//! --workspace` should check it regardless.

use std::path::PathBuf;

fn config() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docker-plugin/config.json");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()))
}

/// The database directory must be a `propagatedMount`, never a bind mount.
///
/// A managed plugin's bind mount needs its source to exist on the daemon's filesystem *already*: the
/// daemon will not create one, and `docker plugin enable` fails in the OCI runtime with `error
/// mounting "/var/lib/imbh" to rootfs ... no such file or directory`. The plugin cannot rescue
/// itself either, because mounts are established before the entrypoint runs -- `imbhd` never reaches
/// the `create_dir_all` it already performs. A `propagatedMount` is provisioned by the daemon, so
/// there is nothing for a user to create and nothing to get wrong, on any platform.
#[test]
fn the_database_directory_is_daemon_provisioned_not_bind_mounted() {
    let cfg = config();

    let propagated = cfg["propagatedMount"].as_str().unwrap_or("");
    assert_eq!(
        propagated, "/var/lib/imbh",
        "the database directory must be declared as the plugin's propagatedMount",
    );

    let mounts = cfg["mounts"].as_array().cloned().unwrap_or_default();
    assert!(
        mounts.is_empty(),
        "no bind mount may be reintroduced: a missing bind source is the one thing the daemon \
         will not create for a plugin, and it fails `plugin enable`. Found: {mounts:?}",
    );
}

/// The entrypoint's data-directory argument must be the directory that actually persists.
///
/// `imbhd`'s database path is frozen into `entrypoint` (a plugin's entrypoint args cannot be changed
/// by `docker plugin set`). If it drifts from `propagatedMount`, the plugin still starts and still
/// logs -- into the plugin's rootfs, which is replaced wholesale on upgrade. That is silent data
/// loss with no error anywhere, so pin the two together.
#[test]
fn the_entrypoint_writes_into_the_propagated_mount() {
    let cfg = config();
    let entrypoint: Vec<&str> = cfg["entrypoint"]
        .as_array()
        .expect("entrypoint is an array")
        .iter()
        .map(|v| v.as_str().expect("entrypoint args are strings"))
        .collect();

    let data_dir = entrypoint
        .get(1)
        .expect("entrypoint is [imbhd, <data dir>]");
    assert_eq!(
        *data_dir,
        cfg["propagatedMount"].as_str().unwrap_or(""),
        "imbhd's data directory must be the propagatedMount, or the database is written to \
         non-persistent storage in the plugin's rootfs",
    );
}

/// `IMBH_DOCKER_PLUGIN_SOCKET` must agree with `interface.socket`, and must not be settable.
///
/// The daemon dials `/run/docker/plugins/<id>/<interface.socket>`; `imbhd` serves whatever the
/// environment variable names. If they disagree the plugin builds, packages and starts, then fails
/// activation with a `dial unix ... no such file or directory` that names neither side of the
/// mismatch. `src/docker/mod.rs` documents the same coupling from the Rust side.
#[test]
fn the_plugin_socket_path_agrees_with_the_declared_interface() {
    let cfg = config();
    let socket = cfg["interface"]["socket"]
        .as_str()
        .expect("interface.socket is a string");

    let env = cfg["env"]
        .as_array()
        .expect("env is an array")
        .iter()
        .find(|e| e["name"] == "IMBH_DOCKER_PLUGIN_SOCKET")
        .expect("IMBH_DOCKER_PLUGIN_SOCKET is declared");

    assert_eq!(
        env["value"].as_str().unwrap_or(""),
        format!("/run/docker/plugins/{socket}"),
        "IMBH_DOCKER_PLUGIN_SOCKET must point at the socket name interface.socket declares",
    );
    assert!(
        env["settable"].as_array().is_none_or(|s| s.is_empty()),
        "IMBH_DOCKER_PLUGIN_SOCKET must not be settable: moving it away from interface.socket \
         breaks activation with an error that names neither side",
    );
}

/// Every listener address must be discovered, not baked in.
///
/// The plugin used to ship `172.17.0.1:4318` and rely on `build.sh` running `docker network inspect`
/// once and applying the answer with `docker plugin set`. Anyone who installed straight from the
/// registry -- which is what the documented install does -- got the literal default, and a daemon
/// with a custom `bip`, or one whose docker0 was re-created, then had a listener bound to an address
/// it does not have. The failure is silent: container logging is filesystem-only and keeps working,
/// so the only symptom is a query endpoint nothing answers on.
///
/// `auto` binds every bridge gateway the daemon actually has, re-checked on a timer. Pinning this
/// here because it is a one-word edit away from regressing to the shape that shipped broken.
#[test]
fn the_listener_addresses_are_discovered_rather_than_hard_coded() {
    let cfg = config();
    for name in ["IMBH_LISTEN_ADDR", "IMBH_GRPC_LISTEN_ADDR"] {
        let value = env_value(&cfg, name);
        assert_eq!(
            value, "auto",
            "{name} must default to `auto`, not to a literal address this daemon may not have",
        );
    }
}

/// The discovery knobs must be tunable on an installed plugin.
///
/// A plugin's `entrypoint` is frozen in this file, so anything an operator may need to change has to
/// arrive as a `settable` env entry -- otherwise the only way to alter it is to rebuild and
/// re-register the plugin, which destroys the database (`plugin rm` is `DROP DATABASE`).
#[test]
fn the_network_discovery_settings_are_declared_and_settable() {
    let cfg = config();
    for (name, default) in [
        ("IMBH_ALLOW_FROM", "any"),
        ("IMBH_DOCKER_API", "auto"),
        ("IMBH_DOCKER_NETWORK_REFRESH", "30s"),
        // Docker Desktop is where `auto` alone is not enough -- the daemon is in a VM whose
        // host-facing interface is not a bridge -- and it is also where an operator is most
        // likely to want the extra listener back off again.
        ("IMBH_DOCKER_VM_NET", "auto"),
    ] {
        assert_eq!(env_value(&cfg, name), default, "{name} default");
        let settable = env_entry(&cfg, name)["settable"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            settable.iter().any(|s| s.as_str() == Some("value")),
            "{name} must be settable with `docker plugin set`",
        );
    }
}

/// `IMBH_ALLOW_FROM` must default to filtering nothing.
///
/// The listeners are reachable from every container on the box, which is exactly what makes the
/// plugin useful; narrowing that is an operator's decision, and a default of `docker` would break
/// any deployment reaching the endpoint from somewhere else the moment it upgraded.
#[test]
fn the_allow_list_is_off_by_default() {
    assert_eq!(env_value(&config(), "IMBH_ALLOW_FROM"), "any");
}

fn env_entry<'a>(cfg: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    cfg["env"]
        .as_array()
        .expect("env is an array")
        .iter()
        .find(|entry| entry["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("config.json declares no {name}"))
}

fn env_value(cfg: &serde_json::Value, name: &str) -> String {
    env_entry(cfg, name)["value"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}
