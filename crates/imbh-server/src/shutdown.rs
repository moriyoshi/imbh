//! Cooperative shutdown for the reference server (ARCHITECTURE.md §10.16).
//!
//! A collector process is asked to stop by a signal — `SIGTERM` from `docker stop`/systemd/`kill`,
//! `SIGINT` from Ctrl-C. The default disposition for both is "die now", which for `imbhd` means the
//! buffered rows that have not been sealed yet stay only in the WAL, so the next start pays a replay
//! and an operator watching `/stats` sees a segment count that never advanced. Nothing is *lost* (the
//! WAL is the durability contract), but a process that owns a flush scheduler should seal on the way
//! out rather than leave the work to recovery.
//!
//! So `imbhd` handles those two signals and winds down in order:
//!
//! 1. every accept loop stops accepting,
//! 2. in-flight requests get up to [`Shutdown::drain_timeout`] to finish,
//! 3. the Docker plugin's container readers stop and its ingest queue drains into the DB,
//! 4. `Db::close()` seals the buffer and joins the maintenance worker.
//!
//! A **second** signal skips all of that and exits immediately — the operator has stopped waiting.
//!
//! The pieces here are the [`Shutdown`] token every accept loop watches and the signal plumbing that
//! trips it. Two properties shape the implementation:
//!
//! - **No polling on the hot path.** A listener registers a wake-up with [`Shutdown::on_trigger`]
//!   rather than checking a flag on a timer; both accept loops turn that into a `oneshot` they select
//!   on, so an idle server costs nothing and shutdown is observed immediately. Draining what is
//!   already in flight is hyper's `GracefulShutdown`, not something this module counts.
//! - **The signal handler only does async-signal-safe work.** It stores an atomic and writes one byte
//!   to a self-pipe; a watcher thread parked on the read end does the rest (taking locks, notifying a
//!   condvar). Tripping the token from the handler itself would take a mutex inside a signal context.
//!
//! Footprint: signal handling needs `libc` (`std` has no way to catch `SIGTERM`), which is already in
//! `imbhd`'s dependency graph via DataFusion — so this adds **no crate** (ARCHITECTURE.md §11).

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// How long a listener waits for its in-flight requests to finish before returning anyway. Docker's
/// own `stop` grace is 10s and systemd's default `TimeoutStopSec` is 90s, so 5s leaves room for the
/// final seal that follows.
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// A one-shot listener wake-up, run when the token is tripped.
type Waker = Box<dyn FnOnce() + Send>;

/// The shutdown token: tripped once, watched by every accept loop, and the carrier of the drain
/// deadline they all share.
///
/// Always held as an `Arc` (see [`Shutdown::new`]) because each endpoint runs on its own thread.
/// Cheap to check — [`Shutdown::is_triggered`] is one atomic load, which is what an accept loop does
/// between connections.
pub struct Shutdown {
    triggered: AtomicBool,
    /// The signal number that tripped it, or 0 for a programmatic [`Shutdown::trigger`].
    cause: AtomicI32,
    /// Listener wake-ups, taken (not run) under the lock so a waker cannot re-enter the token. Also
    /// the mutex `changed` waits on, which is what makes [`Shutdown::wait`] free of lost wake-ups:
    /// `triggered` is only ever set while this lock is held.
    wakers: Mutex<Vec<Waker>>,
    changed: Condvar,
    drain_timeout: Duration,
}

impl Shutdown {
    /// A fresh token with the [`DEFAULT_DRAIN_TIMEOUT`].
    pub fn new() -> Arc<Shutdown> {
        Shutdown::with_drain_timeout(DEFAULT_DRAIN_TIMEOUT)
    }

    /// A fresh token whose listeners wait `drain_timeout` for in-flight requests at shutdown. Zero
    /// means "do not wait" — drop whatever is in flight and return.
    pub fn with_drain_timeout(drain_timeout: Duration) -> Arc<Shutdown> {
        Arc::new(Shutdown {
            triggered: AtomicBool::new(false),
            cause: AtomicI32::new(0),
            wakers: Mutex::new(Vec::new()),
            changed: Condvar::new(),
            drain_timeout,
        })
    }

    /// How long an accept loop waits for its in-flight requests once this token trips.
    pub fn drain_timeout(&self) -> Duration {
        self.drain_timeout
    }

    /// Whether shutdown has begun. What accept loops check between connections.
    pub fn is_triggered(&self) -> bool {
        self.triggered.load(Ordering::SeqCst)
    }

    /// Begin shutdown: wake every registered listener and release everyone in [`Shutdown::wait`].
    /// Idempotent — the second call does nothing.
    pub fn trigger(&self) {
        self.trip(0);
    }

    /// The signal that caused shutdown, if a signal did (`SIGTERM` → `Some(15)`). `None` while the
    /// token is untripped, and for a programmatic [`Shutdown::trigger`].
    pub fn cause(&self) -> Option<i32> {
        self.is_triggered()
            .then(|| self.cause.load(Ordering::SeqCst))
            .filter(|signum| *signum != 0)
    }

    /// Register a wake-up to run when the token trips — for both listeners, sending on the `oneshot`
    /// their accept loop selects on.
    ///
    /// Runs `waker` immediately if the token is already tripped, so a listener that registers late
    /// (bound after the signal arrived) still winds down instead of parking in `accept` forever.
    pub fn on_trigger(&self, waker: impl FnOnce() + Send + 'static) {
        let mut wakers = self.lock();
        if self.is_triggered() {
            drop(wakers);
            waker();
            return;
        }
        wakers.push(Box::new(waker));
    }

    /// Park until the token trips, then report the signal that did it (`None` for a programmatic
    /// trigger). This is where `imbhd`'s `main` spends the life of the process.
    pub fn wait(&self) -> Option<i32> {
        let mut guard = self.lock();
        while !self.is_triggered() {
            guard = self
                .changed
                .wait(guard)
                .unwrap_or_else(PoisonError::into_inner);
        }
        drop(guard);
        self.cause()
    }

    /// Trip the token with a cause, run the wakers, and release the waiters.
    fn trip(&self, signum: i32) {
        let wakers = {
            let mut wakers = self.lock();
            // Under the lock: `wait` checks `triggered` while holding it, so it cannot miss this.
            if self.triggered.swap(true, Ordering::SeqCst) {
                return;
            }
            self.cause.store(signum, Ordering::SeqCst);
            std::mem::take(&mut *wakers)
        };
        // Wake the listeners *before* releasing the waiters, so by the time `main` is running its
        // shutdown sequence every accept loop is already on its way out — not still parked in
        // `accept` while the drain deadline it is being measured against ticks down. Outside the lock:
        // a waker connects to a socket, and a listener may re-enter the token.
        for wake in wakers {
            wake();
        }
        self.changed.notify_all();
    }

    /// A poisoned lock carries no invariant here (the guarded `Vec` is only ever taken whole), so
    /// recover rather than propagate a panic into every listener.
    fn lock(&self) -> MutexGuard<'_, Vec<Waker>> {
        self.wakers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Trip this token when the process is asked to stop: `SIGINT` (Ctrl-C) or `SIGTERM`
    /// (`docker stop`, systemd, `kill`). A **second** such signal exits the process immediately with
    /// `128 + signum`, skipping the graceful path — the standard escape hatch for an operator who is
    /// done waiting.
    ///
    /// Call once per process: the handlers are process-global, so a second call reports
    /// [`std::io::ErrorKind::AlreadyExists`] rather than silently leaving the earlier token as the
    /// only one that trips. Costs one blocked thread and no polling.
    ///
    /// Non-Unix targets have no `SIGTERM` to catch; there this reports
    /// [`std::io::ErrorKind::Unsupported`] and the caller keeps whatever shutdown path it drives
    /// itself (`imbhd` warns and serves on).
    #[cfg(unix)]
    pub fn install_signal_handlers(self: &Arc<Self>) -> std::io::Result<()> {
        use std::io::{Error, ErrorKind};

        if INSTALLED.swap(true, Ordering::SeqCst) {
            return Err(Error::new(
                ErrorKind::AlreadyExists,
                "signal handlers are already installed for this process",
            ));
        }

        // The self-pipe the handler writes to and the watcher thread reads. Created before the
        // handlers so a signal arriving during installation has somewhere to go.
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` is a two-element array, exactly what `pipe(2)` writes into.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(Error::last_os_error());
        }
        let [read_fd, write_fd] = fds;
        SIGNAL_PIPE_WRITE.store(write_fd, Ordering::SeqCst);

        for signum in [libc::SIGINT, libc::SIGTERM] {
            // SAFETY: `sigaction` is a plain C struct; an all-zero value is a valid starting point
            // (no flags, empty mask) and every field we care about is set below.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = handle_signal as *const () as libc::sighandler_t;
            // SA_RESTART so an arriving signal does not surface as `EINTR` in the middle of the FIFO
            // readers' or the storage engine's blocking I/O — the shutdown is announced through the
            // pipe, never through an interrupted syscall.
            action.sa_flags = libc::SA_RESTART;
            // SAFETY: `action` is initialized above; `sigemptyset`/`sigaction` only read through the
            // pointers we pass, and the null third argument means "do not report the old handler".
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signum, &action, std::ptr::null_mut()) != 0 {
                    return Err(Error::last_os_error());
                }
            }
        }

        // The watcher: parked in a `read` on the pipe, so it wakes the instant a signal arrives and
        // costs nothing until then. It does the work the handler must not — take locks, notify.
        let shutdown = Arc::clone(self);
        std::thread::Builder::new()
            .name("imbh-signal".to_owned())
            .spawn(move || {
                let mut byte = 0u8;
                loop {
                    // SAFETY: a one-byte read into a one-byte stack buffer on our own pipe fd.
                    let n = unsafe { libc::read(read_fd, (&raw mut byte).cast(), 1) };
                    if n == 1 {
                        break;
                    }
                    // EINTR: another signal landed on this thread mid-read; look again. Anything else
                    // means the pipe is unusable and there is nothing left to watch.
                    if n < 0 && Error::last_os_error().kind() == ErrorKind::Interrupted {
                        continue;
                    }
                    return;
                }
                shutdown.trip(SIGNAL_RECEIVED.load(Ordering::SeqCst));
            })?;
        Ok(())
    }

    /// No-op stand-in on targets without POSIX signals; see the Unix version for the contract.
    #[cfg(not(unix))]
    pub fn install_signal_handlers(self: &Arc<Self>) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "signal-driven shutdown is only implemented for Unix targets",
        ))
    }
}

/// Whether [`Shutdown::install_signal_handlers`] has already claimed this process's handlers.
#[cfg(unix)]
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Write end of the self-pipe, for the handler. `-1` until installed.
#[cfg(unix)]
static SIGNAL_PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);

/// The signal that arrived: written by the handler, read by the watcher thread for the exit report.
#[cfg(unix)]
static SIGNAL_RECEIVED: AtomicI32 = AtomicI32::new(0);

/// The signal handler. **Async-signal-safe only**: atomics, `write(2)`, `_exit(2)`. No allocation,
/// no locks, no formatting — a mutex taken here could deadlock against the thread it interrupted.
#[cfg(unix)]
extern "C" fn handle_signal(signum: libc::c_int) {
    if SIGNAL_RECEIVED.swap(signum, Ordering::SeqCst) != 0 {
        // A second signal: the operator is done waiting for the graceful path. `_exit` skips atexit
        // handlers and destructors, which is the point — whatever is stuck stays stuck.
        // SAFETY: `_exit` is async-signal-safe and does not return.
        unsafe { libc::_exit(128 + signum) };
    }
    let fd = SIGNAL_PIPE_WRITE.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte = 1u8;
        // SAFETY: a one-byte write from a live stack buffer to our own pipe fd. A failure (a full or
        // closed pipe) is ignored: the watcher is either already awake or gone.
        let _ = unsafe { libc::write(fd, (&raw const byte).cast(), 1) };
    }
}

/// The name of a shutdown signal, for the exit report: `15` → `"SIGTERM"`. Anything other than the two
/// signals [`Shutdown::install_signal_handlers`] catches reads as a bare `"signal"`, since only those
/// two ever reach [`Shutdown::cause`].
pub fn signal_name(signum: i32) -> &'static str {
    #[cfg(unix)]
    match signum {
        libc::SIGINT => return "SIGINT",
        libc::SIGTERM => return "SIGTERM",
        _ => {}
    }
    let _ = signum;
    "signal"
}

#[cfg(test)]
mod tests {
    use super::*;
    // Not in the module's own imports: only the wake-up counter in the first test needs it.
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn a_token_starts_untripped_and_trips_once() {
        let shutdown = Shutdown::new();
        assert!(!shutdown.is_triggered());
        assert_eq!(shutdown.cause(), None);
        assert_eq!(shutdown.drain_timeout(), DEFAULT_DRAIN_TIMEOUT);

        let woken = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let woken = Arc::clone(&woken);
            shutdown.on_trigger(move || {
                woken.fetch_add(1, Ordering::SeqCst);
            });
        }

        shutdown.trigger();
        assert!(shutdown.is_triggered());
        assert_eq!(woken.load(Ordering::SeqCst), 2, "every listener is woken");
        // A programmatic trigger has no signal behind it, and re-triggering must not run the wakers
        // a second time (a listener has already stopped by then; a second connect could hang).
        assert_eq!(shutdown.cause(), None);
        shutdown.trigger();
        assert_eq!(woken.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_late_waker_runs_immediately() {
        // A listener that binds after the signal arrived would otherwise park in `accept` forever.
        let shutdown = Shutdown::new();
        shutdown.trigger();
        let woken = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&woken);
        shutdown.on_trigger(move || flag.store(true, Ordering::SeqCst));
        assert!(woken.load(Ordering::SeqCst));
    }

    #[test]
    fn wait_returns_when_another_thread_triggers() {
        let shutdown = Shutdown::new();
        let trigger = Arc::clone(&shutdown);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            trigger.trigger();
        });
        // No timeout in the test: `wait` blocks on the condvar, so this hangs if the notification is
        // ever lost.
        assert_eq!(shutdown.wait(), None);
        assert!(shutdown.is_triggered());
        // Waiting on an already-tripped token returns at once.
        assert_eq!(shutdown.wait(), None);
    }

    /// The signal path end to end: install the handlers, raise `SIGTERM` at ourselves, and require
    /// that it becomes a tripped token with the right cause instead of killing the process.
    ///
    /// This is the **only** test in this binary that raises a signal, deliberately: the handlers are
    /// process-global and a *second* signal is defined to `_exit`.
    #[cfg(unix)]
    #[test]
    fn a_signal_trips_the_token_instead_of_killing_the_process() {
        let shutdown = Shutdown::with_drain_timeout(Duration::from_millis(500));
        shutdown
            .install_signal_handlers()
            .expect("install signal handlers");
        // Process-global: a second install must say so rather than leave a token that never trips.
        assert_eq!(
            Shutdown::new()
                .install_signal_handlers()
                .expect_err("a second install is refused")
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );

        // SAFETY: `raise` sends the signal to this process; the handler installed above is what
        // receives it, so the default "terminate" disposition is no longer in play.
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);

        assert_eq!(shutdown.wait(), Some(libc::SIGTERM));
        assert!(shutdown.is_triggered());
        assert_eq!(shutdown.drain_timeout(), Duration::from_millis(500));
    }
}
