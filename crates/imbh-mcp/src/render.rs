//! The two JSON serializers the MCP tools and `imbhd`'s plain HTTP endpoints share.
//!
//! [`batches_to_json`] backs both the `query_sql` tool and `POST /api/query`; [`stats_json`] backs
//! both the `db_stats` tool and `GET /stats`. They live here, below both callers, so the tool
//! surface and the HTTP surface can never describe the same rows — or the same database — two
//! different ways. Both write JSON text by hand rather than through `serde_json`: the row serializer
//! streams straight out of arrow's value formatter with no intermediate `Value` per cell, and
//! `DbStats` is a fixed shape with no derive to hang a `Serialize` on.

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

/// Serialize [`imbh::DbStats`]: per-table segment/row/buffer counts and time span, plus buffer
/// bytes, WAL bytes, and the durable LSN.
pub fn stats_json(stats: &imbh::DbStats) -> String {
    let opt = |v: Option<i64>| v.map_or("null".to_owned(), |n| n.to_string());
    let mut tables = String::from("[");
    for (i, t) in stats.tables.iter().enumerate() {
        if i > 0 {
            tables.push(',');
        }
        use std::fmt::Write as _;
        let _ = write!(
            tables,
            "{{\"table\":{},\"segment_count\":{},\"segment_rows\":{},\"buffer_rows\":{},\
             \"min_time_unix_nano\":{},\"max_time_unix_nano\":{}}}",
            json_string(t.table.as_str()),
            t.segment_count,
            t.segment_rows,
            t.buffer_rows,
            opt(t.min_time_unix_nano),
            opt(t.max_time_unix_nano),
        );
    }
    tables.push(']');
    format!(
        "{{\"buffer_bytes\":{},\"wal_bytes\":{},\"durable_lsn\":{},\"tables\":{}}}",
        stats.buffer_bytes,
        stats.wal_bytes,
        stats.durable_lsn.map_or(0, |l| l.get()),
        tables,
    )
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
