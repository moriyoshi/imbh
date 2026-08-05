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
    // This generator is blocking by design — it runs on a `spawn_blocking` task (see
    // `super::read_logs`) — while the typed query API is async, so it owns a current-thread runtime to
    // drive it. A blocking-pool thread is allowed to create and drive one: tokio marks it a blocking
    // region, so the "cannot start a runtime from within a runtime" rule does not apply there.
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
/// trailing newline that ingest stripped goes back on.
///
/// Two body shapes reach here. A **string** body is what the driver stores without remapping (and
/// what OTLP ingest stores for an unstructured record): it goes out verbatim, so `docker logs` is
/// byte-identical to what the container printed. A **map** body is what a remap script produces
/// (`docker-remap`); since the original line was not kept a second time, it is re-rendered as a
/// logfmt line — `ts=… level=… ` then its own fields.
fn to_entry(row: &LogRow) -> LogEntry {
    let mut line = match structured(&row.body) {
        Some(fields) => logfmt(row, &fields).into_bytes(),
        None => row.body.clone().into_bytes(),
    };
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

/// The body's fields when it is a structured (map) body, `None` when it is plain text.
///
/// `imbh-otlp` stores a map body as canonical JSON, so the cheap pre-test is the leading brace —
/// only a body that could be an object is handed to the parser.
fn structured(body: &str) -> Option<Vec<(String, imbh::AnyValue)>> {
    if !body.starts_with('{') {
        return None;
    }
    match imbh::parse_json(body)? {
        imbh::AnyValue::Map(pairs) => Some(pairs),
        _ => None,
    }
}

/// Render a stored record as a logfmt line.
///
/// `ts=` and `level=` come first because the default remap script *lifts* them out of the body onto
/// the record — printing them here is what keeps that lossless, and doing it in a fixed order keeps
/// `docker logs` output scannable. `msg` leads the body's own fields for the same reason: it is what
/// a human reads first. (`message` is the fallback for records this driver did not produce.)
fn logfmt(row: &LogRow, fields: &[(String, imbh::AnyValue)]) -> String {
    let mut out = String::with_capacity(row.body.len() + 48);
    out.push_str("ts=");
    out.push_str(&rfc3339(row.time));
    out.push_str(" level=");
    out.push_str(
        row.severity_text
            .as_deref()
            .unwrap_or_else(|| band(row.severity_number.0)),
    );

    let ordered = fields
        .iter()
        .filter(|(key, _)| key == "msg")
        .chain(fields.iter().filter(|(key, _)| key == "message"))
        .chain(
            fields
                .iter()
                .filter(|(key, _)| key != "msg" && key != "message"),
        );
    for (key, value) in ordered {
        out.push(' ');
        out.push_str(key);
        out.push('=');
        out.push_str(&quoted(&render(value)));
    }
    out
}

/// The OTel severity band for a number, for a record stored without a `severity_text`.
fn band(number: u8) -> &'static str {
    match number {
        1..=4 => "TRACE",
        5..=8 => "DEBUG",
        9..=12 => "INFO",
        13..=16 => "WARN",
        17..=20 => "ERROR",
        21..=24 => "FATAL",
        _ => "UNSPECIFIED",
    }
}

/// One attribute value as logfmt text. Scalars render bare; a nested map or array renders as its
/// canonical JSON, which [`quoted`] then wraps — logfmt has no nesting, and the JSON is at least
/// exact and re-parseable.
fn render(value: &imbh::AnyValue) -> String {
    match value {
        imbh::AnyValue::Str(s) => s.clone(),
        imbh::AnyValue::Int(i) => i.to_string(),
        imbh::AnyValue::Double(d) => d.to_string(),
        imbh::AnyValue::Bool(b) => b.to_string(),
        imbh::AnyValue::Null => String::new(),
        other => imbh::canonical_json_value(other),
    }
}

/// Quote a logfmt value when it needs it.
///
/// Bare whenever the value is safe to read back — no whitespace, quote, backslash, `=` or control
/// character — and an empty value always quotes, because `k=` on its own reads as a flag rather than
/// as an empty string.
fn quoted(value: &str) -> String {
    let plain = !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\\' | '='));
    if plain {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Format epoch nanoseconds as RFC 3339 UTC — the inverse of [`timestamp`].
///
/// Hand-written for the same reason the parser is: a date-time crate is not worth carrying in a
/// footprint-gated graph for one field. It must also work in a `docker`-only build, which has no
/// chrono at all (that arrives with `docker-remap`).
fn rfc3339(time: Timestamp) -> String {
    let nanos = time.unix_nanos();
    // Floor-divide so pre-epoch instants land on the right second rather than one too late.
    let secs = nanos.div_euclid(1_000_000_000);
    let frac = nanos.rem_euclid(1_000_000_000);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let tod = secs.rem_euclid(86_400);
    let (hour, minute, second) = (tod / 3600, (tod / 60) % 60, tod % 60);
    match frac {
        0 => format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"),
        _ => format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{frac:09}Z"),
    }
}

/// The proleptic-Gregorian civil date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`,
/// the exact inverse of [`days_from_civil`]).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
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

    /// A row as the ingest path stores one, with `body` in whatever shape is under test.
    fn row_with(body: &str, severity: u8, text: Option<&str>, nanos: i64) -> LogRow {
        LogRow {
            time: Timestamp::from_unix_nanos(nanos),
            observed_time: None,
            severity_number: imbh::SeverityNumber(severity),
            severity_text: text.map(str::to_owned),
            service: Some("web".to_owned()),
            body: body.to_owned(),
            attributes: imbh::Attributes::new(),
            resource: imbh::Attributes::new(),
            scope: imbh::Attributes::new(),
            trace_id: None,
            span_id: None,
            flags: 0,
        }
    }

    fn rendered(body: &str, severity: u8, text: Option<&str>, nanos: i64) -> String {
        let entry = to_entry(&row_with(body, severity, text, nanos));
        String::from_utf8(entry.line).expect("utf-8 line")
    }

    /// The un-remapped path — and every OTLP-ingested record — must be untouched by the renderer.
    #[test]
    fn a_plain_text_body_goes_out_verbatim() {
        assert_eq!(
            rendered("starting server", 9, Some("INFO"), 0),
            "starting server\n"
        );
        // Text that merely starts with a brace but is not an object is still text.
        assert_eq!(rendered("{not json", 9, Some("INFO"), 0), "{not json\n");
        assert_eq!(rendered("", 9, Some("INFO"), 0), "\n");
    }

    #[test]
    fn a_structured_body_renders_as_logfmt_with_the_record_fields_first() {
        let line = rendered(
            r#"{"disk":"/dev/sda","msg":"disk low"}"#,
            13,
            Some("WARN"),
            1_700_000_000_000_000_000,
        );
        // `ts=` and `level=` lead because the remap script lifted them OUT of the body; `msg`
        // leads the body's own fields.
        assert_eq!(
            line,
            "ts=2023-11-14T22:13:20Z level=WARN msg=\"disk low\" disk=/dev/sda\n"
        );
    }

    #[test]
    fn a_record_without_severity_text_falls_back_to_the_otel_band() {
        let line = rendered(r#"{"msg":"x"}"#, 17, None, 0);
        assert!(
            line.starts_with("ts=1970-01-01T00:00:00Z level=ERROR "),
            "{line}"
        );
    }

    #[test]
    fn logfmt_values_are_quoted_only_when_they_need_it() {
        let line = rendered(
            r#"{"bare":"plain","spaced":"two words","quote":"a\"b","equals":"k=v","empty":"","nl":"a\nb"}"#,
            9,
            Some("INFO"),
            0,
        );
        assert!(line.contains(" bare=plain "), "{line}");
        assert!(line.contains(r#" spaced="two words" "#), "{line}");
        assert!(line.contains(r#" quote="a\"b" "#), "{line}");
        // `=` must quote, or the value would read as a second field.
        assert!(line.contains(r#" equals="k=v" "#), "{line}");
        // An empty value quotes, so it does not read as a bare flag.
        assert!(line.contains(r#" empty="" "#), "{line}");
        assert!(line.contains(r#" nl="a\nb""#), "{line}");
    }

    #[test]
    fn non_string_and_nested_values_render_usefully() {
        let line = rendered(
            r#"{"n":42,"f":1.5,"b":true,"nested":{"a":1},"list":[1,2]}"#,
            9,
            Some("INFO"),
            0,
        );
        assert!(line.contains(" n=42 "), "{line}");
        assert!(line.contains(" f=1.5 "), "{line}");
        assert!(line.contains(" b=true "), "{line}");
        // logfmt has no nesting, so a nested value renders as exact, re-parseable JSON. Quoting is
        // driven by the characters present, not by the value's shape: an object needs it (the JSON
        // carries `"`), a flat array does not.
        assert!(line.contains(r#" nested="{\"a\":1}" "#), "{line}");
        assert!(line.contains(" list=[1,2]"), "{line}");
    }

    #[test]
    fn message_is_the_fallback_lead_field_for_records_this_driver_did_not_produce() {
        let line = rendered(r#"{"z":"1","message":"hello"}"#, 9, Some("INFO"), 0);
        assert!(line.contains("level=INFO message=hello z=1"), "{line}");
    }

    /// The formatter is the exact inverse of the parser this module already had, so anything
    /// `docker logs` prints can be fed back to `--since`.
    #[test]
    fn the_rfc3339_formatter_round_trips_against_the_parser() {
        for nanos in [
            0i64,
            1,
            1_000_000_000,
            123_456_789,
            1_700_000_000_000_000_000,
            1_700_000_000_123_456_789,
            // Pre-epoch: floor division must land on the right second, not one too late.
            -1_000_000_000,
            -86_400_000_000_000,
        ] {
            let text = rfc3339(Timestamp::from_unix_nanos(nanos));
            assert_eq!(
                timestamp(&text).map(|t| t.unix_nanos()),
                Some(nanos),
                "{nanos} rendered as {text}"
            );
        }
    }

    #[test]
    fn civil_from_days_inverts_days_from_civil() {
        for (y, m, d) in [
            (1970, 1, 1),
            (1969, 12, 31),
            (2000, 2, 29),
            (2026, 8, 6),
            (2400, 12, 31),
        ] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
    }
}
