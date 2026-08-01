//! Time formatting and parsing.
//!
//! Hand-rolled to avoid pulling a datetime crate into the terminal graph (footprint is a first-class
//! constraint); the civil-date conversions are Howard Hinnant's algorithms.

/// Format nanoseconds-since-the-Unix-epoch as a UTC `YYYY-MM-DD HH:MM:SS.mmm` string. Hand-rolled to
/// avoid pulling a datetime crate into the terminal graph (footprint is a first-class constraint);
/// the civil-date conversion is Howard Hinnant's `civil_from_days` algorithm, valid for any i64.
pub(crate) fn format_timestamp_ns(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let millis = ns.rem_euclid(1_000_000_000) / 1_000_000;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hours, minutes, seconds) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

/// Just the `HH:MM:SS` (UTC) time-of-day — compact axis tick labels for the time-series viewer.
pub(crate) fn clock_hms_ns(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let tod = secs.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", tod / 3600, (tod % 3600) / 60, tod % 60)
}

/// Wall-clock `YYYY-MM-DD HH:MM:SS` (UTC), no sub-second part — used for the header clock.
pub(crate) fn format_datetime_ns(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hours, minutes, seconds) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}:{seconds:02}")
}

/// Parse a UTC `YYYY-MM-DD[ HH:MM[:SS]]` (space or `T` separator) into nanoseconds since the Unix
/// epoch. The time part is optional (missing minute/second default to 0), but every field is range-
/// checked; returns `None` on any malformed or out-of-range field so the form can report it. Public
/// so a host can build [`Options::window`](crate::model::Options::window) from the same textual
/// format the picker accepts.
pub fn parse_datetime(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, time) = match text.split_once([' ', 'T']) {
        Some((date, time)) => (date, time.trim()),
        None => (text, ""),
    };
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.trim().parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (mut hour, mut minute, mut second) = (0u32, 0u32, 0u32);
    if !time.is_empty() {
        let mut time_parts = time.split(':');
        hour = time_parts.next()?.parse().ok()?;
        if let Some(part) = time_parts.next() {
            minute = part.parse().ok()?;
        }
        if let Some(part) = time_parts.next() {
            second = part.parse().ok()?;
        }
        if time_parts.next().is_some() {
            return None;
        }
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let secs = days_from_civil(year, month, day)
        .checked_mul(86_400)?
        .checked_add(hour as i64 * 3600 + minute as i64 * 60 + second as i64)?;
    secs.checked_mul(1_000_000_000)
}

/// Inverse of [`civil_from_days`]: days since 1970-01-01 for a proleptic-Gregorian UTC date (Howard
/// Hinnant's algorithm, matching the constants used by `civil_from_days`).
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64; // Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + day as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Convert a count of days since 1970-01-01 into `(year, month, day)` (proleptic Gregorian, UTC).
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Format a nanosecond duration, scaling the unit so both a sub-microsecond span and a multi-second one
/// read naturally. `ascii` spells the microsecond unit `us` instead of `µs` (the `--ascii` guarantee).
pub(crate) fn format_duration_ns(ns: u64, ascii: bool) -> String {
    if ns < 1_000 {
        format!("{ns}ns")
    } else if ns < 1_000_000 {
        format!(
            "{:.3}{}",
            ns as f64 / 1_000.0,
            if ascii { "us" } else { "µs" }
        )
    } else if ns < 1_000_000_000 {
        format!("{:.3}ms", ns as f64 / 1_000_000.0)
    } else {
        format!("{:.3}s", ns as f64 / 1_000_000_000.0)
    }
}

/// Compact `Ns`/`Nm`/`Nh`/`Nd` rendering of a whole-second duration for the picker rows.
pub(crate) fn humanize_secs(secs: u64) -> String {
    if secs.is_multiple_of(86_400) {
        format!("{}d", secs / 86_400)
    } else if secs.is_multiple_of(3_600) {
        format!("{}h", secs / 3_600)
    } else if secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_format_as_utc() {
        // 0 ns is the Unix epoch; a known instant checks the civil-date math and sub-second field.
        assert_eq!(format_timestamp_ns(0), "1970-01-01 00:00:00.000");
        // 2021-01-01T00:00:00.123Z == 1_609_459_200 s.
        assert_eq!(
            format_timestamp_ns(1_609_459_200_123_000_000),
            "2021-01-01 00:00:00.123"
        );
        // Before the epoch must not panic and must borrow correctly across the day boundary.
        assert_eq!(format_timestamp_ns(-1), "1969-12-31 23:59:59.999");
    }

    #[test]
    fn header_clock_drops_the_sub_second_field() {
        // The header clock is the same civil-date math without the millis suffix.
        assert_eq!(format_datetime_ns(0), "1970-01-01 00:00:00");
        assert_eq!(
            format_datetime_ns(1_609_459_200_123_000_000),
            "2021-01-01 00:00:00"
        );
    }

    #[test]
    fn durations_scale_their_unit() {
        assert_eq!(format_duration_ns(900, false), "900ns");
        assert_eq!(format_duration_ns(1_500, false), "1.500µs");
        assert_eq!(format_duration_ns(1_500, true), "1.500us");
        assert_eq!(format_duration_ns(2_500_000, false), "2.500ms");
        assert_eq!(format_duration_ns(3_000_000_000, false), "3.000s");
    }

    #[test]
    fn clock_hms_formats_time_of_day() {
        assert_eq!(clock_hms_ns(0), "00:00:00");
        // 2021-01-01T00:00:00Z + 1h1m1s.
        assert_eq!(
            clock_hms_ns(1_609_459_200_000_000_000 + 3_661_000_000_000),
            "01:01:01"
        );
    }

    #[test]
    fn parse_datetime_round_trips_and_validates() {
        // Round-trips the header formatter for whole-second instants (epoch and a known date).
        for ns in [0i64, 1_609_459_200_000_000_000, 1_763_000_000_000_000_000] {
            assert_eq!(parse_datetime(&format_datetime_ns(ns)), Some(ns));
        }
        // The time part is optional (midnight) and `T` is accepted as the separator.
        assert_eq!(parse_datetime("1970-01-01"), Some(0));
        assert_eq!(
            parse_datetime("2021-01-01T00:00:00"),
            Some(1_609_459_200_000_000_000)
        );
        assert_eq!(
            parse_datetime("2021-01-01 00:01"),
            Some(1_609_459_260_000_000_000)
        );
        // Malformed or out-of-range fields are rejected.
        assert_eq!(parse_datetime(""), None);
        assert_eq!(parse_datetime("2021-13-01 00:00:00"), None); // month 13
        assert_eq!(parse_datetime("2021-01-01 24:00:00"), None); // hour 24
        assert_eq!(parse_datetime("2021-01-01 00:60:00"), None); // minute 60
        assert_eq!(parse_datetime("not-a-date"), None);
    }

    #[test]
    fn days_from_civil_inverts_civil_from_days() {
        for days in [-40_000i64, -719_468, -1, 0, 1, 18_628, 50_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }
}
