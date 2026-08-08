//! `imbh-housekeeper` — segment housekeeping for an **embedded** imbh database, from a separate
//! process, while the host keeps writing.
//!
//! Design: ARCHITECTURE.md §7.2.
//!
//! ## Why this exists
//!
//! Compaction is CPU- and IO-heavy, unbounded in duration, and an embedded application does not want
//! it competing with its own work inside its own process. The obvious answer — run it elsewhere — is
//! blocked by the single-writer invariant (ARCHITECTURE.md §5): `writer.lock` is an exclusive advisory
//! lock held for the lifetime of a read-write handle, so a second process cannot open the directory
//! for writing while the host is running. And an embedded host has no `imbhd`, so there is no admin
//! endpoint to drive either.
//!
//! The way through is to notice that segment rewriting is ~99% expensive IO and ~1% atomic
//! bookkeeping, and that the two halves need completely different guarantees:
//!
//! - **This process prepares.** It opens the database **read-only** — no lock, and readers already
//!   work correctly against a live writer — reads the segments it wants to rewrite, projects any
//!   promoted columns they predate, sorts, writes one Parquet plus its `.tidx`, and leaves a record
//!   under `<db>/pending/`. It never touches the manifest and never deletes anything.
//! - **The host commits.** `Db::maintain()` picks the records up, validates each, and performs the
//!   swap the writer alone may do: one manifest delta, then unlink the inputs. A host that never
//!   calls `maintain()` can call `Db::commit_pending()` directly.
//!
//! Nothing here can corrupt the database. The worst outcome of racing the writer is a record the
//! host later discards, costing this process's work and nothing else.
//!
//! ## Usage
//!
//! ```text
//! imbh-housekeeper <db-dir> [options]
//!
//!   --max-jobs <n>     rewrites to prepare per pass (default 4)
//!   --every <seconds>  run continuously at this interval instead of once
//!   --commit           take the writer lock and commit the records too — only valid when no
//!                      writer is running; fails fast if one holds the lock
//!   --json             machine-readable output
//! ```
//!
//! `--commit` is the offline mode, and it falls out of the same design rather than being a second
//! implementation: with no writer running, this process can be the writer for as long as the swap
//! takes. It runs `maintain()`, so it also applies the database's **own** retention policy — durable
//! state, not flags invented here, so this process cannot disagree with the host about when data is
//! deleted.
//!
//! **Retention is deliberately not a handoff job.** Its scan is segment metadata plus one `stat()`
//! per segment, so there is no expensive half to move off-process — the writer can compute the drop
//! list faster than it could read a record describing one. A "drop A" record racing a
//! "merge A,B -> C" record would also introduce a conflict the single-record design does not have,
//! and the deliberate `commit_pending()`-before-`retain()` ordering in `maintain()` would dissolve
//! into whatever order the records happened to be listed in.

use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use imbh::{Compression, Db, MaintenanceReport, PendingRewrite, prepare_pending};

const USAGE: &str = "\
imbh-housekeeper <db-dir> [options]

  --max-jobs <n>     rewrites to prepare per pass (default 4). Bounds the burst after a
                     `set_promote`, which makes every segment lacking the new column eligible.
  --every <seconds>  run continuously at this interval instead of a single pass
  --commit           also commit the prepared records, taking the writer lock. ONLY valid when no
                     writer is running; fails fast if one holds the lock.
  --json             emit JSON instead of text
  -h, --help         this message
";

struct Config {
    dir: PathBuf,
    max_jobs: usize,
    every: Option<Duration>,
    commit: bool,
    json: bool,
}

fn parse_args() -> Result<Option<Config>, Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let mut dir: Option<PathBuf> = None;
    let (mut max_jobs, mut every, mut commit, mut json) = (4usize, None, false, false);
    while let Some(arg) = args.next() {
        let mut value = || -> Result<String, Box<dyn Error>> {
            args.next()
                .ok_or_else(|| format!("{arg} needs a value").into())
        };
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--max-jobs" => max_jobs = value()?.parse::<usize>()?.max(1),
            "--every" => every = Some(Duration::from_secs(value()?.parse()?)),
            "--commit" => commit = true,
            "--json" => json = true,
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other}").into());
            }
            other => dir = Some(PathBuf::from(other)),
        }
    }
    Ok(Some(Config {
        dir: dir.ok_or("missing <db-dir>")?,
        max_jobs,
        every,
        commit,
        json,
    }))
}

fn main() -> Result<(), Box<dyn Error>> {
    let Some(cfg) = parse_args()? else {
        print!("{USAGE}");
        return Ok(());
    };
    if !cfg.dir.is_dir() {
        return Err(format!("{} is not a directory", cfg.dir.display()).into());
    }
    loop {
        pass(&cfg)?;
        let Some(interval) = cfg.every else {
            return Ok(());
        };
        std::thread::sleep(interval);
    }
}

fn pass(cfg: &Config) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    // The compression a prepared segment is written with. It affects only this output's bytes, not
    // how anything else is read, so a housekeeper disagreeing with the host costs a size difference
    // and nothing more.
    let prepared = prepare_pending(&cfg.dir, Compression::default(), cfg.max_jobs)?;
    let elapsed = started.elapsed();

    let committed: Option<MaintenanceReport> = if cfg.commit {
        // Taking the writer lock here is what makes this the *offline* mode. If the host is running,
        // this fails — deliberately, and before anything is changed.
        let db = Db::builder(&cfg.dir)
            .open()
            .map_err(|e| format!("--commit needs exclusive access; is a writer running? ({e})"))?;
        // `maintain()` rather than `commit_pending()`: it seals, commits the records, and then
        // applies **the host's own retention policy**, which is durable database state rather than
        // something this process invents from its own flags. Retention deliberately has no handoff
        // record — its scan is metadata plus one `stat()` per segment, so there is no expensive half
        // to move off-process, and a "drop A" record racing a "merge A,B -> C" record would create a
        // conflict the single-record design does not have.
        let maint = db.blocking().maintain()?;
        db.blocking().close()?;
        Some(maint)
    } else {
        None
    };

    if cfg.json {
        let jobs: Vec<String> = prepared.iter().map(describe_json).collect();
        let committed_json = committed
            .as_ref()
            .map(|r| {
                format!(
                    "{{\"applied\":{},\"discarded\":{},\"segments_replaced\":{},\
                     \"retention_dropped\":{},\"retention_bytes_freed\":{}}}",
                    r.pending_applied,
                    r.pending_discarded,
                    r.pending_segments_replaced,
                    r.segments_dropped,
                    r.bytes_freed
                )
            })
            .unwrap_or_else(|| "null".to_owned());
        println!(
            "{{\"prepared\":[{}],\"committed\":{},\"elapsed_ms\":{}}}",
            jobs.join(","),
            committed_json,
            elapsed.as_millis()
        );
        return Ok(());
    }

    if prepared.is_empty() {
        println!(
            "nothing to do ({} scanned in {elapsed:.1?})",
            cfg.dir.display()
        );
    } else {
        println!("prepared {} rewrite(s) in {elapsed:.1?}:", prepared.len());
        for rec in &prepared {
            println!("  {}", describe(rec));
        }
    }
    match committed {
        Some(r) => {
            println!(
                "committed: {} applied, {} discarded, {} input segment(s) replaced",
                r.pending_applied, r.pending_discarded, r.pending_segments_replaced
            );
            if r.segments_dropped > 0 {
                println!(
                    "retention: {} segment(s) dropped, {} bytes freed (the database's own policy)",
                    r.segments_dropped, r.bytes_freed
                );
            }
        }
        None => {
            if !prepared.is_empty() {
                println!(
                    "  left under {}/pending/ — the writer applies them on its next maintain()",
                    cfg.dir.display()
                );
            }
        }
    }
    Ok(())
}

/// A rewrite's shape at a glance. `1 -> 1` is a **convergence** (a lone segment whose schema lagged
/// the promoted key set); `N -> 1` is a merge. They are the same job, so they read the same way.
fn describe(rec: &PendingRewrite) -> String {
    let kind = if rec.inputs.len() == 1 {
        "converge"
    } else {
        "merge"
    };
    format!(
        "{kind:<8} {:<22} {} -> 1  ({} rows)",
        rec.table.as_str(),
        rec.inputs.len(),
        rec.output.rows
    )
}

fn describe_json(rec: &PendingRewrite) -> String {
    format!(
        "{{\"kind\":\"{}\",\"table\":\"{}\",\"inputs\":{},\"rows\":{}}}",
        if rec.inputs.len() == 1 {
            "converge"
        } else {
            "merge"
        },
        rec.table.as_str(),
        rec.inputs.len(),
        rec.output.rows
    )
}
