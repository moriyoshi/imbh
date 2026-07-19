//! Manifest IO (ARCHITECTURE.md §7): an **append-only delta log with a compacted checkpoint**,
//! replacing the M0 whole-file rewrite.
//!
//! On-disk layout, per DB directory:
//!
//! - `CURRENT` — a tiny text file naming the active manifest log (`MANIFEST-<NNNNNN>`). It is the
//!   single atomically-updated pointer: written temp → fsync → rename → fsync-dir, so a reader always
//!   reads a whole old or whole new name, never a torn one.
//! - `MANIFEST-<NNNNNN>` — an append-only log of **framed** records `| len(4) | xxh3(8) | payload |`.
//!   The first frame is a **checkpoint** (a `reset` edit carrying the full segment set + watermark);
//!   each later frame is a small **delta** (segments added/removed since, + an optional new
//!   watermark). Reconstructing the manifest = replay the frames in order. Frame scanning stops at the
//!   first torn/checksum-failing frame — a crash mid-append simply drops that (not-yet-durable) edit,
//!   exactly like the WAL.
//!
//! Why this shape. A seal now appends O(new segments) bytes instead of rewriting O(total segments);
//! the log is periodically folded back into a fresh single-frame checkpoint (a **roll**) once it
//! grows past [`CHECKPOINT_BYTES`], bounding both file size and reopen-replay cost. `CURRENT` +
//! per-number log files make a roll atomic to readers: a reader resolves `CURRENT`, replays that one
//! log, and — if the writer rolled and unlinked it mid-read — re-resolves `CURRENT` (the new log's
//! checkpoint already contains everything the old log held). Durability ordering is unchanged: an edit
//! is fsync'd before the writer reclaims the WAL frames or deletes the segment files it supersedes.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use imbh_core::{Error, Result, SegmentRef, Table};

use crate::{fsync_dir, table_from_manifest_name};

/// The `CURRENT` pointer filename.
const CURRENT_FILE: &str = "CURRENT";
/// The pre-v2 whole-file manifest filename — read (and migrated) once, on the first v2 open.
const LEGACY_FILE: &str = "MANIFEST";
/// Fold the delta log into a fresh checkpoint once it reaches this size. 256 KiB (~5k segment edits at
/// ~50 B/line) keeps reopen-replay fast while making a roll rare relative to seals.
const CHECKPOINT_BYTES: u64 = 1 << 18;
/// Fixed frame header: len(4) + xxh3(8).
const HEADER: usize = 4 + 8;
/// Bound on how many times a reader re-resolves `CURRENT` when a roll unlinks the log mid-read.
const CURRENT_READ_TRIES: usize = 6;

/// Reconstructed manifest state: per-table sealed-segment lists (insertion order) + the watermark.
/// The same shape the M0 whole-file manifest produced; `imbh-storage` seeds its in-RAM segment lists
/// from it on open and drives reader snapshots from it.
#[derive(Debug, Default, Clone)]
pub(crate) struct Manifest {
    pub logs: Vec<SegmentRef>,
    pub spans: Vec<SegmentRef>,
    pub metrics: BTreeMap<Table, Vec<SegmentRef>>,
    pub watermark: u64,
}

/// A borrowed view of the writer's current in-RAM state to persist — passed to [`ManifestWriter::persist`]
/// so it can diff against the last-persisted [`Manifest`] and emit only the delta.
pub(crate) struct ManifestView<'a> {
    pub logs: &'a [SegmentRef],
    pub spans: &'a [SegmentRef],
    pub metrics: &'a BTreeMap<Table, Vec<SegmentRef>>,
    pub watermark: u64,
}

impl ManifestView<'_> {
    fn to_manifest(&self) -> Manifest {
        Manifest {
            logs: self.logs.to_vec(),
            spans: self.spans.to_vec(),
            metrics: self.metrics.clone(),
            watermark: self.watermark,
        }
    }
}

/// One manifest edit. A checkpoint is `reset = true` with every current segment in `added`; a delta
/// carries just the change. Applied in order: `reset` clears, then `watermark`, then `removed`, then
/// `added` (so a compaction's source-drops precede its merged-adds).
#[derive(Default)]
struct Edit {
    reset: bool,
    watermark: Option<u64>,
    added: Vec<(Table, SegmentRef)>,
    removed: Vec<(Table, String)>,
}

impl Edit {
    fn is_empty(&self) -> bool {
        !self.reset && self.watermark.is_none() && self.added.is_empty() && self.removed.is_empty()
    }
}

fn manifest_name(num: u64) -> String {
    format!("MANIFEST-{num:06}")
}

/// `logs`/`spans`/`metrics_*` name → [`Table`] (the inverse of [`Table::as_str`]).
fn name_to_table(s: &str) -> Option<Table> {
    match s {
        "logs" => Some(Table::Logs),
        "spans" => Some(Table::Spans),
        other => table_from_manifest_name(other),
    }
}

/// `&mut` the list a table's segments live in, creating the metric entry on demand.
fn list_mut(m: &mut Manifest, table: Table) -> &mut Vec<SegmentRef> {
    match table {
        Table::Logs => &mut m.logs,
        Table::Spans => &mut m.spans,
        t => m.metrics.entry(t).or_default(),
    }
}

// ── framing ──────────────────────────────────────────────────────────────────────────────

fn frame(payload: &[u8]) -> Vec<u8> {
    let ck = xxhash_rust::xxh3::xxh3_64(payload);
    let mut f = Vec::with_capacity(HEADER + payload.len());
    f.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    f.extend_from_slice(&ck.to_le_bytes());
    f.extend_from_slice(payload);
    f
}

/// Intact frame payloads from a manifest log's bytes, in order, stopping at the first torn or
/// checksum-failing frame (a crash mid-append). Torn-tail-tolerant, like the WAL.
fn scan_frames(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + HEADER <= bytes.len() {
        let len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let ck = u64::from_le_bytes(bytes[pos + 4..pos + 12].try_into().unwrap());
        let start = pos + HEADER;
        let end = match start.checked_add(len) {
            Some(e) if e <= bytes.len() => e,
            _ => break, // torn tail
        };
        let payload = &bytes[start..end];
        if xxhash_rust::xxh3::xxh3_64(payload) != ck {
            break; // corrupt tail
        }
        out.push(payload);
        pos = end;
    }
    out
}

// ── edit codec (payload text) ────────────────────────────────────────────────────────────

fn encode_edit(e: &Edit) -> Vec<u8> {
    let mut s = String::new();
    if e.reset {
        s.push_str("R\n");
    }
    if let Some(w) = e.watermark {
        let _ = writeln!(s, "W\t{w}");
    }
    for (t, path) in &e.removed {
        let _ = writeln!(s, "-\t{}\t{}", t.as_str(), path);
    }
    for (t, seg) in &e.added {
        let _ = writeln!(
            s,
            "+\t{}\t{}\t{}\t{}\t{}",
            t.as_str(),
            seg.relative_path,
            seg.min_time_unix_nano,
            seg.max_time_unix_nano,
            seg.rows
        );
    }
    s.into_bytes()
}

fn decode_edit(payload: &[u8]) -> Result<Edit> {
    let text = std::str::from_utf8(payload)
        .map_err(|_| Error::corrupt_manifest(None, "non-UTF-8 manifest frame"))?;
    let mut e = Edit::default();
    for (i, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let mut p = line.split('\t');
        let corrupt = |what: &str| Error::corrupt_manifest(Some(i + 1), what.to_owned());
        match p.next() {
            Some("R") => e.reset = true,
            Some("W") => {
                let w = p
                    .next()
                    .and_then(|x| x.parse::<u64>().ok())
                    .ok_or_else(|| corrupt("invalid watermark"))?;
                e.watermark = Some(w);
            }
            Some("+") => {
                let table = p
                    .next()
                    .and_then(name_to_table)
                    .ok_or_else(|| corrupt("invalid add: table"))?;
                let path = p.next().ok_or_else(|| corrupt("invalid add: path"))?;
                let min = p
                    .next()
                    .and_then(|x| x.parse::<i64>().ok())
                    .ok_or_else(|| corrupt("invalid add: min_time"))?;
                let max = p
                    .next()
                    .and_then(|x| x.parse::<i64>().ok())
                    .ok_or_else(|| corrupt("invalid add: max_time"))?;
                let rows = p
                    .next()
                    .and_then(|x| x.parse::<u64>().ok())
                    .ok_or_else(|| corrupt("invalid add: rows"))?;
                e.added.push((
                    table,
                    SegmentRef {
                        relative_path: path.to_owned(),
                        min_time_unix_nano: min,
                        max_time_unix_nano: max,
                        rows,
                    },
                ));
            }
            Some("-") => {
                let table = p
                    .next()
                    .and_then(name_to_table)
                    .ok_or_else(|| corrupt("invalid remove: table"))?;
                let path = p.next().ok_or_else(|| corrupt("invalid remove: path"))?;
                e.removed.push((table, path.to_owned()));
            }
            _ => return Err(corrupt("unknown manifest record")),
        }
    }
    Ok(e)
}

fn apply_edit(m: &mut Manifest, e: Edit) {
    if e.reset {
        *m = Manifest::default();
    }
    if let Some(w) = e.watermark {
        m.watermark = w;
    }
    for (t, path) in e.removed {
        list_mut(m, t).retain(|s| s.relative_path != path);
        // Match the writer's pruned map: an emptied metric table carries no key.
        if let Table::Logs | Table::Spans = t {
        } else if m.metrics.get(&t).is_some_and(Vec::is_empty) {
            m.metrics.remove(&t);
        }
    }
    for (t, seg) in e.added {
        list_mut(m, t).push(seg);
    }
}

fn checkpoint_edit(state: &Manifest) -> Edit {
    let mut added = Vec::new();
    for s in &state.logs {
        added.push((Table::Logs, s.clone()));
    }
    for s in &state.spans {
        added.push((Table::Spans, s.clone()));
    }
    for (t, segs) in &state.metrics {
        for s in segs {
            added.push((*t, s.clone()));
        }
    }
    Edit {
        reset: true,
        watermark: Some(state.watermark),
        added,
        removed: Vec::new(),
    }
}

// ── diff (last-persisted → new) ──────────────────────────────────────────────────────────

fn diff_table(table: Table, old: &[SegmentRef], new: &[SegmentRef], edit: &mut Edit) {
    let new_paths: HashSet<&str> = new.iter().map(|s| s.relative_path.as_str()).collect();
    let old_paths: HashSet<&str> = old.iter().map(|s| s.relative_path.as_str()).collect();
    for s in old {
        if !new_paths.contains(s.relative_path.as_str()) {
            edit.removed.push((table, s.relative_path.clone()));
        }
    }
    for s in new {
        if !old_paths.contains(s.relative_path.as_str()) {
            edit.added.push((table, s.clone()));
        }
    }
}

fn diff(old: &Manifest, new: &ManifestView) -> Edit {
    let mut e = Edit::default();
    if new.watermark != old.watermark {
        e.watermark = Some(new.watermark);
    }
    diff_table(Table::Logs, &old.logs, new.logs, &mut e);
    diff_table(Table::Spans, &old.spans, new.spans, &mut e);
    let mut keys: BTreeSet<Table> = old.metrics.keys().copied().collect();
    keys.extend(new.metrics.keys().copied());
    for t in keys {
        let o = old.metrics.get(&t).map(Vec::as_slice).unwrap_or(&[]);
        let n = new.metrics.get(&t).map(Vec::as_slice).unwrap_or(&[]);
        diff_table(t, o, n, &mut e);
    }
    e
}

// ── low-level file IO ────────────────────────────────────────────────────────────────────

/// Write (creating/truncating) a manifest log file with one frame, fsync its contents.
fn write_manifest_file(path: &Path, frame_bytes: &[u8]) -> Result<()> {
    let mut f = File::create(path).map_err(|e| Error::storage_io(Some(path.to_path_buf()), e))?;
    f.write_all(frame_bytes)
        .map_err(|e| Error::storage_io(Some(path.to_path_buf()), e))?;
    f.sync_all()
        .map_err(|e| Error::storage_io(Some(path.to_path_buf()), e))?;
    Ok(())
}

/// Atomically point `CURRENT` at `name`: temp → fsync → rename → fsync-dir.
fn write_current(dir: &Path, name: &str) -> Result<()> {
    let tmp = dir.join(format!("{CURRENT_FILE}.tmp"));
    let final_path = dir.join(CURRENT_FILE);
    {
        let mut f = File::create(&tmp).map_err(|e| Error::storage_io(Some(tmp.clone()), e))?;
        f.write_all(format!("{name}\n").as_bytes())
            .map_err(|e| Error::storage_io(Some(tmp.clone()), e))?;
        f.sync_all()
            .map_err(|e| Error::storage_io(Some(tmp.clone()), e))?;
    }
    std::fs::rename(&tmp, &final_path)
        .map_err(|e| Error::storage_io(Some(final_path.clone()), e))?;
    fsync_dir(dir)?;
    Ok(())
}

/// Resolve `CURRENT` → the active manifest number, or `None` if there is no v2 manifest yet.
fn read_current(dir: &Path) -> Result<Option<u64>> {
    match std::fs::read_to_string(dir.join(CURRENT_FILE)) {
        Ok(s) => {
            let name = s.trim();
            let num = name
                .strip_prefix("MANIFEST-")
                .and_then(|d| d.parse::<u64>().ok())
                .ok_or_else(|| {
                    Error::corrupt_manifest(None, format!("invalid CURRENT: {name:?}"))
                })?;
            Ok(Some(num))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::open_ctx("read CURRENT", e)),
    }
}

/// Replay one manifest log into a [`Manifest`]. `Ok(None)` if the file is gone (raced a roll's unlink).
fn replay(dir: &Path, num: u64) -> Result<Option<Manifest>> {
    let bytes = match std::fs::read(dir.join(manifest_name(num))) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::open_ctx("read manifest log", e)),
    };
    let mut m = Manifest::default();
    for payload in scan_frames(&bytes) {
        apply_edit(&mut m, decode_edit(payload)?);
    }
    Ok(Some(m))
}

// ── legacy (pre-v2 whole-file MANIFEST) ──────────────────────────────────────────────────

/// Parse the M0 whole-file manifest (`#watermark N` + `<table>\t<path>\t<min>\t<max>\t<rows>` lines).
fn load_legacy(path: &Path) -> Result<Manifest> {
    let mut m = Manifest::default();
    let text = std::fs::read_to_string(path).map_err(|e| Error::open_ctx("read manifest", e))?;
    for (lineno, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#watermark ") {
            m.watermark = rest
                .trim()
                .parse()
                .map_err(|_| Error::corrupt_manifest(Some(lineno + 1), "invalid watermark"))?;
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 5 {
            return Err(Error::corrupt_manifest(
                Some(lineno + 1),
                "expected 5 fields",
            ));
        }
        let parse = |s: &str, what: &str| -> Result<i64> {
            s.parse::<i64>()
                .map_err(|_| Error::corrupt_manifest(Some(lineno + 1), format!("invalid {what}")))
        };
        let seg = SegmentRef {
            relative_path: parts[1].to_owned(),
            min_time_unix_nano: parse(parts[2], "min_time")?,
            max_time_unix_nano: parse(parts[3], "max_time")?,
            rows: parts[4]
                .parse::<u64>()
                .map_err(|_| Error::corrupt_manifest(Some(lineno + 1), "invalid rows"))?,
        };
        let table = name_to_table(parts[0]).ok_or_else(|| {
            Error::corrupt_manifest(Some(lineno + 1), format!("unknown table `{}`", parts[0]))
        })?;
        list_mut(&mut m, table).push(seg);
    }
    Ok(m)
}

// ── public entry points ──────────────────────────────────────────────────────────────────

/// Open the manifest for a **writer**: resolve `CURRENT` and replay it, migrating a legacy whole-file
/// `MANIFEST` on first sight. Returns the reconstructed state plus a [`ManifestWriter`] positioned to
/// append further edits. `active_num()` on the writer names the live log for orphan cleanup.
pub(crate) fn open(dir: &Path) -> Result<(Manifest, ManifestWriter)> {
    if let Some(num) = read_current(dir)? {
        let manifest = replay(dir, num)?.ok_or_else(|| {
            Error::corrupt_manifest(None, "CURRENT names a manifest log that does not exist")
        })?;
        let log_bytes = std::fs::metadata(dir.join(manifest_name(num)))
            .map(|m| m.len())
            .unwrap_or(0);
        let writer = ManifestWriter {
            dir: dir.to_path_buf(),
            current_num: num,
            log_bytes,
            last: manifest.clone(),
        };
        return Ok((manifest, writer));
    }
    // No v2 manifest. Migrate a legacy whole-file MANIFEST if present, else start empty.
    let legacy = dir.join(LEGACY_FILE);
    let mut writer = ManifestWriter {
        dir: dir.to_path_buf(),
        current_num: 0, // 0 = not yet materialized; the first persist writes MANIFEST-000001
        log_bytes: 0,
        last: Manifest::default(),
    };
    if legacy.exists() {
        let state = load_legacy(&legacy)?;
        // Materialize v2 (durable) *before* unlinking the legacy file — a crash in between must never
        // lose the manifest.
        writer.write_checkpoint(&state, 1)?;
        let _ = std::fs::remove_file(&legacy);
        return Ok((state, writer));
    }
    Ok((Manifest::default(), writer))
}

/// Read the manifest for a **reader** (no writer state). Resolves `CURRENT` and replays it, tolerating
/// a concurrent roll: if the named log was unlinked mid-read, re-resolve `CURRENT` (the newer log's
/// checkpoint holds everything the old one did) and retry, bounded. A legacy whole-file `MANIFEST` is
/// read as-is (a reader never migrates).
pub(crate) fn read(dir: &Path) -> Result<Manifest> {
    for _ in 0..CURRENT_READ_TRIES {
        match read_current(dir)? {
            Some(num) => match replay(dir, num)? {
                Some(m) => return Ok(m),
                None => continue, // roll unlinked it; re-resolve CURRENT
            },
            None => {
                let legacy = dir.join(LEGACY_FILE);
                if legacy.exists() {
                    return load_legacy(&legacy);
                }
                return Ok(Manifest::default());
            }
        }
    }
    Err(Error::storage_msg(
        "manifest read: CURRENT kept moving (writer rolling continuously)",
    ))
}

/// Write a fresh, self-contained v2 manifest (one checkpoint frame + `CURRENT`) into `dir` from
/// `view` — used by `Storage::snapshot` so the snapshot directory opens like any other DB.
pub(crate) fn write_fresh(dir: &Path, view: ManifestView) -> Result<()> {
    let mut w = ManifestWriter {
        dir: dir.to_path_buf(),
        current_num: 0,
        log_bytes: 0,
        last: Manifest::default(),
    };
    w.write_checkpoint(&view.to_manifest(), 1)
}

/// Append-only writer over a DB directory's manifest. Diffs each persist against the last-persisted
/// state and appends only the delta, rolling to a fresh checkpoint once the log grows past
/// [`CHECKPOINT_BYTES`].
pub(crate) struct ManifestWriter {
    dir: PathBuf,
    /// Active `MANIFEST-<num>` (0 = none materialized yet).
    current_num: u64,
    /// Bytes in the active log (drives the roll decision).
    log_bytes: u64,
    /// Last-persisted state, for diffing.
    last: Manifest,
}

impl ManifestWriter {
    /// The active manifest log's number, or `None` before the first persist. Used by orphan cleanup to
    /// keep the live log and delete stray `MANIFEST-*` from an interrupted roll.
    pub(crate) fn active_num(&self) -> Option<u64> {
        (self.current_num != 0).then_some(self.current_num)
    }

    /// Persist `view` durably: a no-op if nothing changed, else append the delta (fsync), or — if the
    /// log has grown past [`CHECKPOINT_BYTES`], or none exists yet — write a fresh checkpoint and flip
    /// `CURRENT` to it. On return the edit is durable, so the caller may reclaim WAL / delete files.
    pub(crate) fn persist(&mut self, view: ManifestView) -> Result<()> {
        let edit = diff(&self.last, &view);
        if edit.is_empty() {
            return Ok(());
        }
        let new_state = view.to_manifest();
        if self.current_num == 0 {
            // First persist on a fresh DB: materialize MANIFEST-000001 as a checkpoint.
            self.write_checkpoint(&new_state, 1)?;
        } else {
            let payload = encode_edit(&edit);
            let frame_len = (HEADER + payload.len()) as u64;
            if self.log_bytes + frame_len > CHECKPOINT_BYTES {
                // Fold the log into a fresh checkpoint of the full new state (a roll).
                self.write_checkpoint(&new_state, self.current_num + 1)?;
            } else {
                self.append_frame(&frame(&payload))?;
                self.log_bytes += frame_len;
            }
        }
        self.last = new_state;
        Ok(())
    }

    /// Append one frame to the active log and fsync it durable.
    fn append_frame(&self, frame_bytes: &[u8]) -> Result<()> {
        let path = self.dir.join(manifest_name(self.current_num));
        let mut f = OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|e| Error::storage_io(Some(path.clone()), e))?;
        f.write_all(frame_bytes)
            .map_err(|e| Error::storage_io(Some(path.clone()), e))?;
        f.sync_all()
            .map_err(|e| Error::storage_io(Some(path.clone()), e))?;
        Ok(())
    }

    /// Write `MANIFEST-<num>` as a single checkpoint frame, flip `CURRENT` to it, then unlink the
    /// previous log. Ordering makes every crash point recoverable: the new file is durable before
    /// `CURRENT` moves, and the old file is removed only after — a crash before the flip leaves the
    /// new file as an orphan (old still active), a crash after leaves the old file as an orphan (new
    /// active); either way `cleanup_orphans` sweeps the stray on the next open.
    fn write_checkpoint(&mut self, state: &Manifest, num: u64) -> Result<()> {
        let payload = encode_edit(&checkpoint_edit(state));
        let name = manifest_name(num);
        write_manifest_file(&self.dir.join(&name), &frame(&payload))?;
        write_current(&self.dir, &name)?;
        let prev = self.current_num;
        self.current_num = num;
        self.log_bytes = (HEADER + payload.len()) as u64;
        if prev != 0 && prev != num {
            let _ = std::fs::remove_file(self.dir.join(manifest_name(prev)));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(path: &str, min: i64, max: i64, rows: u64) -> SegmentRef {
        SegmentRef {
            relative_path: path.to_owned(),
            min_time_unix_nano: min,
            max_time_unix_nano: max,
            rows,
        }
    }

    fn view<'a>(
        logs: &'a [SegmentRef],
        spans: &'a [SegmentRef],
        metrics: &'a BTreeMap<Table, Vec<SegmentRef>>,
        watermark: u64,
    ) -> ManifestView<'a> {
        ManifestView {
            logs,
            spans,
            metrics,
            watermark,
        }
    }

    /// Assert a reader `read()` and a fresh writer `open()` both reconstruct the expected state.
    fn assert_state(dir: &Path, logs: &[SegmentRef], spans: &[SegmentRef], watermark: u64) {
        let r = read(dir).unwrap();
        assert_eq!(r.watermark, watermark);
        assert_eq!(r.logs, logs);
        assert_eq!(r.spans, spans);
        let (o, _) = open(dir).unwrap();
        assert_eq!(o.watermark, watermark);
        assert_eq!(o.logs, logs);
        assert_eq!(o.spans, spans);
    }

    #[test]
    fn checkpoint_and_deltas_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (m0, mut w) = open(dir.path()).unwrap();
        assert!(m0.logs.is_empty() && m0.watermark == 0);
        assert_eq!(w.active_num(), None, "no manifest until the first persist");

        // Seal 1: two log segments, watermark 5.
        let logs = vec![
            seg("logs/a.parquet", 0, 9, 3),
            seg("logs/b.parquet", 10, 19, 2),
        ];
        let empty = BTreeMap::new();
        w.persist(view(&logs, &[], &empty, 5)).unwrap();
        assert_eq!(w.active_num(), Some(1));
        assert_state(dir.path(), &logs, &[], 5);

        // Seal 2 (delta append): + one span, watermark 8.
        let spans = vec![seg("spans/s.parquet", 1, 7, 4)];
        w.persist(view(&logs, &spans, &empty, 8)).unwrap();
        assert_eq!(w.active_num(), Some(1), "delta appended to the same log");
        assert_state(dir.path(), &logs, &spans, 8);

        // Retention (delta append): drop the first log segment; watermark unchanged.
        let logs2 = vec![logs[1].clone()];
        w.persist(view(&logs2, &spans, &empty, 8)).unwrap();
        assert_state(dir.path(), &logs2, &spans, 8);

        // No-op persist writes nothing (empty diff).
        let num_before = w.current_num;
        let bytes_before = w.log_bytes;
        w.persist(view(&logs2, &spans, &empty, 8)).unwrap();
        assert_eq!((w.current_num, w.log_bytes), (num_before, bytes_before));
    }

    #[test]
    fn roll_folds_log_into_fresh_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let (_, mut w) = open(dir.path()).unwrap();
        let logs = vec![seg("logs/a.parquet", 0, 9, 3)];
        let empty = BTreeMap::new();
        w.persist(view(&logs, &[], &empty, 5)).unwrap();
        assert_eq!(w.current_num, 1);
        assert!(dir.path().join("MANIFEST-000001").exists());

        // Force the next persist over the roll threshold.
        w.log_bytes = CHECKPOINT_BYTES;
        let logs2 = vec![logs[0].clone(), seg("logs/b.parquet", 10, 19, 2)];
        w.persist(view(&logs2, &[], &empty, 9)).unwrap();
        assert_eq!(w.current_num, 2, "rolled to a fresh checkpoint");
        assert!(
            !dir.path().join("MANIFEST-000001").exists(),
            "old log unlinked"
        );
        assert!(dir.path().join("MANIFEST-000002").exists());
        // The checkpoint is self-contained: reopen reconstructs the full state.
        assert_state(dir.path(), &logs2, &[], 9);
    }

    #[test]
    fn legacy_whole_file_is_migrated_on_open() {
        let dir = tempfile::tempdir().unwrap();
        // Write an old-format whole-file MANIFEST.
        let legacy =
            "#watermark 7\nlogs\tlogs/a.parquet\t0\t9\t3\nspans\tspans/s.parquet\t1\t7\t4\n";
        std::fs::write(dir.path().join(LEGACY_FILE), legacy).unwrap();

        let (m, w) = open(dir.path()).unwrap();
        assert_eq!(m.watermark, 7);
        assert_eq!(m.logs, vec![seg("logs/a.parquet", 0, 9, 3)]);
        assert_eq!(m.spans, vec![seg("spans/s.parquet", 1, 7, 4)]);
        assert_eq!(w.active_num(), Some(1), "materialized as v2");
        assert!(
            !dir.path().join(LEGACY_FILE).exists(),
            "legacy file removed"
        );
        assert!(dir.path().join(CURRENT_FILE).exists());
        assert_state(
            dir.path(),
            &[seg("logs/a.parquet", 0, 9, 3)],
            &[seg("spans/s.parquet", 1, 7, 4)],
            7,
        );
    }

    #[test]
    fn torn_trailing_frame_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let (_, mut w) = open(dir.path()).unwrap();
        let logs = vec![seg("logs/a.parquet", 0, 9, 3)];
        let empty = BTreeMap::new();
        w.persist(view(&logs, &[], &empty, 5)).unwrap();

        // Append a truncated frame (a header claiming more bytes than follow) — a crash mid-append.
        let path = dir.path().join(manifest_name(1));
        let mut torn = std::fs::read(&path).unwrap();
        torn.extend_from_slice(&999u32.to_le_bytes()); // len
        torn.extend_from_slice(&0u64.to_le_bytes()); // checksum
        torn.extend_from_slice(b"partial"); // < 999 bytes of payload
        std::fs::write(&path, &torn).unwrap();

        // The torn tail is skipped; the last durable state stands.
        assert_state(dir.path(), &logs, &[], 5);
    }
}
