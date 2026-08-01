//! End-to-end test of the Docker logging-driver plugin (the optional `docker` feature) over a
//! **real Unix socket**: bind the plugin endpoint in a temp directory, run `serve_plugin()` on a
//! background thread, and drive it exactly as `dockerd` would — `Plugin.Activate`, `Capabilities`,
//! `StartLogging` against a log stream we write ourselves, then `ReadLogs` to get the lines back.
//!
//! This covers what the socket-free `route()` unit tests cannot: the accept loop, the HTTP/1.1
//! request parsing over `AF_UNIX`, the FIFO reader thread, the batching ingest worker, and the
//! framed `ReadLogs` response — plus the assertion that matters most, that container output ends up
//! **queryable in the DB** as OTLP logs.
//!
//! One case needs a real FIFO (`mkfifo`) to exercise the blocking-open and live-streaming path; it
//! skips cleanly where the tool is unavailable. Everything else runs anywhere Unix sockets do. No
//! daemon, no network — within the hermetic `cargo test` rule (TESTING.md Layer 1).
//!
//! Gated on the `docker` feature: `cargo test -p imbh-server --features docker`. Absent the feature
//! the file compiles to nothing, so the default `cargo test --workspace` path is unaffected.
#![cfg(all(feature = "docker", unix))]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use imbh::Db;
use imbh::arrow::array::{Array, StringArray, UInt8Array};
use imbh_server::docker::entry::{EntryReader, LogEntry, PartialLogEntryMetadata, write_entry};
use imbh_server::docker::serve_plugin;

/// Longest a test waits for a line to travel FIFO → ingest worker → DB. The worker's default flush
/// interval is 200 ms, so this is ~25 flushes of slack for a loaded CI box.
const SETTLE: Duration = Duration::from_secs(5);

// ── the daemon side of the protocol ──────────────────────────────────────────────────────

/// A parsed plugin response.
struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// `POST path` to the plugin socket, reading the whole response.
///
/// `Connection: close` is explicit because the plugin speaks HTTP/1.1 with keep-alive now: without
/// it this read-to-EOF would sit out the server's header deadline between requests. Real Docker uses
/// Go's `net/http`, which reuses the connection and reads `Content-Length` instead.
fn post(socket: &Path, path: &str, body: &[u8]) -> Reply {
    let mut stream = UnixStream::connect(socket).expect("connect to the plugin socket");
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: docker\r\nContent-Type: application/json\r\n\
         Connection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).expect("write head");
    stream.write_all(body).expect("write body");
    stream.flush().expect("flush");

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response has a header/body separator");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("status code");
    // `ReadLogs` has no knowable length, so hyper frames it as `Transfer-Encoding: chunked`; the
    // small JSON endpoints carry a `Content-Length` and arrive verbatim.
    let raw_body = &raw[split + 4..];
    let body = match head
        .split("\r\n")
        .any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.contains("chunked"))
    {
        true => dechunk(raw_body),
        false => raw_body.to_vec(),
    };
    Reply { status, body }
}

/// Decode a complete `Transfer-Encoding: chunked` body.
fn dechunk(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut reader = ChunkedReader::new(std::io::BufReader::new(raw));
    reader.read_to_end(&mut out).expect("decode a chunked body");
    out
}

/// Un-chunks a `Transfer-Encoding: chunked` body as it arrives, so a streaming `ReadLogs` response
/// can be read frame by frame.
///
/// The plugin used to write frames raw and close the socket, which needed no decoding at all. hyper
/// frames a body of unknowable length properly; Docker's own client is Go's `net/http`, which
/// un-chunks transparently, so this is the test catching up to the wire rather than a behaviour
/// regression.
struct ChunkedReader<R: std::io::BufRead> {
    inner: R,
    /// Bytes left in the chunk currently being read.
    remaining: usize,
    /// Whether the terminating zero-length chunk has been seen.
    done: bool,
}

impl<R: std::io::BufRead> ChunkedReader<R> {
    fn new(inner: R) -> Self {
        ChunkedReader {
            inner,
            remaining: 0,
            done: false,
        }
    }

    /// Read the next chunk-size line, skipping the CRLF that terminates the previous chunk's data.
    fn next_chunk_size(&mut self) -> std::io::Result<Option<usize>> {
        for _ in 0..2 {
            let mut line = String::new();
            if self.inner.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            let line = line.trim();
            if line.is_empty() {
                continue; // the CRLF after the previous chunk's data
            }
            let size = line.split(';').next().unwrap_or_default();
            return usize::from_str_radix(size, 16).map(Some).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("not a chunk size: {line:?}"),
                )
            });
        }
        Ok(None)
    }
}

impl<R: std::io::BufRead> Read for ChunkedReader<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        if self.remaining == 0 {
            match self.next_chunk_size()? {
                Some(0) | None => {
                    self.done = true;
                    return Ok(0);
                }
                Some(size) => self.remaining = size,
            }
        }
        let want = out.len().min(self.remaining);
        let read = self.inner.read(&mut out[..want])?;
        self.remaining -= read;
        Ok(read)
    }
}

/// Start the plugin on a socket in a fresh temp dir; returns the DB it writes into.
fn start_plugin() -> (Arc<Db>, PathBuf, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let socket = tmp.path().join("imbh.sock");
    let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");

    let (plugin_db, plugin_socket) = (db.clone(), socket.clone());
    std::thread::spawn(move || {
        let _ = serve_plugin(plugin_db, &plugin_socket);
    });

    // Poll until the accept loop answers the handshake.
    let deadline = Instant::now() + SETTLE;
    while Instant::now() < deadline {
        if UnixStream::connect(&socket).is_ok() {
            return (db, socket, tmp);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("plugin socket never came up at {}", socket.display());
}

/// A `StartLogging` request body for `file` and a container.
fn start_logging_body(file: &Path, id: &str, name: &str, opts: &str) -> Vec<u8> {
    format!(
        r#"{{"File":{:?},"Info":{{"ContainerID":"{id}","ContainerName":"/{name}",
            "ContainerImageName":"nginx:1.27","ContainerLabels":{{"app":"cart"}},
            "ContainerEnv":["REGION=eu-1"],"Config":{{{opts}}}}}}}"#,
        file.display().to_string()
    )
    .into_bytes()
}

fn entry(source: &str, line: &str, time_nano: i64) -> LogEntry {
    LogEntry {
        source: source.to_owned(),
        time_nano,
        line: line.as_bytes().to_vec(),
        partial: false,
        partial_log_metadata: None,
    }
}

// ── DB assertions ────────────────────────────────────────────────────────────────────────

/// Poll until the `logs` table holds `want` rows, then return them ordered oldest-first as
/// `(body, severity_number, service)`.
fn wait_for_logs(db: &Arc<Db>, want: usize) -> Vec<(String, u8, String)> {
    let blocking = db.blocking();
    let deadline = Instant::now() + SETTLE;
    loop {
        let batches = blocking
            .sql("SELECT body, severity_number, service FROM logs ORDER BY time, body")
            .expect("query logs");
        let mut rows = Vec::new();
        for b in &batches {
            let body = b
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("body is Utf8");
            let severity = b
                .column(1)
                .as_any()
                .downcast_ref::<UInt8Array>()
                .expect("severity_number is UInt8");
            let service = b.column(2);
            for i in 0..b.num_rows() {
                rows.push((
                    body.value(i).to_owned(),
                    severity.value(i),
                    imbh::arrow::util::display::ArrayFormatter::try_new(
                        service,
                        &imbh::arrow::util::display::FormatOptions::default(),
                    )
                    .expect("format service")
                    .value(i)
                    .to_string(),
                ));
            }
        }
        if rows.len() >= want || Instant::now() >= deadline {
            assert_eq!(rows.len(), want, "expected {want} log rows, got {rows:?}");
            return rows;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The single `resource` JSON blob stored for the logs of `service`.
fn resource_json(db: &Arc<Db>, service: &str) -> String {
    let batches = db
        .blocking()
        .sql(&format!(
            "SELECT resource FROM logs WHERE service = '{service}' LIMIT 1"
        ))
        .expect("query resource");
    let column = batches
        .iter()
        .find(|b| b.num_rows() > 0)
        .map(|b| b.column(0).clone())
        .expect("at least one row");
    imbh::arrow::util::display::ArrayFormatter::try_new(
        &column,
        &imbh::arrow::util::display::FormatOptions::default(),
    )
    .expect("format resource")
    .value(0)
    .to_string()
}

// ── tests ────────────────────────────────────────────────────────────────────────────────

#[test]
fn handshake_declares_a_log_driver_that_can_read_logs() {
    let (_db, socket, _tmp) = start_plugin();

    let activate = post(&socket, "/Plugin.Activate", b"");
    assert_eq!(activate.status, 200);
    assert!(
        activate.text().contains("LogDriver"),
        "got {}",
        activate.text()
    );

    let caps = post(&socket, "/LogDriver.Capabilities", b"");
    assert_eq!(caps.status, 200);
    assert!(
        caps.text().contains("\"ReadLogs\":true"),
        "got {}",
        caps.text()
    );
}

#[test]
fn a_containers_stream_lands_in_the_database_as_otlp_logs() {
    let (db, socket, tmp) = start_plugin();

    // A container's log stream: the frames Docker would write to the FIFO. Written up front so the
    // reader sees a complete stream — the live-FIFO shape is covered separately below.
    let stream_path = tmp.path().join("container-stream");
    let mut framed = Vec::new();
    for e in [
        entry("stdout", "listening on :8080\n", 1_700_000_000_000_000_000),
        entry("stderr", "upstream timed out\n", 1_700_000_000_000_000_001),
        // A line Docker had to split into three frames must arrive as one record.
        LogEntry {
            partial: true,
            line: b"a very ".to_vec(),
            partial_log_metadata: Some(PartialLogEntryMetadata {
                last: false,
                id: "split".to_owned(),
                ordinal: 1,
            }),
            ..entry("stdout", "", 1_700_000_000_000_000_002)
        },
        LogEntry {
            partial: true,
            line: b"long ".to_vec(),
            partial_log_metadata: Some(PartialLogEntryMetadata {
                last: false,
                id: "split".to_owned(),
                ordinal: 2,
            }),
            ..entry("stdout", "", 1_700_000_000_000_000_003)
        },
        LogEntry {
            partial: true,
            line: b"line\n".to_vec(),
            partial_log_metadata: Some(PartialLogEntryMetadata {
                last: true,
                id: "split".to_owned(),
                ordinal: 3,
            }),
            ..entry("stdout", "", 1_700_000_000_000_000_004)
        },
    ] {
        write_entry(&mut framed, &e).expect("frame entry");
    }
    std::fs::write(&stream_path, &framed).expect("write the container stream");

    let start = post(
        &socket,
        "/LogDriver.StartLogging",
        &start_logging_body(
            &stream_path,
            "abc123def456",
            "web",
            r#""labels":"app","env":"REGION""#,
        ),
    );
    assert_eq!(start.status, 200);
    assert_eq!(start.text(), r#"{"Err":""}"#);

    let rows = wait_for_logs(&db, 3);
    assert_eq!(
        rows,
        vec![
            ("listening on :8080".to_owned(), 9, "web".to_owned()),
            ("upstream timed out".to_owned(), 17, "web".to_owned()),
            ("a very long line".to_owned(), 9, "web".to_owned()),
        ],
        "stdout → INFO, stderr → ERROR, split frames → one record, newlines stripped"
    );

    // Container identity is on the OTel resource, so it is queryable and survives compaction.
    let resource = resource_json(&db, "web");
    for want in [
        "\"container.id\":\"abc123def456\"",
        "\"container.name\":\"web\"",
        "\"container.image.name\":\"nginx:1.27\"",
        "\"container.runtime\":\"docker\"",
        "\"container.label.app\":\"cart\"",
        "\"container.env.REGION\":\"eu-1\"",
    ] {
        assert!(resource.contains(want), "{want} missing from {resource}");
    }

    // The queries `docs/DOCKER_LOG_DRIVER.md` tells operators to run must actually run — full-text
    // search over container output, and an error rate grouped by container.
    let blocking = db.blocking();
    let hits = blocking
        .sql("SELECT body FROM logs WHERE matches(body, 'upstream timed')")
        .expect("full-text search over container logs");
    assert_eq!(hits.iter().map(|b| b.num_rows()).sum::<usize>(), 1);

    let errors = blocking
        .sql(
            "SELECT date_bin(INTERVAL '5 minutes', time) AS bucket, \
             json_get_str(resource, 'container.name') AS container, count(*) AS errors \
             FROM logs WHERE severity_number >= 17 GROUP BY 1, 2 ORDER BY 1",
        )
        .expect("error-rate rollup by container");
    assert_eq!(errors.iter().map(|b| b.num_rows()).sum::<usize>(), 1);

    // Stopping a stream the plugin owns is clean, and a second stop is not an error.
    let stop_body = format!(r#"{{"File":{:?}}}"#, stream_path.display().to_string());
    for _ in 0..2 {
        let stop = post(&socket, "/LogDriver.StopLogging", stop_body.as_bytes());
        assert_eq!(stop.text(), r#"{"Err":""}"#);
    }
}

#[test]
fn read_logs_streams_stored_lines_back_to_docker() {
    let (db, socket, tmp) = start_plugin();

    let stream_path = tmp.path().join("readable-stream");
    let mut framed = Vec::new();
    for i in 0..5i64 {
        let source = if i % 2 == 0 { "stdout" } else { "stderr" };
        write_entry(
            &mut framed,
            &entry(
                source,
                &format!("line {i}\n"),
                1_700_000_000_000_000_000 + i,
            ),
        )
        .expect("frame entry");
    }
    std::fs::write(&stream_path, &framed).expect("write the container stream");

    let start = post(
        &socket,
        "/LogDriver.StartLogging",
        &start_logging_body(&stream_path, "readme00", "reader", ""),
    );
    assert_eq!(start.text(), r#"{"Err":""}"#);
    wait_for_logs(&db, 5);

    // The whole history, oldest first — what `docker logs` prints.
    let all = read_logs(
        &socket,
        r#"{"Info":{"ContainerID":"readme00"},"Config":{"Tail":-1}}"#,
    );
    let lines: Vec<String> = all
        .iter()
        .map(|e| String::from_utf8_lossy(&e.line).into_owned())
        .collect();
    assert_eq!(
        lines,
        ["line 0\n", "line 1\n", "line 2\n", "line 3\n", "line 4\n"],
        "history must come back oldest-first with the newline restored"
    );
    // The stream each line came from round-trips through the `log.iostream` attribute.
    assert_eq!(all[0].source, "stdout");
    assert_eq!(all[1].source, "stderr");
    assert_eq!(all[0].time_nano, 1_700_000_000_000_000_000);

    // `docker logs --tail 2` — the newest two, still oldest-first.
    let tail = read_logs(
        &socket,
        r#"{"Info":{"ContainerID":"readme00"},"Config":{"Tail":2}}"#,
    );
    let tail_lines: Vec<String> = tail
        .iter()
        .map(|e| String::from_utf8_lossy(&e.line).into_owned())
        .collect();
    assert_eq!(tail_lines, ["line 3\n", "line 4\n"]);

    // `--since` is honored (nanosecond 2 onward), and the `ReadConfig` spelling of the request
    // object works as well as `Config`.
    let since = read_logs(
        &socket,
        r#"{"Info":{"ContainerID":"readme00"},
            "ReadConfig":{"Tail":-1,"Since":"2023-11-14T22:13:20.000000002Z"}}"#,
    );
    assert_eq!(since.len(), 3, "since must drop the first two lines");

    // An unknown container yields an empty stream rather than everything in the DB.
    assert!(read_logs(&socket, r#"{"Info":{"ContainerID":"nope"},"Config":{}}"#).is_empty());
}

/// `POST /LogDriver.ReadLogs` and decode the framed response body.
fn read_logs(socket: &Path, body: &str) -> Vec<LogEntry> {
    let reply = post(socket, "/LogDriver.ReadLogs", body.as_bytes());
    assert_eq!(reply.status, 200);
    let mut reader = EntryReader::new(std::io::Cursor::new(reply.body));
    let mut out = Vec::new();
    while let Some(e) = reader.next_entry().expect("decode a response frame") {
        out.push(e);
    }
    out
}

/// `docker logs -f`: the response stays open, new container output shows up as it is ingested, and
/// the stream ends once the container's log stream is gone and the tail goes quiet.
#[test]
fn follow_mode_streams_new_lines_and_ends_with_the_container() {
    let (db, socket, tmp) = start_plugin();
    let stream_path = tmp.path().join("follow-stream");
    std::fs::write(
        &stream_path,
        frame_all(&[entry("stdout", "first\n", 1_700_000_000_000_000_000)]),
    )
    .expect("write the container stream");

    let start = post(
        &socket,
        "/LogDriver.StartLogging",
        &start_logging_body(&stream_path, "follow01", "follower", ""),
    );
    assert_eq!(start.text(), r#"{"Err":""}"#);
    wait_for_logs(&db, 1);

    // Open a follow stream and consume the history frame.
    let mut follow = UnixStream::connect(&socket).expect("connect for follow");
    follow
        .set_read_timeout(Some(SETTLE))
        .expect("set a read timeout so a stuck follow fails the test");
    let body = r#"{"Info":{"ContainerID":"follow01"},"Config":{"Tail":-1,"Follow":true}}"#;
    write!(
        follow,
        "POST /LogDriver.ReadLogs HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .expect("write the follow request");
    follow.flush().expect("flush");

    let mut reader = std::io::BufReader::new(follow.try_clone().expect("clone the follow socket"));
    skip_headers(&mut reader);
    let mut frames = EntryReader::new(ChunkedReader::new(reader));
    assert_eq!(
        frames
            .next_entry()
            .expect("history frame")
            .expect("one row")
            .line,
        b"first\n"
    );

    // New output on a *second* stream of the same container reaches the open follow response.
    let second_path = tmp.path().join("follow-stream-2");
    std::fs::write(
        &second_path,
        frame_all(&[entry("stderr", "later\n", 1_700_000_000_000_000_001)]),
    )
    .expect("write the second stream");
    let start = post(
        &socket,
        "/LogDriver.StartLogging",
        &start_logging_body(&second_path, "follow01", "follower", ""),
    );
    assert_eq!(start.text(), r#"{"Err":""}"#);

    let live = frames
        .next_entry()
        .expect("follow must deliver the new line")
        .expect("a frame, not end of stream");
    assert_eq!(live.line, b"later\n");
    assert_eq!(live.source, "stderr");

    // The container goes away: both streams stop, and the follow response ends on its own rather
    // than hanging until the client disconnects.
    for path in [&stream_path, &second_path] {
        let stop = format!(r#"{{"File":{:?}}}"#, path.display().to_string());
        assert_eq!(
            post(&socket, "/LogDriver.StopLogging", stop.as_bytes()).text(),
            r#"{"Err":""}"#
        );
    }
    assert_eq!(
        frames.next_entry().expect("clean end of stream"),
        None,
        "follow must end once the container's streams are gone"
    );
}

/// Regression: follow must not skip a line that was *emitted* before the follow started but
/// *stored* after it.
///
/// A record's timestamp is when the container wrote the line; ingest lands it up to one batch
/// interval later. `docker logs -f` on a container that just started therefore sees an empty
/// history, and an implementation that then sets its watermark to "now" drops every line already
/// emitted but not yet stored — in practice the first line of every follow. Found against a real
/// dockerd: `docker logs -f` returned 4 of 5 `tick` lines, missing `tick 1`, while all 5 were
/// queryable in the database.
///
/// Reproduced here without timing luck: the follow is opened against an empty database, and only
/// then is a stream started whose record carries an **older** timestamp.
#[test]
fn follow_delivers_a_line_timestamped_before_the_follow_began() {
    let (db, socket, tmp) = start_plugin();

    // Open the follow first — the DB has no rows at all, so history comes back empty.
    let mut follow = UnixStream::connect(&socket).expect("connect for follow");
    follow.set_read_timeout(Some(SETTLE)).expect("read timeout");
    let body = r#"{"Info":{"ContainerID":"late0001"},"Config":{"Tail":-1,"Follow":true}}"#;
    write!(
        follow,
        "POST /LogDriver.ReadLogs HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .expect("write the follow request");
    follow.flush().expect("flush");
    let mut reader = std::io::BufReader::new(follow.try_clone().expect("clone"));
    skip_headers(&mut reader);
    let mut frames = EntryReader::new(ChunkedReader::new(reader));

    // Now produce a line stamped well in the past — the shape of a line emitted before the follow
    // opened but batched into the DB after it.
    let stream_path = tmp.path().join("late-stream");
    std::fs::write(
        &stream_path,
        frame_all(&[entry(
            "stdout",
            "emitted before the follow\n",
            1_600_000_000_000_000_000,
        )]),
    )
    .expect("write the container stream");
    let start = post(
        &socket,
        "/LogDriver.StartLogging",
        &start_logging_body(&stream_path, "late0001", "late", ""),
    );
    assert_eq!(start.text(), r#"{"Err":""}"#);
    wait_for_logs(&db, 1);

    let delivered = frames
        .next_entry()
        .expect("follow must not skip an older-timestamped line")
        .expect("a frame, not end of stream");
    assert_eq!(delivered.line, b"emitted before the follow\n");
}

/// Read past the response head, leaving the reader positioned at the first frame.
fn skip_headers<R: std::io::BufRead>(reader: &mut R) {
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read a response header");
        assert!(n > 0, "response ended inside its headers");
        if line.trim_end().is_empty() {
            return;
        }
    }
}

fn frame_all(entries: &[LogEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        write_entry(&mut out, e).expect("frame entry");
    }
    out
}

/// The production shape: a real FIFO, opened for writing by the "daemon" while the plugin blocks on
/// opening the read end, streamed live, then closed. Skips where `mkfifo` is unavailable.
#[test]
fn a_live_fifo_streams_into_the_database() {
    let (db, socket, tmp) = start_plugin();
    let fifo = tmp.path().join("live-fifo");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !made {
        eprintln!("skipping: mkfifo unavailable");
        return;
    }

    // Docker opens the write end before calling StartLogging; the open blocks until the plugin
    // opens the read end, which is exactly the interlock being tested here.
    let writer_path = fifo.clone();
    let writer = std::thread::spawn(move || {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&writer_path)
            .expect("open the fifo for writing");
        for i in 0..3i64 {
            write_entry(
                &mut f,
                &entry(
                    "stdout",
                    &format!("live {i}\n"),
                    1_700_000_000_000_000_000 + i,
                ),
            )
            .expect("write a frame");
            f.flush().expect("flush the frame");
            std::thread::sleep(Duration::from_millis(10));
        }
        // Closing the write end is what ends the plugin's read loop.
    });

    let start = post(
        &socket,
        "/LogDriver.StartLogging",
        &start_logging_body(&fifo, "live0001", "live", ""),
    );
    assert_eq!(start.text(), r#"{"Err":""}"#);
    writer.join().expect("writer thread");

    let rows = wait_for_logs(&db, 3);
    assert_eq!(
        rows.iter().map(|(b, _, _)| b.as_str()).collect::<Vec<_>>(),
        ["live 0", "live 1", "live 2"]
    );
}

/// Shutting the plugin down must not strand container output.
///
/// The batching worker is deliberately configured with a **30-second** flush interval, so nothing
/// reaches the DB on the normal path within the life of this test: every row that shows up afterwards
/// got there because `serve_plugin_*_until` drained the ingest queue on its way out. That is the
/// property a `docker stop` of the plugin depends on — lines already read off a container's stream are
/// in the DB before `main` closes it.
#[test]
fn shutdown_drains_queued_container_lines_and_unlinks_the_socket() {
    use imbh_server::Shutdown;
    use imbh_server::docker::ingest::IngestConfig;
    use imbh_server::docker::serve_plugin_with_until;

    let tmp = tempfile::tempdir().expect("temp dir");
    let socket = tmp.path().join("imbh.sock");
    let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");
    let shutdown = Shutdown::with_drain_timeout(SETTLE);

    let ingest = IngestConfig {
        // Long enough that no batch closes on its own during this test.
        flush_interval: Duration::from_secs(30),
        ..IngestConfig::default()
    };
    let server = {
        let (db, socket, shutdown) = (db.clone(), socket.clone(), shutdown.clone());
        std::thread::spawn(move || {
            serve_plugin_with_until(db, &socket, ingest, shutdown).expect("serve the plugin")
        })
    };
    let deadline = Instant::now() + SETTLE;
    while Instant::now() < deadline && UnixStream::connect(&socket).is_err() {
        std::thread::sleep(Duration::from_millis(10));
    }

    // A stream the reader can consume to EOF, so its lines are queued and then parked in the worker's
    // half-full batch.
    let stream_file = tmp.path().join("container.log");
    let entries: Vec<LogEntry> = (0..3)
        .map(|i| {
            entry(
                "stdout",
                &format!("queued {i}\n"),
                1_700_000_000_000_000_000 + i,
            )
        })
        .collect();
    std::fs::write(&stream_file, frame_all(&entries)).expect("write the log stream");

    let start = post(
        &socket,
        "/LogDriver.StartLogging",
        &start_logging_body(&stream_file, "queued001", "queued", ""),
    );
    assert_eq!(start.text(), r#"{"Err":""}"#);

    // Give the reader time to consume the file and queue all three lines, then confirm the batch is
    // still open — otherwise this test would pass even with no drain at all.
    std::thread::sleep(Duration::from_millis(300));
    let before = db
        .blocking()
        .sql("SELECT count(*) AS c FROM logs")
        .expect("count before shutdown");
    assert_eq!(
        before.iter().map(|b| b.num_rows()).sum::<usize>(),
        1,
        "one count row"
    );

    shutdown.trigger();
    server.join().expect("the plugin accept loop returns");

    // The drain put them in the DB.
    let rows = wait_for_logs(&db, 3);
    assert_eq!(
        rows.iter().map(|(b, _, _)| b.as_str()).collect::<Vec<_>>(),
        ["queued 0", "queued 1", "queued 2"],
        "the ingest queue was not drained on shutdown"
    );

    // And the socket is gone, so a restart binds a clean path.
    assert!(
        !socket.exists(),
        "the plugin socket outlived the plugin: {}",
        socket.display()
    );
}
