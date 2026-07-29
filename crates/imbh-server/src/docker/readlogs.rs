//! `/LogDriver.ReadLogs` — serving `docker logs` back out of the database.
//!
//! The plugin advertises `ReadLogs` in its capabilities, so `docker logs <container>` comes here
//! instead of reading a file. The response body is the same length-prefixed protobuf frame stream
//! the FIFO uses ([`super::entry`]), written incrementally: a `docker logs -f` on a busy container
//! must not be buffered up in memory before the first byte reaches the client.
//!
//! History comes from a typed [`LogQuery`] on the `container.id` resource attribute — the query
//! surface an embedder would use (ARCHITECTURE.md §10.6), not a hand-written SQL string. Follow mode
//! then polls for records newer than the last one written.

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use imbh::{
    Db, Direction, LogEntry as LogRow, LogQuery, LogStringField, StringPredicate, Timestamp,
};

use super::entry::{LogEntry, write_entry};
use super::json;

/// Rows per query round-trip. Bounds the memory a `docker logs` with no `--tail` can occupy,
/// however long the container has been running.
const PAGE: usize = 1000;

/// How often follow mode polls for new records. Matches the ingest worker's default flush interval,
/// so a line typically appears within ~2 polls of being written.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Consecutive empty polls after the container's stream has ended before follow mode gives up.
/// Without this, `docker logs -f` on a stopped container would hang until the client disconnects.
const IDLE_POLLS_BEFORE_EXIT: u32 = 5;

/// Parsed `/LogDriver.ReadLogs` request.
struct ReadRequest {
    container_id: String,
    since: Option<Timestamp>,
    until: Option<Timestamp>,
    /// Negative means "all history", 0 means "none", positive means "the last N".
    tail: i64,
    follow: bool,
}

impl ReadRequest {
    fn parse(body: &[u8]) -> ReadRequest {
        let root = json::parse(body);
        let container_id = json::field(&root, "Info")
            .map(|info| json::string(info, "ContainerID"))
            .unwrap_or_default();
        // Docker's own proxy has shipped this object under both names; accept either rather than
        // silently serving unfiltered logs to a daemon that used the other one.
        let config = json::field(&root, "Config")
            .or_else(|| json::field(&root, "ReadConfig"))
            .cloned()
            .unwrap_or(imbh::AnyValue::Map(Vec::new()));
        ReadRequest {
            container_id,
            since: timestamp(&json::string(&config, "Since")),
            until: timestamp(&json::string(&config, "Until")),
            tail: json::int(&config, "Tail").unwrap_or(-1),
            follow: json::bool_at(&config, "Follow"),
        }
    }

    /// The base query: this container's records, within the requested window.
    ///
    /// The container id is container-operator-adjacent data, so it goes through `string_predicate`
    /// — whose values are **bound query parameters**, never interpolated into SQL text. An absent
    /// bound becomes the extreme timestamp, which `LogQuery` drops from the `WHERE` clause entirely.
    fn query(&self, start: Option<Timestamp>, end: Option<Timestamp>) -> LogQuery {
        LogQuery::new()
            .string_predicate(
                LogStringField::ResourceAttribute("container.id".to_owned()),
                StringPredicate::Eq,
                self.container_id.clone(),
            )
            .range_inclusive(
                start.unwrap_or(Timestamp::from_unix_nanos(i64::MIN)),
                end.unwrap_or(Timestamp::from_unix_nanos(i64::MAX)),
            )
    }
}

/// Serve one `ReadLogs` request, streaming frames into `out` until the history (and, under
/// `Follow`, the live tail) is exhausted or the client goes away.
///
/// `active` reports whether the container still has a live `StartLogging` stream; follow mode uses
/// it to decide when a quiet container is quiet because it stopped.
pub fn stream<W: Write>(
    db: &Arc<Db>,
    req_body: &[u8],
    out: &mut W,
    active: impl Fn(&str) -> bool,
) -> std::io::Result<()> {
    let req = ReadRequest::parse(req_body);
    if req.container_id.is_empty() {
        return Ok(());
    }
    // The plugin server is a blocking thread-per-connection design; the typed query API is async, so
    // this connection owns a current-thread runtime, exactly as the HTTP server's does.
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build docker ReadLogs runtime");

    // `Until` also bounds the history page, so forward paging cannot be shifted by rows arriving
    // mid-scan: the window is fixed before the first query runs.
    let history_end = req.until.unwrap_or_else(Timestamp::now);
    // `None` means "nothing written yet", which is *not* the same as "caught up to now" — see the
    // watermark note below.
    let mut last: Option<Timestamp> = match req.tail {
        // `--tail 0` asks for no history, so skipping straight to the present is its defined
        // semantic, not a race.
        0 => Some(history_end),
        n if n > 0 => {
            // The last N: read backwards (the `logs` table's natural direction), then emit oldest
            // first, which is the order `docker logs` prints.
            let q = req
                .query(req.since, Some(history_end))
                .direction(Direction::Backward)
                .limit(n as usize);
            let mut rows = rt.block_on(query(db, q));
            rows.reverse();
            write_rows(out, &rows)?
        }
        _ => write_history(&rt, db, &req, history_end, out)?,
    };

    if !req.follow {
        return Ok(());
    }

    let mut idle = 0;
    loop {
        std::thread::sleep(POLL_INTERVAL);
        if let Some(until) = req.until
            && last.is_some_and(|l| l >= until)
        {
            return Ok(());
        }
        // The follow watermark is the last record *written*, not the wall clock. When history came
        // back empty the watermark stays at the request's lower bound, because a record's timestamp
        // is when the container emitted the line while ingest lands it up to one batch interval
        // later: advancing to "now" on an empty history would permanently skip every line already
        // emitted but not yet stored. `docker logs -f` on a container that just started is exactly
        // that case — it cost the first line of every follow before this was fixed.
        let after = match last {
            Some(t) => Timestamp::from_unix_nanos(t.unix_nanos().saturating_add(1)),
            None => req.since.unwrap_or(Timestamp::from_unix_nanos(i64::MIN)),
        };
        let q = req
            .query(Some(after), req.until)
            .direction(Direction::Forward)
            .limit(PAGE);
        let rows = rt.block_on(query(db, q));
        match write_rows(out, &rows)? {
            Some(newest) => {
                last = Some(newest);
                idle = 0;
            }
            // Nothing new. Keep waiting while the container is still logging; once its stream is
            // gone and the tail has stayed quiet, `docker logs -f` should return like it does for
            // any other driver.
            None => {
                idle += 1;
                if !active(&req.container_id) && idle >= IDLE_POLLS_BEFORE_EXIT {
                    return Ok(());
                }
            }
        }
    }
}

/// Stream the whole history in `PAGE`-sized pages, oldest first. Returns the newest timestamp
/// written, if any.
fn write_history<W: Write>(
    rt: &tokio::runtime::Runtime,
    db: &Arc<Db>,
    req: &ReadRequest,
    end: Timestamp,
    out: &mut W,
) -> std::io::Result<Option<Timestamp>> {
    let mut cursor = None;
    let mut newest = None;
    loop {
        let mut q = req
            .query(req.since, Some(end))
            .direction(Direction::Forward)
            .limit(PAGE);
        if let Some(c) = cursor {
            q = q.after(c);
        }
        let page = match rt.block_on(db.logs().query(q)) {
            Ok(p) => p,
            Err(e) => {
                super::warn(&format!("ReadLogs query failed: {e}"));
                return Ok(newest);
            }
        };
        if let Some(ts) = write_rows(out, &page.entries)? {
            newest = Some(ts);
        }
        match page.next {
            Some(next) => cursor = Some(next),
            None => return Ok(newest),
        }
    }
}

/// Run one page of the typed query, reporting (not propagating) a query failure: a `docker logs`
/// that hits a transient query error should end the stream, not kill the plugin.
async fn query(db: &Arc<Db>, q: LogQuery) -> Vec<LogRow> {
    match db.logs().query(q).await {
        Ok(page) => page.entries,
        Err(e) => {
            super::warn(&format!("ReadLogs query failed: {e}"));
            Vec::new()
        }
    }
}

/// Write rows as frames, returning the newest timestamp written (`None` when `rows` is empty).
fn write_rows<W: Write>(out: &mut W, rows: &[LogRow]) -> std::io::Result<Option<Timestamp>> {
    let mut newest = None;
    for row in rows {
        write_entry(out, &to_entry(row))?;
        newest = Some(match newest {
            Some(t) if t > row.time => t,
            _ => row.time,
        });
    }
    if newest.is_some() {
        out.flush()?;
    }
    Ok(newest)
}

/// Rebuild the wire entry from a stored row. `log.iostream` restores the original stream, and the
/// trailing newline that ingest stripped goes back on, so `docker logs` prints what the container
/// printed.
fn to_entry(row: &LogRow) -> LogEntry {
    let mut line = row.body.clone().into_bytes();
    line.push(b'\n');
    LogEntry {
        source: row
            .attributes
            .get_str("log.iostream")
            .unwrap_or("stdout")
            .to_owned(),
        time_nano: row.time.unix_nanos(),
        line,
        partial: false,
        partial_log_metadata: None,
    }
}

/// Parse an RFC 3339 timestamp into epoch nanoseconds.
///
/// Go marshals `time.Time` this way, and its **zero value** — `0001-01-01T00:00:00Z` — is how
/// `docker logs` says "no bound", so any year ≤ 1 reads as unset. Written out by hand (a civil-date
/// conversion and some integer parsing) because the alternative is a date-time crate in a footprint-
/// gated graph, for one field of one optional feature.
fn timestamp(text: &str) -> Option<Timestamp> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (date, rest) = text.split_once(['T', 't', ' '])?;
    let mut d = date.splitn(3, '-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    if year <= 1 {
        return None; // Go's zero time — "unbounded".
    }

    // Split the offset off the end before parsing the clock. A `-` can only start an offset here,
    // never a field, so scanning from the right is unambiguous.
    let (clock, offset_secs) = match rest.chars().last() {
        Some('Z' | 'z') => (&rest[..rest.len() - 1], 0),
        _ => match rest.rfind(['+', '-']) {
            Some(i) => {
                let sign = if rest.as_bytes()[i] == b'-' { -1 } else { 1 };
                let mut o = rest[i + 1..].splitn(2, ':');
                let h: i64 = o.next()?.parse().ok()?;
                let m: i64 = o.next().unwrap_or("0").parse().ok()?;
                (&rest[..i], sign * (h * 3600 + m * 60))
            }
            None => (rest, 0),
        },
    };

    let (hms, frac) = clock.split_once('.').unwrap_or((clock, ""));
    let mut t = hms.splitn(3, ':');
    let hour: i64 = t.next()?.parse().ok()?;
    let minute: i64 = t.next().unwrap_or("0").parse().ok()?;
    let second: i64 = t.next().unwrap_or("0").parse().ok()?;

    // Fractional seconds: pad or truncate to exactly 9 digits.
    let digits: String = frac.chars().filter(char::is_ascii_digit).take(9).collect();
    let nanos: i64 = match digits.is_empty() {
        true => 0,
        false => digits.parse::<i64>().ok()? * 10i64.pow(9 - digits.len() as u32),
    };

    let secs = days_from_civil(year, month, day)
        .checked_mul(86_400)?
        .checked_add(hour * 3600 + minute * 60 + second - offset_secs)?;
    secs.checked_mul(1_000_000_000)
        .and_then(|ns| ns.checked_add(nanos))
        .map(Timestamp::from_unix_nanos)
}

/// Days from 1970-01-01 to a proleptic-Gregorian civil date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_match_known_epochs() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(days_from_civil(2026, 7, 29), 20663);
    }

    #[test]
    fn rfc3339_parses_the_shapes_go_emits() {
        let ns = |s: &str| timestamp(s).map(|t| t.unix_nanos());
        assert_eq!(ns("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(ns("1970-01-01T00:00:01Z"), Some(1_000_000_000));
        assert_eq!(ns("1970-01-01T00:00:00.5Z"), Some(500_000_000));
        assert_eq!(ns("1970-01-01T00:00:00.123456789Z"), Some(123_456_789));
        // More than nanosecond precision is truncated, not rejected.
        assert_eq!(ns("1970-01-01T00:00:00.1234567891Z"), Some(123_456_789));
        assert_eq!(
            ns("2026-07-29T00:00:00Z"),
            Some(20663 * 86_400 * 1_000_000_000)
        );
    }

    #[test]
    fn rfc3339_honors_numeric_offsets() {
        let ns = |s: &str| timestamp(s).map(|t| t.unix_nanos());
        assert_eq!(ns("1970-01-01T09:00:00+09:00"), Some(0));
        assert_eq!(ns("1969-12-31T19:00:00-05:00"), Some(0));
        assert_eq!(ns("1970-01-01T00:00:00+00:00"), Some(0));
    }

    #[test]
    fn gos_zero_time_and_junk_read_as_unbounded() {
        assert_eq!(timestamp("0001-01-01T00:00:00Z"), None);
        assert_eq!(timestamp(""), None);
        assert_eq!(timestamp("   "), None);
        assert_eq!(timestamp("yesterday"), None);
        assert_eq!(timestamp("2026-07-29"), None);
    }

    #[test]
    fn request_defaults_to_all_history_without_follow() {
        let req = ReadRequest::parse(br#"{"Info":{"ContainerID":"abc"},"Config":{}}"#);
        assert_eq!(req.container_id, "abc");
        assert_eq!(req.tail, -1);
        assert!(!req.follow);
        assert_eq!(req.since, None);
        assert_eq!(req.until, None);
    }

    #[test]
    fn request_reads_the_read_config_alias() {
        let req = ReadRequest::parse(
            br#"{"Info":{"ContainerID":"abc"},
                 "ReadConfig":{"Tail":25,"Follow":true,"Since":"1970-01-01T00:00:02Z"}}"#,
        );
        assert_eq!(req.tail, 25);
        assert!(req.follow);
        assert_eq!(req.since.map(|t| t.unix_nanos()), Some(2_000_000_000));
    }

    #[test]
    fn stored_rows_round_trip_back_to_wire_entries() {
        let row = LogRow {
            time: Timestamp::from_unix_nanos(7),
            observed_time: None,
            severity_number: imbh::SeverityNumber(17),
            severity_text: Some("ERROR".to_owned()),
            service: Some("web".to_owned()),
            body: "boom".to_owned(),
            attributes: imbh::Attributes::from_pairs(vec![(
                "log.iostream".to_owned(),
                imbh::AnyValue::Str("stderr".to_owned()),
            )]),
            resource: imbh::Attributes::new(),
            scope: imbh::Attributes::new(),
            trace_id: None,
            span_id: None,
            flags: 0,
        };
        let entry = to_entry(&row);
        assert_eq!(entry.source, "stderr");
        assert_eq!(entry.time_nano, 7);
        assert_eq!(entry.line, b"boom\n");

        // A row with no `log.iostream` (e.g. ingested by something other than this driver) still
        // reads back as a valid entry rather than an empty source.
        let plain = LogRow {
            attributes: imbh::Attributes::new(),
            ..row
        };
        assert_eq!(to_entry(&plain).source, "stdout");
    }
}
