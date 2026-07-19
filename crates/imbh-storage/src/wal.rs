//! Write-ahead log (ARCHITECTURE.md §7).
//!
//! Append-only frames of `(len, xxh3, lsn, signal, payload)` where `payload` is the raw OTLP
//! export-request bytes and `xxh3` is an XXH3-64 checksum over `lsn || signal || payload`.
//! Each frame carries a monotonic log-sequence number (LSN). Recovery is idempotent because
//! the manifest records a durable watermark and replay re-ingests only records with
//! `lsn > watermark` (ARCHITECTURE.md §7).
//!
//! The log is split into **numbered segment files** `wal.<NNNNNNNN>.log` (zero-padded monotonic
//! seq) in the DB directory. Appends land in the current (highest-seq) segment; once it reaches
//! [`ROTATE_BYTES`] it is fsync'd and frozen, and a fresh segment becomes current. Space is
//! reclaimed after a seal by **deleting whole superseded segment files** ([`Wal::reclaim`]) rather
//! than rewriting the log — no write amplification.
//!
//! A crash mid-append leaves a torn or checksum-failing tail; frame scanning stops at the first
//! such frame and returns everything before it — the expected shape of an unclean shutdown, not
//! corruption. Only the current (highest-seq) segment can carry such a tail; earlier segments were
//! fsync'd before rotation.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use imbh_core::{Error, Result, WalPhase};
use xxhash_rust::xxh3::xxh3_64;

/// Signal tags stored per frame. All three signals are written and replayed.
pub const SIGNAL_LOGS: u8 = 0;
pub const SIGNAL_TRACES: u8 = 1;
pub const SIGNAL_METRICS: u8 = 2;

/// Rotate to a fresh segment once the current one reaches this many bytes. 64 MiB keeps the segment
/// count low while bounding how much a single reclaim/replay touches per file (ARCHITECTURE.md §7).
pub(crate) const ROTATE_BYTES: u64 = 64 << 20;

/// Fixed frame header size: len(4) + xxh3(8) + lsn(8) + signal(1).
const HEADER: usize = 4 + 8 + 8 + 1;

/// One decoded WAL frame.
#[derive(Clone, Debug)]
pub struct WalRecord {
    pub lsn: u64,
    pub signal: u8,
    pub payload: Vec<u8>,
}

/// The segment filename for sequence `seq` (`wal.<NNNNNNNN>.log`, zero-padded to 8 digits; wider
/// only if `seq` overflows 8 digits, which still parses and sorts numerically).
pub(crate) fn segment_name(seq: u64) -> String {
    format!("wal.{seq:08}.log")
}

/// Parse the sequence out of a `wal.<digits>.log` segment filename, or `None` if it is not one
/// (rejects the legacy `wal.log`, `MANIFEST`, `*.parquet`, temp files, etc.).
fn parse_segment_seq(name: &str) -> Option<u64> {
    let mid = name.strip_prefix("wal.")?.strip_suffix(".log")?;
    if mid.is_empty() || !mid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    mid.parse::<u64>().ok()
}

/// Enumerate the WAL segment files in `dir`, sorted by sequence ascending. Empty when the dir has
/// none (or does not exist yet).
pub(crate) fn list_segments(dir: &Path) -> Result<Vec<(u64, PathBuf)>> {
    let mut segs = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(segs),
        Err(e) => return Err(Error::open_ctx("read WAL dir", e)),
    };
    for entry in entries.flatten() {
        if let Some(seq) = entry.file_name().to_str().and_then(parse_segment_seq) {
            segs.push((seq, entry.path()));
        }
    }
    segs.sort_by_key(|(seq, _)| *seq);
    Ok(segs)
}

/// Path of the current (highest-seq) WAL segment in `dir`; the first segment name when none exist
/// yet. Test-only helper for tests that need to touch the segment appends currently target.
#[cfg(test)]
pub(crate) fn current_segment_path(dir: &Path) -> PathBuf {
    list_segments(dir)
        .ok()
        .and_then(|mut v| v.pop())
        .map(|(_, p)| p)
        .unwrap_or_else(|| dir.join(segment_name(1)))
}

/// Total on-disk size of every WAL segment in `dir` — the `wal_bytes()` gauge. Best-effort: a
/// segment that races deletion just contributes 0.
pub(crate) fn total_bytes(dir: &Path) -> u64 {
    list_segments(dir)
        .map(|segs| {
            segs.iter()
                .map(|(_, p)| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
                .sum()
        })
        .unwrap_or(0)
}

/// The checksum an intact frame must carry. Covers the `len` prefix too, so a corrupted length
/// that happens to point at a plausible payload region is still caught.
fn checksum(len: u32, lsn: u64, signal: u8, payload: &[u8]) -> u64 {
    let mut buf = Vec::with_capacity(13 + payload.len());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(&lsn.to_le_bytes());
    buf.push(signal);
    buf.extend_from_slice(payload);
    xxh3_64(&buf)
}

/// Serialize one `(len, xxh3, lsn, signal, payload)` frame. Caller must ensure
/// `payload.len() <= u32::MAX` (enforced by [`Wal::append`]).
fn encode_frame(lsn: u64, signal: u8, payload: &[u8]) -> Vec<u8> {
    let len = payload.len() as u32;
    let ck = checksum(len, lsn, signal, payload);
    let mut frame = Vec::with_capacity(HEADER + payload.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&ck.to_le_bytes());
    frame.extend_from_slice(&lsn.to_le_bytes());
    frame.push(signal);
    frame.extend_from_slice(payload);
    frame
}

/// Scan the intact frames out of one segment's `bytes`, seeded with `last_lsn` so LSN
/// strict-monotonicity is enforced *across* segment boundaries too. Returns the frames, the number of
/// bytes consumed (the offset of the first unparsed byte — always a clean frame boundary), and
/// whether the whole buffer parsed cleanly to EOF (`false` marks a torn/corrupt/non-monotonic tail —
/// the caller stops there and ignores any later segment). The consumed count lets an incremental
/// reader ([`WalTailCursor`]) resume exactly where a torn tail left off once the writer completes it.
fn scan_frames(bytes: &[u8], mut last_lsn: Option<u64>) -> (Vec<WalRecord>, usize, bool) {
    let mut records = Vec::new();
    let mut pos = 0usize;
    while pos + HEADER <= bytes.len() {
        let len_u32 = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap());
        let len = len_u32 as usize;
        let ck = u64::from_le_bytes(bytes[pos + 4..pos + 12].try_into().unwrap());
        let lsn = u64::from_le_bytes(bytes[pos + 12..pos + 20].try_into().unwrap());
        let signal = bytes[pos + 20];
        let start = pos + HEADER;
        let end = match start.checked_add(len) {
            Some(e) if e <= bytes.len() => e,
            _ => return (records, pos, false), // torn tail
        };
        let payload = &bytes[start..end];
        if checksum(len_u32, lsn, signal, payload) != ck {
            return (records, pos, false); // corrupt tail
        }
        // LSNs are assigned strictly increasing at append; a non-increasing value on replay means a
        // corrupt (yet checksum-passing) region — stop rather than accept out-of-order frames.
        if last_lsn.is_some_and(|prev| lsn <= prev) {
            return (records, pos, false);
        }
        last_lsn = Some(lsn);
        records.push(WalRecord {
            lsn,
            signal,
            payload: payload.to_vec(),
        });
        pos = end;
    }
    // Clean EOF only if we consumed every byte; trailing bytes shorter than a header are a torn tail.
    (records, pos, pos == bytes.len())
}

/// Parse all intact frames from a single WAL segment file (empty if it does not exist), stopping at
/// the first torn or checksum-failing frame.
pub fn read_frames(path: &Path) -> Result<Vec<WalRecord>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::open_ctx("read WAL", e)),
    };
    Ok(scan_frames(&bytes, None).0)
}

/// Read a segment's bytes from `start` to EOF — only the tail appended since a previous scan. Empty
/// when the file is absent (raced a reclaim delete) or `start` is already at/after EOF. Reading from
/// the offset (rather than the whole file) is what makes an incremental refresh cost O(new bytes).
fn read_segment_from(path: &Path, start: u64) -> Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::open_ctx("open WAL segment", e)),
    };
    if start > 0 {
        f.seek(SeekFrom::Start(start))
            .map_err(|e| Error::open_ctx("seek WAL segment", e))?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)
        .map_err(|e| Error::open_ctx("read WAL segment", e))?;
    Ok(buf)
}

/// The highest LSN recorded in a segment file (`0` when empty). Read once at open to seed the
/// per-segment max-LSN bookkeeping that drives reclaim without re-reading files at runtime.
fn segment_max_lsn(path: &Path) -> Result<u64> {
    Ok(read_frames(path)?.iter().map(|r| r.lsn).max().unwrap_or(0))
}

/// Parse every intact frame across all segments of `dir`, in seq order, as one stream — the
/// open/replay view. LSN strict-monotonicity is carried across segment boundaries. Scanning stops
/// at the first torn/corrupt segment tail (normally only possible in the current, highest-seq
/// segment) and ignores anything after it.
pub fn read_all_frames(dir: &Path) -> Result<Vec<WalRecord>> {
    let mut out = Vec::new();
    let mut last_lsn: Option<u64> = None;
    for (_seq, path) in list_segments(dir)? {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue, // raced a reclaim delete
            Err(e) => return Err(Error::open_ctx("read WAL segment", e)),
        };
        let (frames, _consumed, complete) = scan_frames(&bytes, last_lsn);
        if let Some(f) = frames.last() {
            last_lsn = Some(f.lsn);
        }
        out.extend(frames);
        if !complete {
            break; // torn/corrupt tail — everything past this point is unreliable
        }
    }
    Ok(out)
}

/// Incremental reader-side cursor over the WAL tail (ARCHITECTURE.md §5). A long-lived read-only
/// handle reuses one cursor across [`read_disk_snapshot_incremental`] calls so each refresh scans
/// only the bytes appended since the last — instead of re-reading and re-decoding every segment from
/// byte 0, the O(WAL) cost [`read_all_frames`] pays on every query. It accumulates the intact frames
/// it has seen; each refresh drops the sealed prefix (`lsn <= watermark`, now durable in a segment)
/// and appends the freshly written tail. A default (fresh) cursor reproduces a full scan exactly, so
/// it is a pure performance cache, not a behavior change.
#[derive(Debug, Default)]
pub struct WalTailCursor {
    /// Byte offset scanned so far in each live segment (always a clean frame boundary), keyed by seq.
    offsets: BTreeMap<u64, u64>,
    /// Running max LSN across every frame scanned — seeds strict monotonicity across newly appended
    /// bytes and across segment boundaries. Never rewound (LSNs only increase).
    last_lsn: Option<u64>,
    /// The intact frames scanned so far across the live segments, in stream (LSN) order, minus the
    /// sealed prefix pruned on each refresh by [`Self::prune_sealed`].
    frames: Vec<WalRecord>,
}

impl WalTailCursor {
    /// Scan the bytes appended to `dir`'s WAL segments since the last call, appending new intact
    /// frames. Stops at the first torn/corrupt/non-monotonic tail (only the current, highest-seq
    /// segment can carry one) and resumes from that clean boundary next time — so a frame the writer
    /// is mid-appending is picked up once it is complete.
    pub(crate) fn advance(&mut self, dir: &Path) -> Result<()> {
        let segs = list_segments(dir)?;
        // Forget offset bookkeeping for reclaimed (deleted) segments so the map can't grow unbounded;
        // their frames, if any linger in `frames`, are dropped by `prune_sealed` (reclaim only
        // deletes segments whose frames are already `<= watermark`).
        self.offsets
            .retain(|seq, _| segs.iter().any(|(s, _)| s == seq));
        for (seq, path) in segs {
            let start = *self.offsets.get(&seq).unwrap_or(&0);
            let bytes = read_segment_from(&path, start)?;
            if bytes.is_empty() {
                continue; // nothing new (or the segment raced a reclaim delete)
            }
            let (frames, consumed, complete) = scan_frames(&bytes, self.last_lsn);
            if let Some(f) = frames.last() {
                self.last_lsn = Some(f.lsn);
            }
            self.offsets.insert(seq, start + consumed as u64);
            self.frames.extend(frames);
            if !complete {
                break; // torn tail — resume from `start + consumed` next refresh
            }
        }
        Ok(())
    }

    /// Drop the sealed prefix: every frame with `lsn <= watermark` now lives in a segment. Safe
    /// because the watermark only advances, so a dropped frame is never needed by a later snapshot.
    pub(crate) fn prune_sealed(&mut self, watermark: u64) {
        self.frames.retain(|r| r.lsn > watermark);
    }

    /// The accumulated live tail (after [`Self::prune_sealed`], exactly the frames `lsn > watermark`).
    pub(crate) fn frames(&self) -> &[WalRecord] {
        &self.frames
    }
}

/// Open (creating if absent) a segment file for appending.
fn open_append_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| Error::open_ctx("open WAL segment", e))
}

/// Unlink a segment file, treating "already gone" as success.
fn remove_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::wal(WalPhase::Remove, e)),
    }
}

/// fsync a directory so a preceding create/rename inside it is durable (the new current segment's
/// directory entry must persist before appends to it are acknowledged).
fn fsync_dir(dir: &Path) -> Result<()> {
    let f = File::open(dir).map_err(|e| Error::wal(WalPhase::DirFsync, e))?;
    f.sync_all().map_err(|e| Error::wal(WalPhase::DirFsync, e))
}

/// An open, append-only WAL over numbered segment files in one DB directory.
pub struct Wal {
    dir: PathBuf,
    /// Sequence of the current (append target) segment; always the highest on disk.
    seq: u64,
    /// The current segment, opened for append.
    file: File,
    /// Bytes written to the current segment so far (drives rotation).
    current_bytes: u64,
    /// Highest LSN in the current segment (`0` while empty).
    current_max_lsn: u64,
    /// Rotate once the current segment reaches this size (production: [`ROTATE_BYTES`]).
    rotate_bytes: u64,
    /// `(seq, max_lsn)` of every closed (frozen, never-again-appended) segment, ascending. Lets
    /// reclaim decide deletion without re-reading any file at runtime.
    closed: Vec<(u64, u64)>,
}

impl Wal {
    /// Open the WAL for `dir`: enumerate segments, adopt the highest-seq one as the current append
    /// target (creating `wal.00000001.log` when the dir has none), and load each closed segment's
    /// max-LSN once so reclaim needs no runtime reads.
    pub fn open(dir: &Path) -> Result<Self> {
        Self::open_with_rotate(dir, ROTATE_BYTES)
    }

    /// [`Wal::open`] with an explicit rotation threshold (test hook — production uses
    /// [`ROTATE_BYTES`] via [`Wal::open`]).
    pub(crate) fn open_with_rotate(dir: &Path, rotate_bytes: u64) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|e| Error::open_ctx("create WAL dir", e))?;
        let mut segs = list_segments(dir)?;
        let (seq, file, current_bytes, current_max_lsn, closed) = match segs.pop() {
            None => {
                // Fresh DB: create the first segment and make its dir entry durable.
                let seq = 1;
                let path = dir.join(segment_name(seq));
                let file = open_append_file(&path)?;
                fsync_dir(dir)?;
                (seq, file, 0, 0, Vec::new())
            }
            Some((cur_seq, cur_path)) => {
                // `segs` now holds only the closed (lower-seq) segments; load their max-LSNs once.
                let mut closed = Vec::with_capacity(segs.len());
                for (seq, path) in &segs {
                    closed.push((*seq, segment_max_lsn(path)?));
                }
                let current_bytes = std::fs::metadata(&cur_path).map(|m| m.len()).unwrap_or(0);
                let current_max_lsn = segment_max_lsn(&cur_path)?;
                let file = open_append_file(&cur_path)?;
                (cur_seq, file, current_bytes, current_max_lsn, closed)
            }
        };
        Ok(Wal {
            dir: dir.to_path_buf(),
            seq,
            file,
            current_bytes,
            current_max_lsn,
            rotate_bytes,
            closed,
        })
    }

    /// Append one frame to the current segment, rotating to a fresh segment once it reaches the
    /// rotation threshold. Does not fsync the frame; the caller applies the [`imbh_core::WalMode`]
    /// policy via [`Wal::sync`].
    pub fn append(&mut self, lsn: u64, signal: u8, payload: &[u8]) -> Result<()> {
        if payload.len() > u32::MAX as usize {
            return Err(Error::payload_too_large(payload.len(), u32::MAX as u64));
        }
        let frame = encode_frame(lsn, signal, payload);
        self.file
            .write_all(&frame)
            .map_err(|e| Error::wal(WalPhase::Append, e))?;
        self.current_bytes += frame.len() as u64;
        self.current_max_lsn = self.current_max_lsn.max(lsn);
        // Rotate *after* writing so a frame is never split across segments (the crossing frame stays
        // whole in the now-closed segment).
        if self.current_bytes >= self.rotate_bytes {
            self.rotate()?;
        }
        Ok(())
    }

    /// Flush the current segment's frame data to stable storage.
    pub fn sync(&mut self) -> Result<()> {
        self.file
            .sync_data()
            .map_err(|e| Error::wal(WalPhase::Fsync, e))
    }

    /// Freeze the current segment and open the next one as the new append target. The old segment is
    /// fsync'd first so, once closed, it is fully durable and never torn.
    fn rotate(&mut self) -> Result<()> {
        self.file
            .sync_data()
            .map_err(|e| Error::wal(WalPhase::Rotate, e))?;
        self.closed.push((self.seq, self.current_max_lsn));
        let next_seq = self.seq + 1;
        let path = self.dir.join(segment_name(next_seq));
        let file = open_append_file(&path)?;
        fsync_dir(&self.dir)?; // the new segment's dir entry must persist before we append to it
        self.seq = next_seq;
        self.file = file;
        self.current_bytes = 0;
        self.current_max_lsn = 0;
        #[cfg(feature = "tracing")]
        tracing::debug!(seq = next_seq, "WAL rotated to new segment");
        Ok(())
    }

    /// Reclaim WAL space after a seal by deleting whole superseded segments (ARCHITECTURE.md §7):
    /// first rotate the current segment to a fresh one so the pre-seal frames sit in a now-closed
    /// segment, then unlink every closed segment whose max-LSN `<= watermark` (fully captured by a
    /// sealed segment). A closed segment straddling the watermark (max-LSN `> watermark`, e.g. frames
    /// ingested concurrently with the seal) is retained until a later seal supersedes it; the fresh
    /// current segment is never deleted. No rewrite — whole-file unlink only.
    ///
    /// Safe to interrupt: the manifest watermark already makes replay skip the dropped frames, so a
    /// crash before/after this only affects reclaimed space, never correctness. The caller must hold
    /// the WAL mutex so no append races the rotation.
    pub fn reclaim(&mut self, watermark: u64) -> Result<()> {
        self.rotate()?;
        let mut retained = Vec::with_capacity(self.closed.len());
        for (seq, max_lsn) in std::mem::take(&mut self.closed) {
            if max_lsn <= watermark {
                remove_if_exists(&self.dir.join(segment_name(seq)))?;
            } else {
                retained.push((seq, max_lsn));
            }
        }
        self.closed = retained;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_segments(dir: &Path) -> usize {
        list_segments(dir).unwrap().len()
    }

    fn lsns(dir: &Path) -> Vec<u64> {
        read_all_frames(dir)
            .unwrap()
            .iter()
            .map(|f| f.lsn)
            .collect()
    }

    #[test]
    fn parses_and_rejects_segment_names() {
        assert_eq!(parse_segment_seq("wal.00000001.log"), Some(1));
        assert_eq!(parse_segment_seq("wal.00000042.log"), Some(42));
        assert_eq!(parse_segment_seq("wal.123456789.log"), Some(123_456_789));
        assert_eq!(parse_segment_seq("wal.log"), None);
        assert_eq!(parse_segment_seq("wal..log"), None);
        assert_eq!(parse_segment_seq("wal.0001a.log"), None);
        assert_eq!(parse_segment_seq("MANIFEST"), None);
        assert_eq!(parse_segment_seq("logs/2020-01-01/x.parquet"), None);
    }

    #[test]
    fn append_read_roundtrip_single_segment() {
        let dir = tempfile::tempdir().unwrap();
        let mut wal = Wal::open(dir.path()).unwrap();
        wal.append(1, SIGNAL_LOGS, b"one").unwrap();
        wal.append(2, SIGNAL_LOGS, b"two").unwrap();
        wal.sync().unwrap();
        assert_eq!(count_segments(dir.path()), 1);
        assert_eq!(lsns(dir.path()), vec![1, 2]);
    }

    #[test]
    fn rotation_and_whole_segment_reclaim() {
        let dir = tempfile::tempdir().unwrap();
        // Tiny rotation threshold so a couple of small frames force a real segment boundary — the
        // production 64 MiB path is identical, just larger.
        let mut wal = Wal::open_with_rotate(dir.path(), 64).unwrap();

        // A frame larger than the threshold fills segment 1 and rotates to segment 2…
        wal.append(1, SIGNAL_LOGS, &[0u8; 100]).unwrap();
        // …the next (small) frame lands in segment 2.
        wal.append(2, SIGNAL_LOGS, b"small").unwrap();
        wal.sync().unwrap();

        assert_eq!(
            count_segments(dir.path()),
            2,
            "rotation split the log in two"
        );
        assert!(dir.path().join(segment_name(1)).exists());
        // Frames stream back in order across the segment boundary — no data loss on rotation.
        assert_eq!(lsns(dir.path()), vec![1, 2]);

        // Seal at watermark = 1: segment 1 (max-LSN 1 <= 1) is fully superseded and deleted whole;
        // segment 2 (max-LSN 2 > 1) straddles the watermark and is retained.
        wal.reclaim(1).unwrap();

        assert!(
            !dir.path().join(segment_name(1)).exists(),
            "the superseded segment file is unlinked, not rewritten"
        );
        assert_eq!(
            lsns(dir.path()),
            vec![2],
            "the still-unsealed record survives whole-segment reclaim"
        );
    }

    #[test]
    fn reclaim_empties_wal_when_all_below_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let mut wal = Wal::open(dir.path()).unwrap();
        for lsn in 1..=5 {
            wal.append(lsn, SIGNAL_LOGS, b"payload").unwrap();
        }
        wal.sync().unwrap();
        assert!(total_bytes(dir.path()) > 0);

        wal.reclaim(5).unwrap(); // every frame <= watermark → all closed segments deleted
        assert_eq!(
            total_bytes(dir.path()),
            0,
            "a fully-superseded WAL reclaims to an empty current segment"
        );
        assert!(lsns(dir.path()).is_empty());

        // A post-reclaim append lands in the fresh current segment and survives a reopen.
        wal.append(6, SIGNAL_LOGS, b"after").unwrap();
        wal.sync().unwrap();
        drop(wal);
        assert_eq!(lsns(dir.path()), vec![6]);
    }

    #[test]
    fn torn_tail_in_current_segment_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut wal = Wal::open(dir.path()).unwrap();
            wal.append(1, SIGNAL_LOGS, b"good").unwrap();
            wal.sync().unwrap();
        }
        // Corrupt the current segment's tail with a partial (sub-header) frame.
        let seg = current_segment_path(dir.path());
        let mut bytes = std::fs::read(&seg).unwrap();
        bytes.extend_from_slice(&[0xff; 7]);
        std::fs::write(&seg, &bytes).unwrap();

        let frames = read_all_frames(dir.path()).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload, b"good");
    }

    #[test]
    fn cross_segment_lsn_monotonicity_stops_at_break() {
        // A closed segment whose tail duplicates an LSN must halt the cross-segment scan there.
        let dir = tempfile::tempdir().unwrap();
        let mut wal = Wal::open_with_rotate(dir.path(), 64).unwrap();
        wal.append(1, SIGNAL_LOGS, &[0u8; 100]).unwrap(); // seg1, then rotate
        wal.append(2, SIGNAL_LOGS, b"x").unwrap(); // seg2
        wal.sync().unwrap();
        // Hand-append a frame reusing lsn 2 (<= last seen) to segment 2's tail: the scan must stop.
        let seg2 = current_segment_path(dir.path());
        let mut bytes = std::fs::read(&seg2).unwrap();
        bytes.extend_from_slice(&encode_frame(2, SIGNAL_LOGS, b"dup"));
        std::fs::write(&seg2, &bytes).unwrap();
        assert_eq!(lsns(dir.path()), vec![1, 2], "non-monotonic tail ignored");
    }
}
