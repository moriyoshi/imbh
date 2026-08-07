//! Listeners whose addresses come from discovery rather than from configuration.
//!
//! `IMBH_LISTEN_ADDR=auto` means "every bridge gateway this daemon has". That set is not knowable
//! at startup alone — a `docker compose up` creates a network, `docker network rm` destroys one —
//! so a listener bound to it has to be supervised: bind what the spec resolves to now, and revisit
//! on a timer.
//!
//! The accept loop itself is unchanged and unduplicated. [`crate::serve_on_listener`] takes an
//! already-bound listener precisely so several of them can share one runtime; everything this
//! module adds is the bookkeeping around it, and none of it is compiled into a build without the
//! `docker` feature.
//!
//! ## Which bind failures are fatal
//!
//! A **literal** address that will not bind has always been fatal, and still is: an address this
//! host does not have, or a port already in use, is a configuration error, and starting anyway
//! would leave the operator believing in an endpoint that nothing serves. A **discovered** address
//! that will not bind is not fatal — a bridge that went away between the scan and the `bind` is
//! ordinary, and the next tick will notice.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use imbh::Db;

use super::addr::{Access, BindSpec, Discovery};
use crate::shutdown::Shutdown;
use crate::{Limits, warn};

/// Extra patience beyond each listener's own drain, covering the wake-up. A listener past this is
/// not coming back before the process exits.
const STOP_GRACE: Duration = Duration::from_secs(1);

/// One supervised listener: the token that stops just this one, and the task serving it.
struct Live {
    stop: Arc<Shutdown>,
    task: tokio::task::JoinHandle<()>,
}

/// Serve `db` over HTTP on every address `spec` resolves to, keeping that set in step with
/// `discovery` until `shutdown` trips.
///
/// Blocking: it owns the runtime for its whole life, so `imbhd`'s `main` runs it on a plain thread
/// exactly as it runs the single-address [`crate::serve_with_limits_until`]. Every listener under
/// it is a task on that one runtime rather than a thread of its own — N bridge networks cost N
/// sockets, not N runtimes.
pub fn serve_supervised_until(
    db: Arc<Db>,
    spec: &BindSpec,
    limits: Limits,
    access: Arc<Access>,
    discovery: Option<Arc<dyn Discovery>>,
    refresh: Duration,
    shutdown: Arc<Shutdown>,
) -> std::io::Result<()> {
    // Multi-threaded on purpose: `offload` needs `block_in_place`, which a current-thread runtime
    // does not have, and one blocking `Db` call would otherwise stop the listener answering.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let app = crate::app(db);
        let allow = access.filter();
        supervise(spec, discovery, refresh, &shutdown, |listener, stop| {
            tokio::spawn(crate::serve_on_listener(
                app.clone(),
                listener,
                limits,
                allow.clone(),
                stop,
            ))
        })
        .await
    })
}

/// [`serve_supervised_until`] for OTLP/gRPC.
///
/// tonic serves the *pre-bound* listener through `serve_with_incoming_shutdown`, so the supervisor
/// owns the socket and the "which failures are fatal" rule is the same one the HTTP side follows.
/// The allow-list is applied to the incoming stream, which puts a refused peer in exactly the place
/// the HTTP accept loop puts it: closed before a byte is read.
#[cfg(feature = "grpc")]
pub fn serve_grpc_supervised_until(
    db: Arc<Db>,
    spec: &BindSpec,
    access: Arc<Access>,
    discovery: Option<Arc<dyn Discovery>>,
    refresh: Duration,
    shutdown: Arc<Shutdown>,
) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let allow = access.filter();
        supervise(spec, discovery, refresh, &shutdown, |listener, stop| {
            let db = Arc::clone(&db);
            let allow = allow.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::grpc::serve_grpc_on_listener(db, listener, allow, stop).await
                {
                    warn(&format!("gRPC listener stopped: {e}"));
                }
            })
        })
        .await
    })?;
    Ok(())
}

/// Bind what `spec` resolves to, then revisit on `refresh` until `shutdown` trips.
///
/// `start` is handed each newly-bound listener and the token that stops just that one, and returns
/// the task serving it. Split from the protocol specifics so HTTP and gRPC share one supervisor and
/// one definition of what a changing address set means.
async fn supervise(
    spec: &BindSpec,
    discovery: Option<Arc<dyn Discovery>>,
    refresh: Duration,
    shutdown: &Arc<Shutdown>,
    mut start: impl FnMut(tokio::net::TcpListener, Arc<Shutdown>) -> tokio::task::JoinHandle<()>,
) -> std::io::Result<()> {
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    shutdown.on_trigger(move || {
        let _ = stop_tx.send(());
    });

    let mut live: HashMap<String, Live> = HashMap::new();
    // A rescan only makes sense while something can change. Without one this is a plain accept loop
    // that happens to be supervised, which is what a purely literal spec should cost.
    let ticking = spec.is_dynamic() && !refresh.is_zero() && discovery.is_some();
    let mut first = true;

    loop {
        let gateways = discovery.as_ref().map(|d| d.gateways()).unwrap_or_default();
        let wanted = spec.resolve(&gateways);

        // Retired first, so a bridge that changed its address frees the old socket before the new
        // one is bound — the same port on a different address is otherwise a needless conflict.
        live.retain(|addr, entry| {
            // A listener whose task has ended is not listening, whatever the address set says: it
            // gave up after `crate::ACCEPT_FAILURES_BEFORE_RETIRING` consecutive accept failures.
            // Forgetting it here is what lets the bind below bring the address back on this tick —
            // without it, a socket that stopped accepting would stay in the map for ever and the
            // endpoint would be silently gone.
            if entry.task.is_finished() {
                warn(&format!("listener on {addr} stopped; rebinding"));
                return false;
            }
            let keep = wanted.contains(addr);
            if !keep {
                warn(&format!("no longer listening on {addr}"));
                entry.stop.trigger();
            }
            keep
        });

        for addr in &wanted {
            if live.contains_key(addr) {
                continue;
            }
            let listener = match tokio::net::TcpListener::bind(addr.as_str()).await {
                Ok(listener) => listener,
                Err(e) if first && spec.is_required(addr) => return Err(e),
                Err(e) => {
                    warn(&format!("cannot bind {addr}: {e}"));
                    continue;
                }
            };
            let stop = Shutdown::with_drain_timeout(shutdown.drain_timeout());
            let task = start(listener, Arc::clone(&stop));
            live.insert(addr.clone(), Live { stop, task });
        }

        if first && live.is_empty() && !spec.is_empty() {
            // Not fatal: a plugin whose daemon has no bridge network yet still logs containers, and
            // the endpoint appears as soon as one exists. But silence here would be indistinguishable
            // from a working endpoint, which is the failure this whole feature exists to end.
            warn(&format!(
                "nothing bound for `{}` yet: no address resolved",
                spec.describe()
            ));
        }
        first = false;

        if !ticking {
            let _ = (&mut stop_rx).await;
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep(refresh) => continue,
            _ = &mut stop_rx => break,
        }
    }

    // Every listener drains on its own token, so shutdown only has to trip them and wait out the
    // drain each one bounds itself by.
    for entry in live.values() {
        entry.stop.trigger();
    }
    let draining = async {
        for entry in live.into_values() {
            let _ = entry.task.await;
        }
    };
    if tokio::time::timeout(shutdown.drain_timeout() + STOP_GRACE, draining)
        .await
        .is_err()
    {
        warn("listener(s) still draining when the supervisor gave up waiting");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, SocketAddr};
    use std::sync::Mutex;

    use crate::docker::addr::{AllowFrom, Cidr};

    /// A `Discovery` whose answer the test moves under the supervisor's feet.
    struct Fake(Mutex<Vec<IpAddr>>);

    impl Discovery for Fake {
        fn gateways(&self) -> Vec<IpAddr> {
            self.0.lock().expect("gateways").clone()
        }
        fn subnets(&self) -> Vec<Cidr> {
            Vec::new()
        }
    }

    /// Is something accepting on this address?
    fn reachable(addr: &str) -> bool {
        let addr: SocketAddr = addr.parse().expect("an address");
        std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok()
    }

    /// Wait for `check` to hold, up to a second — the supervisor's tick is asynchronous.
    fn eventually(what: &str, check: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if check() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for {what}");
    }

    /// Two loopback aliases, which Linux has by default — a stand-in for two bridge gateways, so
    /// the supervisor's add/remove behaviour is testable without a daemon or a netns.
    const ONE: &str = "127.0.0.2";
    const TWO: &str = "127.0.0.3";

    #[test]
    #[cfg_attr(not(target_os = "linux"), ignore = "needs 127.0.0.0/8 aliases")]
    fn listeners_follow_the_addresses_discovery_reports() {
        let port = 47311;
        let dir = tempdir("supervisor-follows");
        let db = imbh::Db::builder(&dir).open().expect("open");
        let discovery = Arc::new(Fake(Mutex::new(vec![ONE.parse().expect("ip")])));
        let shutdown = Shutdown::new();
        let spec = BindSpec::parse("auto", port).expect("spec");

        let serving = std::thread::spawn({
            let (db, discovery, shutdown) = (db.clone(), Arc::clone(&discovery), shutdown.clone());
            move || {
                serve_supervised_until(
                    db,
                    &spec,
                    Limits::default(),
                    Access::unrestricted(),
                    Some(discovery),
                    Duration::from_millis(50),
                    shutdown,
                )
            }
        });

        eventually("the first gateway to be served", || {
            reachable(&format!("{ONE}:{port}"))
        });
        assert!(!reachable(&format!("{TWO}:{port}")));

        // A network appears: a listener must appear with it, without restarting anything.
        *discovery.0.lock().expect("gateways") = vec![ONE.parse().unwrap(), TWO.parse().unwrap()];
        eventually("the second gateway to be served", || {
            reachable(&format!("{TWO}:{port}"))
        });

        // ...and a network goes away: its listener must go with it.
        *discovery.0.lock().expect("gateways") = vec![TWO.parse().unwrap()];
        eventually("the first gateway to stop being served", || {
            !reachable(&format!("{ONE}:{port}"))
        });
        assert!(reachable(&format!("{TWO}:{port}")));

        shutdown.trigger();
        serving.join().expect("thread").expect("clean stop");
        db.blocking().close().expect("close");
    }

    #[test]
    fn a_literal_address_that_cannot_bind_is_still_fatal() {
        let dir = tempdir("supervisor-fatal");
        let db = imbh::Db::builder(&dir).open().expect("open");
        // Hold the port, so the supervisor's bind cannot succeed.
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = held.local_addr().expect("addr").to_string();

        let error = serve_supervised_until(
            db.clone(),
            &BindSpec::parse(&addr, 4318).expect("spec"),
            Limits::default(),
            Access::unrestricted(),
            None,
            Duration::ZERO,
            Shutdown::new(),
        )
        .expect_err("a literal address in use must not start silently");
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
        db.blocking().close().expect("close");
    }

    #[test]
    #[cfg_attr(not(target_os = "linux"), ignore = "needs 127.0.0.0/8 aliases")]
    fn a_discovered_address_that_cannot_bind_only_costs_that_one_listener() {
        let dir = tempdir("supervisor-partial");
        let db = imbh::Db::builder(&dir).open().expect("open");
        // Take one of the two gateways' ports before the supervisor can.
        let held = std::net::TcpListener::bind(format!("{ONE}:0")).expect("bind");
        let port = held.local_addr().expect("addr").port();

        let discovery = Arc::new(Fake(Mutex::new(vec![
            ONE.parse().expect("ip"),
            TWO.parse().expect("ip"),
        ])));
        let shutdown = Shutdown::new();
        let serving = std::thread::spawn({
            let (db, discovery, shutdown) = (db.clone(), Arc::clone(&discovery), shutdown.clone());
            move || {
                serve_supervised_until(
                    db,
                    &BindSpec::parse("auto", port).expect("spec"),
                    Limits::default(),
                    Access::unrestricted(),
                    Some(discovery),
                    Duration::from_millis(50),
                    shutdown,
                )
            }
        });

        // The other gateway is served regardless — one unavailable bridge must not cost the endpoint.
        eventually("the bindable gateway to be served", || {
            reachable(&format!("{TWO}:{port}"))
        });
        shutdown.trigger();
        serving
            .join()
            .expect("thread")
            .expect("a discovered address failing to bind is not fatal");
        db.blocking().close().expect("close");
    }

    /// The allow-list at the only place it can be enforced honestly: on accept, before a request.
    #[test]
    #[cfg_attr(not(target_os = "linux"), ignore = "needs 127.0.0.0/8 aliases")]
    fn a_peer_outside_the_allow_list_is_closed_without_being_served() {
        use std::io::{Read, Write};

        let dir = tempdir("supervisor-allow");
        let db = imbh::Db::builder(&dir).open().expect("open");
        let held = std::net::TcpListener::bind(format!("{ONE}:0")).expect("probe");
        let port = held.local_addr().expect("addr").port();
        drop(held);

        // Everything except loopback — and the connections below come *from* loopback, since a
        // client binding 127.0.0.2 still reports 127.0.0.1 as its source unless it asks otherwise.
        let access = Access::new(AllowFrom::parse("10.99.0.0/16").expect("rule"), &[]);
        let shutdown = Shutdown::new();
        let serving = std::thread::spawn({
            let (db, access, shutdown) = (db.clone(), Arc::clone(&access), shutdown.clone());
            move || {
                serve_supervised_until(
                    db,
                    &BindSpec::parse(&format!("{ONE}:{port}"), 4318).expect("spec"),
                    Limits::default(),
                    access,
                    None,
                    Duration::ZERO,
                    shutdown,
                )
            }
        });

        eventually("the listener to accept", || {
            reachable(&format!("{ONE}:{port}"))
        });

        // The connection is accepted by the kernel and then dropped, so a request gets no reply at
        // all — not a 403, which would confirm that something is listening.
        let mut stream = std::net::TcpStream::connect(format!("{ONE}:{port}"))
            .expect("the kernel accepts; the server is what refuses");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        let _ = stream.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        let mut reply = Vec::new();
        let _ = stream.read_to_end(&mut reply);
        assert!(
            reply.is_empty(),
            "a refused peer must get nothing back, got {:?}",
            String::from_utf8_lossy(&reply)
        );

        shutdown.trigger();
        serving.join().expect("thread").expect("clean stop");
        db.blocking().close().expect("close");
    }

    #[test]
    fn an_empty_specification_serves_nothing_and_stops_cleanly() {
        let dir = tempdir("supervisor-empty");
        let db = imbh::Db::builder(&dir).open().expect("open");
        let shutdown = Shutdown::new();
        let serving = std::thread::spawn({
            let (db, shutdown) = (db.clone(), shutdown.clone());
            move || {
                serve_supervised_until(
                    db,
                    &BindSpec::parse("", 4318).expect("spec"),
                    Limits::default(),
                    Access::unrestricted(),
                    None,
                    Duration::ZERO,
                    shutdown,
                )
            }
        });
        std::thread::sleep(Duration::from_millis(100));
        shutdown.trigger();
        serving.join().expect("thread").expect("clean stop");
        db.blocking().close().expect("close");
    }

    fn tempdir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("imbh-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }
}
