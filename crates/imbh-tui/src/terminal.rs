//! Terminal setup and teardown: raw mode, the alternate screen, the panic hook, and the blocking
//! input reader.

use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyEvent};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

/// Best-effort restore of the terminal to its pre-`enter` state, writing directly to `out`.
///
/// Canonical ordering: first show the cursor and leave the alternate screen, then disable raw
/// mode, so the normal screen and cursor are back before the mode flip. Every step is best-effort
/// (a failing step must not skip the others), which is what makes this safe to call from any of the
/// three teardown sites — the `enter()` error paths, `Drop`, and the panic hook — and idempotent,
/// so running it more than once (panic path *and* `Drop`) is harmless.
pub(crate) fn restore_terminal<W: io::Write>(out: &mut W) {
    let _ = execute!(out, Show, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

/// One-shot claim used by `install_panic_hook`: returns `true` exactly once for a given flag, so
/// repeated `run` calls don't stack duplicate panic hooks. Extracted so the idempotency is unit
/// testable without mutating the process-global panic hook.
pub(crate) fn claim_panic_hook_install(flag: &AtomicBool) -> bool {
    !flag.swap(true, Ordering::SeqCst)
}

pub(crate) static PANIC_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install a panic hook that restores the terminal *before* delegating to the previously-installed
/// hook, so a panic's message lands on the normal screen instead of staying hidden behind the
/// alternate screen. Installed at most once per process (guarded by `PANIC_HOOK_INSTALLED`) so
/// repeated `run` calls don't chain the hook onto itself.
///
/// The hook writes straight to `std::io::stdout()` rather than capturing a `Terminal`/stdout handle
/// a panic may have left in a poisoned state, and the restore is best-effort/idempotent so it does
/// not interfere with the normal `Drop`-based teardown on the non-panic path.
pub(crate) fn install_panic_hook() {
    if !claim_panic_hook_install(&PANIC_HOOK_INSTALLED) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Restore first (idempotent with `Drop`), then delegate so the message prints on the
        // now-restored normal screen.
        restore_terminal(&mut io::stdout());
        previous(info);
    }));
}

pub(crate) struct TerminalGuard {
    pub(crate) terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    pub(crate) fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, Hide) {
            // The two commands run in sequence, so a failure may leave the alternate screen active
            // and/or the cursor hidden (e.g. `EnterAlternateScreen` succeeded but `Hide` failed).
            // Best-effort restore everything before returning so we never strand the terminal in
            // the alternate screen or with a hidden cursor.
            restore_terminal(&mut stdout);
            return Err(error);
        }
        let terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                // `stdout` was moved into the backend above, so restore via a fresh handle.
                restore_terminal(&mut io::stdout());
                return Err(error);
            }
        };
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal(self.terminal.backend_mut());
        let _ = self.terminal.show_cursor();
    }
}

/// Blocking terminal-input reader, run on the blocking pool so it never stalls the runtime. Polls
/// with a timeout and re-checks `shutdown` each iteration so it exits promptly on quit rather than
/// parking forever in a blocking read.
pub(crate) fn input_reader(keys: &mpsc::UnboundedSender<KeyEvent>, shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::Relaxed) {
        match event::poll(Duration::from_millis(100)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => {
                    if keys.send(key).is_err() {
                        break; // the event loop dropped the receiver
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            },
            Ok(false) => {}
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_terminal_emits_leave_alt_screen_and_show() {
        // Guards gap #1: the restore path must always leave the alternate screen (and show the
        // cursor), never just disable raw mode. `disable_raw_mode` acts on the real terminal fd, not
        // our buffer, so the buffer captures exactly the `execute!(Show, LeaveAlternateScreen)`
        // output — compare it against the same command sequence rendered independently.
        let mut buf: Vec<u8> = Vec::new();
        restore_terminal(&mut buf);

        let mut expected: Vec<u8> = Vec::new();
        let _ = execute!(expected, Show, LeaveAlternateScreen);
        assert_eq!(buf, expected);
        // Sanity: something was actually written (the alt-screen leave is not a no-op).
        assert!(!buf.is_empty());
    }

    #[test]
    fn panic_hook_install_claim_is_idempotent() {
        // Guards gap #2's double-install guard without touching the process-global panic hook.
        let flag = AtomicBool::new(false);
        assert!(claim_panic_hook_install(&flag), "first claim wins");
        assert!(!claim_panic_hook_install(&flag), "second claim is refused");
        assert!(
            !claim_panic_hook_install(&flag),
            "further claims stay refused"
        );
    }
}
