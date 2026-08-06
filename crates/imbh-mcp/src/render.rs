//! The two JSON serializers the MCP tools and `imbhd`'s plain HTTP endpoints share.
//!
//! [`batches_to_json`] backs both the `query_sql` tool and `POST /api/query`; [`stats_json`] backs
//! both the `db_stats` tool and `GET /stats`. They live here, below both callers, so the tool
//! surface and the HTTP surface can never describe the same rows — or the same database — two
//! different ways.
//!
//! [`batches_to_json`] writes JSON text by hand rather than through `serde_json`: it streams
//! straight out of arrow's value formatter with no intermediate `Value` per cell.
//!
//! [`stats_json`] used to as well — `imbh::DbStats` carries no derive — but the head API already
//! needed a typed, round-trippable mirror of it (`imbh_head::dto::Stats`), and two serializers for
//! one struct is one too many: the hand-written one silently omitted the ingest gauges and spelled a
//! `None` durable LSN as `0`. It now converts and defers to that derive, so all three surfaces
//! (`GET /stats`, `db_stats`, `GET /api/head/stats`) emit exactly one shape.

use imbh::arrow::array::Array;
use imbh::arrow::record_batch::RecordBatch;
use imbh::arrow::util::display::{ArrayFormatter, FormatOptions};

/// Serialize result batches into a JSON array of row objects. Numeric columns render as JSON
/// numbers; everything else as JSON strings (via arrow's value formatter); nulls as `null`.
pub fn batches_to_json(batches: &[RecordBatch]) -> Vec<u8> {
    let mut out = String::from("[");
    let opts = FormatOptions::default();
    let mut first_row = true;
    for batch in batches {
        let names: Vec<String> = batch
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        // A column whose type arrow can't build a formatter for renders as `null` rather than
        // panicking the connection (`.ok()` instead of `.expect(...)`). Every type imbh emits is
        // supported, so this is defensive.
        let formatters: Vec<Option<ArrayFormatter>> = batch
            .columns()
            .iter()
            .map(|c| ArrayFormatter::try_new(c, &opts).ok())
            .collect();
        for row in 0..batch.num_rows() {
            if !first_row {
                out.push(',');
            }
            first_row = false;
            out.push('{');
            for (col, name) in names.iter().enumerate() {
                if col > 0 {
                    out.push(',');
                }
                out.push_str(&json_string(name));
                out.push(':');
                let array = batch.column(col);
                match formatters[col].as_ref() {
                    Some(f) if !array.is_null(row) => {
                        let value = f.value(row).to_string();
                        if is_numeric(array.data_type()) {
                            out.push_str(&value);
                        } else {
                            out.push_str(&json_string(&value));
                        }
                    }
                    _ => out.push_str("null"),
                }
            }
            out.push('}');
        }
    }
    out.push(']');
    out.into_bytes()
}

fn is_numeric(dt: &imbh::arrow::datatypes::DataType) -> bool {
    use imbh::arrow::datatypes::DataType::*;
    matches!(
        dt,
        Int8 | Int16 | Int32 | Int64 | UInt8 | UInt16 | UInt32 | UInt64 | Float32 | Float64
    )
}

/// Serialize [`imbh::DbStats`] as [`imbh_head::dto::Stats`]: per-table segment/row/buffer counts and
/// time span, plus buffer bytes, WAL bytes, the durable LSN, and the four ingest gauges. The output
/// deserializes back into that type, which is what the `db_stats` tool and any consumer of
/// `GET /stats` get to rely on.
///
/// A `None` durable LSN is `null`, not `0`: zero is not a legal LSN (`imbh::Lsn` is a
/// `NonZero<u64>`), so the old spelling was indistinguishable from a real value in a typed reader.
pub fn stats_json(stats: &imbh::DbStats) -> String {
    // Infallible: `Stats` is numbers, strings and `Option`s of them — no map with non-string keys,
    // no non-finite float, and no `Serialize` impl of ours that can fail.
    serde_json::to_string(&imbh_head::dto::Stats::from(stats))
        .expect("imbh_head::dto::Stats cannot fail to serialize")
}

/// JSON-quote and escape a string.
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use imbh::{DbStats, Lsn, Table, TableStats};
    use imbh_head::dto;

    use super::stats_json;

    /// A `DbStats` with every gauge set to a distinct value, so a serializer that crossed two of
    /// them (or dropped one) cannot pass.
    fn sample(durable_lsn: Option<Lsn>) -> DbStats {
        DbStats {
            tables: vec![TableStats {
                table: Table::Logs,
                segment_count: 2,
                segment_rows: 30,
                buffer_rows: 4,
                min_time_unix_nano: None,
                max_time_unix_nano: Some(9),
            }],
            buffer_bytes: 1024,
            wal_bytes: 64,
            durable_lsn,
            ingest_queue_depth: 3,
            ingest_dropped: 5,
            ingest_errors: 7,
            ingest_rejected: 11,
        }
    }

    #[test]
    fn the_ingest_gauges_reach_the_wire() {
        // They were missing entirely: an operator watching `/stats` could not see a queue backing up
        // or a `DropOldest` eviction, both of which are silent data loss from the caller's side.
        let json = stats_json(&sample(Lsn::new(42)));
        for field in [
            "\"ingest_queue_depth\":3",
            "\"ingest_dropped\":5",
            "\"ingest_errors\":7",
            "\"ingest_rejected\":11",
        ] {
            assert!(json.contains(field), "{field} missing from {json}");
        }

        let back: dto::Stats = serde_json::from_str(&json).expect("round-trip");
        assert_eq!(back.ingest_queue_depth, 3);
        assert_eq!(back.ingest_dropped, 5);
        assert_eq!(back.ingest_errors, 7);
        assert_eq!(back.ingest_rejected, 11);
        // And the rest of the document is what it always was.
        assert_eq!(back.buffer_bytes, 1024);
        assert_eq!(back.wal_bytes, 64);
        assert_eq!(back.durable_lsn, Some(42));
        assert_eq!(back.tables.len(), 1);
        assert_eq!(back.tables[0].table, "logs");
        assert_eq!(back.tables[0].segment_count, 2);
        assert_eq!(back.tables[0].segment_rows, 30);
        assert_eq!(back.tables[0].buffer_rows, 4);
        assert_eq!(back.tables[0].min_time_unix_nano, None);
        assert_eq!(back.tables[0].max_time_unix_nano, Some(9));
    }

    #[test]
    fn nothing_durable_is_null_not_zero() {
        // `0` is not a legal LSN (`Lsn` is a `NonZero<u64>`), so the old spelling could not be told
        // apart from a real watermark by a typed reader.
        let json = stats_json(&sample(None));
        assert!(json.contains("\"durable_lsn\":null"), "got {json}");
        assert!(!json.contains("\"durable_lsn\":0"), "got {json}");
        assert_eq!(
            serde_json::from_str::<dto::Stats>(&json)
                .expect("round-trip")
                .durable_lsn,
            None
        );
        // An absent per-table bound is `null` too, and comes back as `None` — the field is present,
        // not omitted, which is what `/stats` has always emitted.
        assert!(json.contains("\"min_time_unix_nano\":null"), "got {json}");
    }

    #[test]
    fn the_output_is_the_typed_value() {
        // The whole point of routing through `dto::Stats`: `GET /stats`, the `db_stats` tool, and
        // `GET /api/head/stats` are one serializer, so a parse of one is byte-identical to the other.
        let stats = sample(Lsn::new(1));
        let back: dto::Stats = serde_json::from_str(&stats_json(&stats)).expect("round-trip");
        assert_eq!(back, dto::Stats::from(&stats));
        assert_eq!(
            serde_json::to_string(&back).expect("re-serialize"),
            stats_json(&stats)
        );
    }
}
