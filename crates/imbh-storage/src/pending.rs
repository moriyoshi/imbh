//! Pending housekeeping records: the handoff between a **prepare** step that may run out of process
//! and a **commit** step that only the writer performs.
//!
//! Design: ARCHITECTURE.md §7.2. The short version — segment rewriting is ~99%
//! expensive IO (read N segments, project promoted columns, concat, sort, write Parquet + `.tidx`)
//! and ~1% atomic bookkeeping (one manifest delta, then unlink the inputs). The two halves have
//! completely different safety requirements, so they are split along that line rather than by
//! splitting `writer.lock`:
//!
//! - **Prepare** needs only *read* access. It opens the database read-only (no lock — readers
//!   already work correctly against a live writer), does the rewrite into a scratch-named file, and
//!   drops a record here. It never touches the manifest and never deletes anything.
//! - **Commit** needs the writer lock, which the writer already holds. It validates the record,
//!   appends one manifest delta, unlinks the inputs, and removes the record.
//!
//! Because the manifest stays single-mutator, none of the hard parts of concurrent writers apply:
//! no commit protocol, no checkpoint-vs-delta race, and no stale in-RAM segment view on the writer
//! (it is the one making the change).
//!
//! ## On-disk shape
//!
//! One file per record under `<db>/pending/`, framed exactly like a manifest edit —
//! `| len(4) | xxh3(8) | payload |` — so a record torn by a crash mid-write fails its checksum and
//! is discarded rather than half-applied. The payload is tab-separated text so a stuck record can be
//! read by a human; keys may repeat (`input`, `promote`) and order is significant for `promote`.
//!
//! **Discarding is always safe.** The inputs are untouched until commit, so rejecting a record costs
//! only the housekeeper's wasted work — never data.

use std::path::{Path, PathBuf};

use imbh_core::{Error, Promote, Result, SegmentRef, Table};

/// Directory holding pending records, relative to the DB directory.
pub(crate) const PENDING_DIR: &str = "pending";
/// Fixed frame header: len(4) + xxh3(8). Same shape as a manifest frame.
const HEADER: usize = 4 + 8;
/// Record format version. A record whose version this build does not know is discarded, not guessed
/// at — the output it names is scratch, so discarding loses nothing but effort.
const VERSION: u32 = 1;

/// What a prepared rewrite proposes: replace `inputs` with `output`, in one table.
///
/// A **merge** (`inputs.len() > 1`) and a **convergence** (`inputs.len() == 1`, rewritten because its
/// schema lagged the promote set) are the same record shape, because they are the same job with
/// different triggers — see the design note §5. Splitting them would let two records claim the same
/// input segment, where whichever committed first would invalidate the other.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingRewrite {
    pub table: Table,
    /// Segment paths (DB-relative) this rewrite consumes. Must all still be in the manifest at
    /// commit time, or the record is stale.
    pub inputs: Vec<String>,
    /// The rewritten segment (DB-relative), already on disk.
    ///
    /// It is written under the ordinary segment naming scheme and left **unreferenced** by the
    /// manifest until commit — the shape of an orphan. `cleanup_orphans` therefore treats a file
    /// named by a *valid* pending record as referenced, so a preparer does not lose its work to
    /// every writer restart. Once the record is gone (committed or discarded) the file is an orphan
    /// again and is swept on the next open.
    pub output: SegmentRef,
    /// xxh3-64 of the output file as written, so a truncated or partially-synced output is caught
    /// before it is committed into the manifest.
    pub output_digest: u64,
    /// The promoted key set the output was built against. If the writer has since changed it, the
    /// output's column layout no longer matches what the manifest would imply, so the record is
    /// discarded and the housekeeper can redo it.
    pub promote: Promote,
}

/// Only the *write* half needs this; a commit-only build never encodes a record.
#[cfg(feature = "compaction")]
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

impl PendingRewrite {
    #[cfg(feature = "compaction")]
    fn encode(&self) -> Vec<u8> {
        let mut s = String::new();
        s.push_str(&format!("version\t{VERSION}\n"));
        s.push_str(&format!("table\t{}\n", self.table.as_str()));
        for k in self.promote.keys() {
            s.push_str(&format!("promote\t{}\n", escape(k)));
        }
        for i in &self.inputs {
            s.push_str(&format!("input\t{}\n", escape(i)));
        }
        s.push_str(&format!("output\t{}\n", escape(&self.output.relative_path)));
        s.push_str(&format!("rows\t{}\n", self.output.rows));
        s.push_str(&format!("min_time\t{}\n", self.output.min_time_unix_nano));
        s.push_str(&format!("max_time\t{}\n", self.output.max_time_unix_nano));
        s.push_str(&format!("digest\t{:016x}\n", self.output_digest));
        s.into_bytes()
    }

    fn decode(payload: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(payload).ok()?;
        let (mut table, mut output, mut digest) = (None, None, None);
        let (mut rows, mut min_time, mut max_time) = (None, None, None);
        let (mut inputs, mut promote) = (Vec::new(), Vec::new());
        let mut version = None;
        for line in text.lines() {
            let mut it = line.splitn(2, '\t');
            let (k, v) = (it.next()?, it.next().unwrap_or_default());
            match k {
                "version" => version = v.parse::<u32>().ok(),
                "table" => table = Table::ALL.iter().copied().find(|t| t.as_str() == v),
                "promote" => promote.push(unescape(v)),
                "input" => inputs.push(unescape(v)),
                "output" => output = Some(unescape(v)),
                "rows" => rows = v.parse().ok(),
                "min_time" => min_time = v.parse().ok(),
                "max_time" => max_time = v.parse().ok(),
                "digest" => digest = u64::from_str_radix(v, 16).ok(),
                // Unknown keys are ignored so a newer writer's extra fields do not make a record
                // unreadable; the version check below is what actually gates compatibility.
                _ => {}
            }
        }
        if version? != VERSION || inputs.is_empty() {
            return None;
        }
        Some(PendingRewrite {
            table: table?,
            inputs,
            output: SegmentRef {
                relative_path: output?,
                min_time_unix_nano: min_time?,
                max_time_unix_nano: max_time?,
                rows: rows?,
            },
            output_digest: digest?,
            promote: Promote::new(promote),
        })
    }
}

/// xxh3-64 of a file's bytes, for the output-integrity check at commit.
pub(crate) fn digest_file(path: &Path) -> Result<u64> {
    let bytes = std::fs::read(path).map_err(|e| Error::storage_io(Some(path.to_path_buf()), e))?;
    Ok(xxhash_rust::xxh3::xxh3_64(&bytes))
}

/// Write a record into `<dir>/pending/`, temp → rename so a reader never sees a partial file.
///
/// The name is derived from the output segment path so a repeated prepare of the same rewrite
/// overwrites rather than accumulates.
#[cfg(feature = "compaction")]
pub(crate) fn write(dir: &Path, rec: &PendingRewrite) -> Result<PathBuf> {
    let pending = dir.join(PENDING_DIR);
    std::fs::create_dir_all(&pending).map_err(|e| Error::storage_io(Some(pending.clone()), e))?;
    let payload = rec.encode();
    let ck = xxhash_rust::xxh3::xxh3_64(&payload);
    let mut framed = Vec::with_capacity(HEADER + payload.len());
    framed.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    framed.extend_from_slice(&ck.to_le_bytes());
    framed.extend_from_slice(&payload);

    let name = format!(
        "{:016x}.job",
        xxhash_rust::xxh3::xxh3_64(rec.output.relative_path.as_bytes())
    );
    let final_path = pending.join(&name);
    let tmp = pending.join(format!("{name}.tmp"));
    let wrote = std::fs::write(&tmp, &framed).and_then(|()| std::fs::rename(&tmp, &final_path));
    if let Err(e) = wrote {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::storage_io(Some(final_path), e));
    }
    Ok(final_path)
}

/// Every readable pending record in `<dir>/pending/`, with the path it came from.
///
/// A record that is torn, checksum-failing, or of an unknown version is reported as `Err(path)` so
/// the caller can delete it rather than leave it to be re-read forever. Nothing here interprets the
/// record against the manifest — that is the commit step's job.
pub(crate) fn list(dir: &Path) -> Vec<std::result::Result<(PathBuf, PendingRewrite), PathBuf>> {
    let pending = dir.join(PENDING_DIR);
    let Ok(entries) = std::fs::read_dir(&pending) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("job") {
            continue; // stray `.tmp` from an interrupted write; cleaned by the next successful one
        }
        match read_one(&path) {
            Some(rec) => out.push(Ok((path, rec))),
            None => out.push(Err(path)),
        }
    }
    // Deterministic order so a commit pass is reproducible.
    out.sort_by_key(|r| match r {
        Ok((p, _)) => p.clone(),
        Err(p) => p.clone(),
    });
    out
}

fn read_one(path: &Path) -> Option<PendingRewrite> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < HEADER {
        return None;
    }
    let len = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
    let ck = u64::from_le_bytes(bytes[4..12].try_into().ok()?);
    let payload = bytes.get(HEADER..HEADER + len)?;
    if xxhash_rust::xxh3::xxh3_64(payload) != ck {
        return None;
    }
    PendingRewrite::decode(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PendingRewrite {
        PendingRewrite {
            table: Table::Logs,
            inputs: vec![
                "logs/2026-08-09/a.parquet".into(),
                "logs/2026-08-09/b.parquet".into(),
            ],
            output: SegmentRef {
                relative_path: "logs/2026-08-09/pending-c.parquet".into(),
                min_time_unix_nano: 5,
                max_time_unix_nano: 900,
                rows: 42,
            },
            output_digest: 0xdead_beef_1234_5678,
            promote: Promote::new(["env", "k8s.pod.name"]),
        }
    }

    #[test]
    fn a_record_round_trips_through_the_frame() {
        let dir = tempfile::tempdir().unwrap();
        let rec = sample();
        write(dir.path(), &rec).unwrap();
        let found = list(dir.path());
        assert_eq!(found.len(), 1);
        let (_, back) = found[0].as_ref().unwrap();
        assert_eq!(*back, rec, "every field survives encode → frame → decode");
    }

    /// Keys are arbitrary UTF-8 and the payload is tab-separated, so escaping has to be total rather
    /// than merely likely — a tab in a promoted key must not split a field.
    #[test]
    fn tabs_and_newlines_in_keys_survive() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = sample();
        rec.promote = Promote::new(["we\tird", "new\nline", "back\\slash"]);
        write(dir.path(), &rec).unwrap();
        let found = list(dir.path());
        let (_, back) = found[0].as_ref().unwrap();
        assert_eq!(back.promote, rec.promote);
    }

    /// A record torn or corrupted by a crash must be reported as unreadable, never half-applied.
    #[test]
    fn a_corrupt_record_is_rejected_not_guessed_at() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), &sample()).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let n = bytes.len();
        bytes[n - 1] ^= 0xff; // flip a payload byte; the checksum must catch it
        std::fs::write(&path, &bytes).unwrap();
        assert!(list(dir.path())[0].is_err());

        // Truncation is the other crash shape.
        std::fs::write(&path, &bytes[..HEADER + 2]).unwrap();
        assert!(list(dir.path())[0].is_err());
    }

    /// A record from a future version is discarded rather than partially understood. Its output is
    /// scratch, so nothing is lost but the housekeeper's effort.
    #[test]
    fn an_unknown_version_is_discarded() {
        let payload = b"version\t99\ntable\tlogs\ninput\ta\noutput\tb\nrows\t1\nmin_time\t0\nmax_time\t0\ndigest\t0\n";
        assert!(PendingRewrite::decode(payload).is_none());
    }

    #[test]
    fn digest_detects_a_changed_output() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("seg.parquet");
        std::fs::write(&f, b"hello").unwrap();
        let before = digest_file(&f).unwrap();
        std::fs::write(&f, b"hellp").unwrap();
        assert_ne!(before, digest_file(&f).unwrap());
    }
}
