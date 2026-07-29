//! The Docker log-driver wire format: length-prefixed protobuf `LogEntry` frames.
//!
//! Both directions of the plugin contract use the same framing — the per-container FIFO the daemon
//! writes (ingest) and the `/LogDriver.ReadLogs` response body the plugin writes (`docker logs`).
//! Each frame is a **4-byte big-endian length** followed by that many bytes of a protobuf-encoded
//! [`LogEntry`], mirroring `github.com/docker/docker/api/types/plugins/logdriver` (`entry.proto`).
//!
//! The message types are declared here with prost's derive rather than generated from a `.proto`,
//! so the `docker` feature needs no build-time codegen and adds **no crate** to the graph (prost is
//! already in the default tree via `imbh-otlp`). The schema is five fields and frozen in Docker's
//! API, so hand-declaring it is cheaper than a `build.rs`.

use std::io::{self, Read, Write};

use prost::Message;

/// Reject a frame claiming to be larger than this. Docker splits container output into ~16 KiB
/// entries (see [`PartialAssembler`]), so anything near this bound is a desynchronized stream, not a
/// real log line — bail instead of allocating whatever a corrupt length prefix asks for.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// One log message on the wire — Docker's `logdriver.LogEntry`.
#[derive(Clone, PartialEq, Message)]
pub struct LogEntry {
    /// `"stdout"` or `"stderr"`.
    #[prost(string, tag = "1")]
    pub source: String,
    /// Capture time, nanoseconds since the Unix epoch.
    #[prost(int64, tag = "2")]
    pub time_nano: i64,
    /// The raw line bytes. Docker includes the trailing newline.
    #[prost(bytes = "vec", tag = "3")]
    pub line: Vec<u8>,
    /// Set when this frame is one chunk of a line Docker had to split.
    #[prost(bool, tag = "4")]
    pub partial: bool,
    /// Chunk correlation, present on modern daemons only.
    #[prost(message, optional, tag = "5")]
    pub partial_log_metadata: Option<PartialLogEntryMetadata>,
}

/// Chunk correlation for a split line — Docker's `logdriver.PartialLogEntryMetadata`.
#[derive(Clone, PartialEq, Message)]
pub struct PartialLogEntryMetadata {
    #[prost(bool, tag = "1")]
    pub last: bool,
    #[prost(string, tag = "2")]
    pub id: String,
    #[prost(int32, tag = "3")]
    pub ordinal: i32,
}

/// A framed [`LogEntry`] reader over any byte stream (the container FIFO in production, a `Cursor`
/// or a plain file in tests).
pub struct EntryReader<R: Read> {
    inner: R,
    buf: Vec<u8>,
}

impl<R: Read> EntryReader<R> {
    pub fn new(inner: R) -> Self {
        EntryReader {
            inner,
            buf: Vec::new(),
        }
    }

    /// The next entry, or `None` at a clean end of stream (EOF **on a frame boundary**). EOF part-way
    /// through a frame is an error: it means the writer died mid-message and the caller should say so
    /// rather than silently truncate.
    pub fn next_entry(&mut self) -> io::Result<Option<LogEntry>> {
        let mut len = [0u8; 4];
        if !read_full(&mut self.inner, &mut len)? {
            return Ok(None);
        }
        let n = u32::from_be_bytes(len) as usize;
        if n > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("log entry frame of {n} bytes exceeds the {MAX_FRAME_BYTES}-byte limit"),
            ));
        }
        self.buf.clear();
        self.buf.resize(n, 0);
        self.inner.read_exact(&mut self.buf)?;
        LogEntry::decode(&self.buf[..])
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

/// Fill `buf` completely. `Ok(false)` means the stream ended before the first byte (a clean
/// boundary); a short read after that is `UnexpectedEof`.
fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<bool> {
    let mut read = 0;
    while read < buf.len() {
        match r.read(&mut buf[read..]) {
            Ok(0) if read == 0 => return Ok(false),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Write one length-prefixed frame. The prefix and payload go out in a single `write_all` so a
/// reader on the other end never sees a half-written header.
pub fn write_entry<W: Write>(w: &mut W, entry: &LogEntry) -> io::Result<()> {
    let len = entry.encoded_len();
    let mut frame = Vec::with_capacity(4 + len);
    frame.extend_from_slice(&(len as u32).to_be_bytes());
    entry
        .encode(&mut frame)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    w.write_all(&frame)
}

/// Reassembles the chunks Docker emits for a line longer than its ~16 KiB buffer.
///
/// Two daemon dialects are handled by the same state machine:
/// - **Modern**: every chunk carries `partial_log_metadata`; the final one has `last = true`.
/// - **Legacy** (no metadata): chunks are `partial = true` and the final one is `partial = false`.
///
/// Chunks are keyed by the metadata id (legacy chunks share the empty key), so interleaved splits
/// from different streams of the same container reassemble independently. The first chunk's
/// timestamp and source win — that is when the line was produced.
#[derive(Default)]
pub struct PartialAssembler {
    pending: Vec<(String, LogEntry)>,
}

/// Cap on a reassembled line. Docker itself does not bound how many chunks a line becomes, so
/// without this a single unterminated stream could grow the buffer without limit. On overflow the
/// line is emitted truncated rather than dropped — losing the tail of one pathological line beats
/// losing the line, or the process.
const MAX_ASSEMBLED_BYTES: usize = 1024 * 1024;

impl PartialAssembler {
    /// Feed one wire entry; returns a complete logical line when this entry finishes one.
    pub fn push(&mut self, entry: LogEntry) -> Option<LogEntry> {
        let key = entry
            .partial_log_metadata
            .as_ref()
            .map(|m| m.id.clone())
            .unwrap_or_default();
        let last = entry.partial_log_metadata.as_ref().map(|m| m.last);

        if !entry.partial && !self.has(&key) {
            // The common case: a whole line in one frame, nothing buffered for its key.
            return Some(entry);
        }

        let (still_partial, len) = self.append(&key, entry);
        // Modern: the chunk said it was the last. Legacy: the terminating chunk is not partial.
        if last == Some(true) || !still_partial || len >= MAX_ASSEMBLED_BYTES {
            return self.take(&key);
        }
        None
    }

    /// Flush whatever is still buffered — called when the stream ends so a container whose last line
    /// was never terminated still lands in the DB.
    pub fn drain(&mut self) -> Vec<LogEntry> {
        std::mem::take(&mut self.pending)
            .into_iter()
            .map(|(_, e)| e)
            .collect()
    }

    fn has(&self, key: &str) -> bool {
        self.pending.iter().any(|(k, _)| k == key)
    }

    /// Append `entry`'s bytes to the buffered line for `key` (starting it if absent). Returns
    /// `(is_still_partial, accumulated_len)` by value so the caller can decide to emit without
    /// holding a borrow of `self`.
    fn append(&mut self, key: &str, entry: LogEntry) -> (bool, usize) {
        match self.pending.iter().position(|(k, _)| k == key) {
            Some(i) => {
                let held = &mut self.pending[i].1;
                let room = MAX_ASSEMBLED_BYTES.saturating_sub(held.line.len());
                let take = room.min(entry.line.len());
                held.line.extend_from_slice(&entry.line[..take]);
                // The terminating chunk decides how the line ends; keep the head's time/source.
                held.partial = entry.partial;
                held.partial_log_metadata = entry.partial_log_metadata;
                (held.partial, held.line.len())
            }
            None => {
                let state = (entry.partial, entry.line.len());
                self.pending.push((key.to_owned(), entry));
                state
            }
        }
    }

    fn take(&mut self, key: &str) -> Option<LogEntry> {
        let i = self.pending.iter().position(|(k, _)| k == key)?;
        Some(self.pending.remove(i).1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(line: &str) -> LogEntry {
        LogEntry {
            source: "stdout".to_owned(),
            time_nano: 1_700_000_000_000_000_000,
            line: line.as_bytes().to_vec(),
            partial: false,
            partial_log_metadata: None,
        }
    }

    fn framed(entries: &[LogEntry]) -> Vec<u8> {
        let mut out = Vec::new();
        for e in entries {
            write_entry(&mut out, e).expect("write frame");
        }
        out
    }

    #[test]
    fn frames_round_trip() {
        let want = vec![entry("hello\n"), entry("world\n")];
        let bytes = framed(&want);
        let mut r = EntryReader::new(io::Cursor::new(bytes));

        assert_eq!(r.next_entry().expect("first").as_ref(), Some(&want[0]));
        assert_eq!(r.next_entry().expect("second").as_ref(), Some(&want[1]));
        assert_eq!(r.next_entry().expect("eof"), None);
    }

    #[test]
    fn truncated_frame_is_an_error_not_a_silent_eof() {
        let mut bytes = framed(&[entry("hello\n")]);
        bytes.truncate(bytes.len() - 2);
        let mut r = EntryReader::new(io::Cursor::new(bytes));
        let err = r.next_entry().expect_err("short frame must error");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn oversized_length_prefix_is_rejected_without_allocating() {
        let mut bytes = 0xffff_ffffu32.to_be_bytes().to_vec();
        bytes.extend_from_slice(b"whatever");
        let mut r = EntryReader::new(io::Cursor::new(bytes));
        let err = r.next_entry().expect_err("oversized frame must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn whole_lines_pass_straight_through() {
        let mut a = PartialAssembler::default();
        let out = a.push(entry("one\n")).expect("complete line");
        assert_eq!(out.line, b"one\n");
        assert!(a.drain().is_empty());
    }

    #[test]
    fn modern_partials_reassemble_on_last() {
        let mut a = PartialAssembler::default();
        let chunk = |text: &str, ordinal: i32, last: bool| LogEntry {
            partial: true,
            line: text.as_bytes().to_vec(),
            partial_log_metadata: Some(PartialLogEntryMetadata {
                last,
                id: "split-1".to_owned(),
                ordinal,
            }),
            ..entry("")
        };
        assert!(a.push(chunk("he", 1, false)).is_none());
        assert!(a.push(chunk("ll", 2, false)).is_none());
        let out = a.push(chunk("o\n", 3, true)).expect("last chunk completes");
        assert_eq!(out.line, b"hello\n");
        assert_eq!(out.time_nano, 1_700_000_000_000_000_000);
        assert!(a.drain().is_empty());
    }

    #[test]
    fn interleaved_partial_ids_reassemble_independently() {
        let mut a = PartialAssembler::default();
        let chunk = |id: &str, text: &str, last: bool| LogEntry {
            partial: true,
            line: text.as_bytes().to_vec(),
            partial_log_metadata: Some(PartialLogEntryMetadata {
                last,
                id: id.to_owned(),
                ordinal: 0,
            }),
            ..entry("")
        };
        assert!(a.push(chunk("a", "aa", false)).is_none());
        assert!(a.push(chunk("b", "bb", false)).is_none());
        assert_eq!(
            a.push(chunk("b", "BB", true)).expect("b done").line,
            b"bbBB"
        );
        assert_eq!(
            a.push(chunk("a", "AA", true)).expect("a done").line,
            b"aaAA"
        );
    }

    #[test]
    fn legacy_partials_reassemble_on_the_non_partial_tail() {
        let mut a = PartialAssembler::default();
        let mut head = entry("he");
        head.partial = true;
        let mut mid = entry("ll");
        mid.partial = true;
        assert!(a.push(head).is_none());
        assert!(a.push(mid).is_none());
        let out = a.push(entry("o\n")).expect("non-partial tail completes");
        assert_eq!(out.line, b"hello\n");
    }

    #[test]
    fn unterminated_partials_are_drained_at_end_of_stream() {
        let mut a = PartialAssembler::default();
        let mut head = entry("dangling");
        head.partial = true;
        assert!(a.push(head).is_none());
        let left = a.drain();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].line, b"dangling");
    }

    #[test]
    fn reassembly_is_capped_and_emits_what_it_has() {
        let mut a = PartialAssembler::default();
        let big = "x".repeat(64 * 1024);
        let mut out = None;
        for _ in 0..64 {
            let mut c = entry(&big);
            c.partial = true;
            if let Some(done) = a.push(c) {
                out = Some(done);
                break;
            }
        }
        let out = out.expect("cap forces an emit");
        assert_eq!(out.line.len(), MAX_ASSEMBLED_BYTES);
    }
}
