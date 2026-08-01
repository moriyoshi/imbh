//! The `imbh-tui` binary's command line.
//!
//! The binary is two programs sharing one database opener: the terminal explorer, and — with
//! `--mcp-stdio` — an MCP server on stdin/stdout ([`imbh_mcp::stdio`]). They live in the same
//! binary because they are the same thing from two directions: a read-only view of a database
//! someone else is writing, for a person and for an agent respectively.
//!
//! Parsing lives here rather than in `main.rs` so the combinations that must be refused (a `--url`
//! with no `--mcp-stdio`, two sources at once, a TUI flag in server mode) are covered by tests.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::{Options, parse_datetime};

pub const USAGE: &str = "\
usage:
  imbh-tui <database-directory> [--ascii] [--refresh-seconds N]
           [--from 'YYYY-MM-DD HH:MM:SS' --to 'YYYY-MM-DD HH:MM:SS']
      Browse a database in the terminal.

  imbh-tui --mcp-stdio (<database-directory> | --url http://host:port)
      Serve the Model Context Protocol on stdin/stdout, either from the database
      directory directly (read-only, alongside a running imbhd) or by forwarding to
      a running imbhd's POST /mcp.";

/// What the binary was asked to do.
#[derive(Debug)]
pub enum Mode {
    /// Browse a database in the terminal.
    Tui { path: PathBuf, options: Options },
    /// Serve MCP over stdio.
    McpStdio(Source),
    /// Print [`USAGE`] and exit successfully.
    Help,
}

/// Where an MCP stdio session gets its answers.
#[derive(Debug)]
pub enum Source {
    /// Open this directory read-only in-process. `Db::open_read_only` takes no writer lock, so this
    /// reads alongside a running `imbhd` — it just cannot see what is still in that writer's buffer.
    Db(PathBuf),
    /// Forward to a running `imbhd`'s MCP endpoint, which can.
    Url(String),
}

/// Parse the argument list (without `argv[0]`).
///
/// The `Err` is the message to print; it never carries the usage block, which the caller appends, so
/// a wrong flag reads as one line followed by the two forms rather than as a wall of text.
pub fn parse<I: IntoIterator<Item = OsString>>(args: I) -> Result<Mode, String> {
    let mut arguments = args.into_iter();
    let mut options = Options::default();
    let (mut from, mut to) = (None, None);
    let mut mcp_stdio = false;
    let mut url: Option<String> = None;
    let mut path: Option<PathBuf> = None;
    // Which TUI-only flags were given, so server mode can name the offending one.
    let mut tui_flags: Vec<&'static str> = Vec::new();

    let next = |arguments: &mut I::IntoIter, flag: &str, what: &str| {
        arguments
            .next()
            .ok_or_else(|| format!("{flag} requires {what}"))
    };

    while let Some(argument) = arguments.next() {
        let text = argument.to_string_lossy().into_owned();
        match text.as_str() {
            "--help" | "-h" => return Ok(Mode::Help),
            "--mcp-stdio" => mcp_stdio = true,
            "--url" => {
                let value = next(&mut arguments, "--url", "an address, e.g. 127.0.0.1:4318")?;
                url = Some(value.to_string_lossy().into_owned());
            }
            // An explicit spelling of the positional, so an MCP client's config file can list flags
            // in any order without a bare path floating among them.
            "--db" => {
                let value = next(&mut arguments, "--db", "a database directory")?;
                set_path(&mut path, PathBuf::from(value))?;
            }
            "--ascii" => {
                tui_flags.push("--ascii");
                options.ascii = true;
            }
            "--refresh-seconds" => {
                tui_flags.push("--refresh-seconds");
                let value = next(&mut arguments, "--refresh-seconds", "an integer")?;
                let seconds = value
                    .to_string_lossy()
                    .parse::<u64>()
                    .map_err(|_| "--refresh-seconds requires an integer".to_owned())?;
                options.refresh_interval = Duration::from_secs(seconds.max(1));
            }
            "--from" => {
                tui_flags.push("--from");
                let value = next(&mut arguments, "--from", "a UTC datetime")?;
                from = Some(
                    parse_datetime(&value.to_string_lossy())
                        .ok_or("--from: expected UTC 'YYYY-MM-DD HH:MM:SS'")?,
                );
            }
            "--to" => {
                tui_flags.push("--to");
                let value = next(&mut arguments, "--to", "a UTC datetime")?;
                to = Some(
                    parse_datetime(&value.to_string_lossy())
                        .ok_or("--to: expected UTC 'YYYY-MM-DD HH:MM:SS'")?,
                );
            }
            flag if flag.starts_with('-') => return Err(format!("unknown argument: {flag}")),
            _ => set_path(&mut path, PathBuf::from(argument))?,
        }
    }

    if mcp_stdio {
        if let Some(flag) = tui_flags.first() {
            return Err(format!(
                "{flag} is a terminal-explorer option and means nothing with --mcp-stdio"
            ));
        }
        return match (path, url) {
            (Some(path), None) => Ok(Mode::McpStdio(Source::Db(path))),
            (None, Some(url)) => Ok(Mode::McpStdio(Source::Url(url))),
            (Some(_), Some(_)) => Err(
                "--mcp-stdio takes a database directory or --url, not both: one opens the data \
                 here, the other asks a running imbhd for it"
                    .to_owned(),
            ),
            (None, None) => Err(
                "--mcp-stdio needs a database directory or --url naming a running imbhd".to_owned(),
            ),
        };
    }

    if url.is_some() {
        return Err(
            "--url is only valid with --mcp-stdio; the terminal explorer reads a database \
             directory directly"
                .to_owned(),
        );
    }
    let path = path.ok_or("a database directory is required")?;
    // An absolute launch window needs both bounds, ordered.
    options.window = match (from, to) {
        (Some(start), Some(end)) if start < end => Some((start, end)),
        (Some(_), Some(_)) => return Err("--from must be before --to".to_owned()),
        (None, None) => None,
        _ => return Err("--from and --to must be given together".to_owned()),
    };
    Ok(Mode::Tui { path, options })
}

fn set_path(path: &mut Option<PathBuf>, value: PathBuf) -> Result<(), String> {
    if let Some(first) = path {
        return Err(format!(
            "two database directories given ({} and {})",
            first.display(),
            value.display()
        ));
    }
    *path = Some(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Mode, String> {
        parse(args.iter().map(OsString::from))
    }

    fn tui(args: &[&str]) -> (PathBuf, Options) {
        match parse_args(args).expect("parses") {
            Mode::Tui { path, options } => (path, options),
            _ => panic!("expected the terminal explorer"),
        }
    }

    fn source(args: &[&str]) -> Source {
        match parse_args(args).expect("parses") {
            Mode::McpStdio(source) => source,
            _ => panic!("expected an MCP stdio server"),
        }
    }

    #[test]
    fn the_explorer_keeps_its_original_command_line() {
        let (path, options) = tui(&["./data", "--ascii", "--refresh-seconds", "9"]);
        assert_eq!(path, PathBuf::from("./data"));
        assert!(options.ascii);
        assert_eq!(options.refresh_interval, Duration::from_secs(9));
        assert!(options.window.is_none());

        // A zero refresh interval floors to one second rather than spinning.
        let (_, options) = tui(&["./data", "--refresh-seconds", "0"]);
        assert_eq!(options.refresh_interval, Duration::from_secs(1));

        let (_, options) = tui(&[
            "./data",
            "--from",
            "2026-01-01 00:00:00",
            "--to",
            "2026-01-02 00:00:00",
        ]);
        let (start, end) = options.window.expect("an absolute window");
        assert!(start < end);
    }

    #[test]
    fn the_explorer_refuses_what_it_cannot_act_on() {
        assert!(parse_args(&[]).unwrap_err().contains("required"));
        assert!(
            parse_args(&["a", "b"])
                .unwrap_err()
                .contains("two database")
        );
        assert!(
            parse_args(&["./data", "--from", "2026-01-01 00:00:00"])
                .unwrap_err()
                .contains("must be given together")
        );
        assert!(
            parse_args(&[
                "./data",
                "--from",
                "2026-01-02 00:00:00",
                "--to",
                "2026-01-01 00:00:00"
            ])
            .unwrap_err()
            .contains("must be before")
        );
        assert!(
            parse_args(&["./data", "--nope"])
                .unwrap_err()
                .contains("unknown")
        );
        assert!(parse_args(&["./data", "--refresh-seconds"]).is_err());
    }

    #[test]
    fn mcp_stdio_takes_exactly_one_source() {
        assert!(matches!(
            source(&["--mcp-stdio", "./data"]),
            Source::Db(path) if path == *"./data"
        ));
        // `--db` is the same thing spelled as a flag, and order does not matter.
        assert!(matches!(
            source(&["--db", "./data", "--mcp-stdio"]),
            Source::Db(path) if path == *"./data"
        ));
        assert!(matches!(
            source(&["--mcp-stdio", "--url", "127.0.0.1:4318"]),
            Source::Url(url) if url == "127.0.0.1:4318"
        ));

        assert!(
            parse_args(&["--mcp-stdio"])
                .unwrap_err()
                .contains("needs a database directory or --url")
        );
        assert!(
            parse_args(&["--mcp-stdio", "./data", "--url", "127.0.0.1:4318"])
                .unwrap_err()
                .contains("not both")
        );
    }

    #[test]
    fn the_two_modes_do_not_borrow_each_others_flags() {
        // `--url` has no meaning for the explorer: it opens files, it does not speak HTTP.
        assert!(
            parse_args(&["--url", "127.0.0.1:4318"])
                .unwrap_err()
                .contains("only valid with --mcp-stdio")
        );
        // ...and a display option has none for a server that draws nothing.
        assert!(
            parse_args(&["--mcp-stdio", "./data", "--ascii"])
                .unwrap_err()
                .contains("means nothing with --mcp-stdio")
        );
    }

    #[test]
    fn help_is_a_mode_rather_than_an_error() {
        assert!(matches!(
            parse_args(&["--help"]).expect("parses"),
            Mode::Help
        ));
        assert!(matches!(parse_args(&["-h"]).expect("parses"), Mode::Help));
        // Even alongside arguments that would otherwise be refused.
        assert!(matches!(
            parse_args(&["--url", "x", "--help"]).expect("parses"),
            Mode::Help
        ));
        assert!(USAGE.contains("--mcp-stdio"));
    }
}
