use std::io::Write as _;

use imbh::Db;
use imbh_mcp::proxy::Endpoint;
use imbh_mcp::stdio::{self, Backend as McpBackend};
use imbh_tui::cli::{self, Mode, Source, USAGE};
use imbh_tui::{Backend, run};

/// `main` reports failures itself rather than returning them: `Result`'s exit path prints an error
/// with `Debug`, which would render the usage block as one line of `\n` escapes.
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let mode = match cli::parse(std::env::args_os().skip(1)) {
        Ok(mode) => mode,
        Err(message) => {
            note(&message);
            eprintln!("\n{USAGE}");
            // 2 for a wrong command line, as the shell convention has it; 1 is a failed run.
            return std::process::ExitCode::from(2);
        }
    };
    match dispatch(mode).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            note(&e.to_string());
            std::process::ExitCode::FAILURE
        }
    }
}

async fn dispatch(mode: Mode) -> Result<(), Box<dyn std::error::Error>> {
    match mode {
        Mode::Help => {
            println!("{USAGE}");
            Ok(())
        }
        Mode::Tui { source, options } => {
            let backend = match source {
                Source::Db(path) => Backend::open(path)?,
                Source::Url(url) => {
                    let backend = Backend::connect(&url)?;
                    // Said before the alternate screen swallows it: a head that cannot reach its
                    // daemon otherwise reports it only inside a panel, and the address it is
                    // actually using (port and scheme filled in) is the thing worth checking.
                    note(&format!("browsing {}", backend.describe()));
                    backend
                }
            };
            run(backend, options).await?;
            Ok(())
        }
        Mode::McpStdio(source) => serve_mcp(source).await,
    }
}

/// Serve MCP on stdin/stdout until the client closes the pipe.
///
/// Nothing but protocol messages may go to stdout — that is the transport — so the one line saying
/// what this session is reading goes to stderr, where a client collects it as the server's log.
async fn serve_mcp(source: Source) -> Result<(), Box<dyn std::error::Error>> {
    let backend = match source {
        Source::Db(path) => {
            // Read-only: no writer lock is taken, so this runs alongside a live `imbhd` on the same
            // directory. What it cannot see is that writer's unsealed buffer — for which `--url` is
            // the answer.
            let db = Db::open_read_only(&path)?;
            note(&format!("serving MCP over stdio from {}", path.display()));
            McpBackend::Local(db)
        }
        Source::Url(url) => {
            let endpoint = Endpoint::parse(&url)?;
            note(&format!("serving MCP over stdio via {}", endpoint.url()));
            McpBackend::Remote(endpoint)
        }
    };
    // The blocking std handles are deliberate: this loop is the only thing on the runtime, and
    // locking them for the whole session keeps another writer from interleaving into the framing.
    stdio::serve(&backend, std::io::stdin().lock(), std::io::stdout().lock()).await?;
    Ok(())
}

fn note(message: &str) {
    let _ = writeln!(std::io::stderr(), "imbh-tui: {message}");
}
