//! `attr-stats` — measure attribute cardinality and per-segment selectivity over an imbh database.
//!
//! The measurement itself lives in [`imbh_attrstats`], which is also what `imbhd` serves at
//! `POST /api/head/attributes/stats` and what the TUI's Attributes screen renders. This binary is the
//! command line over it: argument parsing, and a choice of the text report or the JSON document.
//!
//! ```text
//! cargo run -p attr-stats -- ./demo-db
//! cargo run -p attr-stats -- ./demo-db --scope attributes --top 20
//! cargo run -p attr-stats -- ./demo-db --json > attrs.json
//! ```
//!
//! It reads and changes nothing: no writer lock is taken, so it runs against a live database. Only
//! *sealed* segments are covered — see the crate docs for what that excludes and why.

use std::error::Error;
use std::path::PathBuf;

use imbh_attrstats::{AttrScope, Options, analyze, report, text};

const USAGE: &str = "\
attr-stats <db-dir> [options]

  --scope <all|attributes>  attribute scopes to read (default: all). `attributes` restricts the
                            scan to the record-attribute column — the only scope `promote` covers.
                            `all` also reads `resource:`/`scope:`-prefixed keys, which a segment
                            index could cover too.
  --last <minutes>          only consider segments overlapping the last N minutes (default: all)
  --windows <d,..>          window widths for the cardinality-vs-time-scale ladder, innermost
                            first, strictly increasing (default: 1m,1h,24h). Suffixes s/m/h/d.
                            `--windows none` skips the ladder and its per-value memory.
  --top <n>                 keys listed per table, by descending index cost (default: 25)
  --max-keys <n>            per-scan-unit key cap before hash sampling engages (default: 4096)
  --max-values <n>          per-key distinct-value cap before hash sampling engages (default: 50000)
  --batch-size <n>          Parquet read batch size (default: 8192)
  --json                    emit JSON instead of the text report
  -h, --help                this message
";

struct Config {
    dir: PathBuf,
    options: Options,
    top: usize,
    json: bool,
}

impl Config {
    fn from_args() -> Result<Option<Self>, Box<dyn Error>> {
        let mut args = std::env::args().skip(1);
        let mut dir: Option<PathBuf> = None;
        let mut options = Options::default();
        let mut top = 25;
        let mut json = false;
        while let Some(arg) = args.next() {
            let mut value = || -> Result<String, Box<dyn Error>> {
                args.next()
                    .ok_or_else(|| format!("{arg} needs a value").into())
            };
            match arg.as_str() {
                "-h" | "--help" => return Ok(None),
                "--scope" => {
                    options.scopes = match value()?.as_str() {
                        "all" => vec![AttrScope::Attributes, AttrScope::Resource, AttrScope::Scope],
                        "attributes" => vec![AttrScope::Attributes],
                        other => return Err(format!("unknown --scope {other}").into()),
                    }
                }
                "--last" => options = options.with_last_minutes(value()?.parse()?),
                "--windows" => options = options.with_window_spec(&value()?)?,
                "--top" => top = value()?.parse()?,
                "--max-keys" => options.max_keys = value()?.parse()?,
                "--max-values" => options.max_values = value()?.parse()?,
                "--batch-size" => options.batch_size = value()?.parse::<usize>()?.max(1),
                "--json" => json = true,
                other if other.starts_with('-') => {
                    return Err(format!("unknown option {other}").into());
                }
                other => dir = Some(PathBuf::from(other)),
            }
        }
        Ok(Some(Config {
            dir: dir.ok_or("missing <db-dir>")?,
            options,
            top,
            json,
        }))
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let Some(cfg) = Config::from_args()? else {
        print!("{USAGE}");
        return Ok(());
    };
    let report = analyze(&cfg.dir, &cfg.options)?;
    if cfg.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report::to_json(&report))?
        );
    } else {
        for line in text::render(&report, cfg.top) {
            println!("{line}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ladder spec and the scope flag are this binary's only real parsing, and both can fail in
    /// ways that must be refused rather than silently reinterpreted.
    #[test]
    fn the_default_options_match_the_usage_text() {
        let options = Options::default();
        let labels: Vec<&str> = options.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, vec!["1m", "1h", "24h"]);
        assert_eq!(options.max_keys, 4096);
        assert_eq!(options.max_values, 50_000);
        assert_eq!(options.batch_size, 8192);
        assert_eq!(options.scopes.len(), 3, "--scope all is the default");
        assert!(USAGE.contains("--windows"));
    }
}
