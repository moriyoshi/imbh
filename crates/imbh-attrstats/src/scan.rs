//! Reading the attribute columns out of an existing segment.
//!
//! Nothing here writes: a segment is opened read-only, only the attribute columns are projected
//! (Parquet is columnar, so the body/metric/timestamp columns are never touched), and the file is
//! left exactly as found. No sidecar, no manifest edit, no new column — the whole point is that this
//! measurement runs against a database that already exists.
//!
//! ## Why this walks rows in order
//!
//! A promoted column is a Parquet dictionary **plus an `Int32` index array, one entry per row**, and
//! the index array's compressed size depends on the *entropy of the value sequence within a
//! segment* — not on how many distinct values there are. Two keys with the same distinct count and
//! the same per-segment postings can differ several-fold on disk purely by whether their values
//! arrive in runs or interleaved (measured: 9,079 B against 64,252 B for the same session population
//! contiguous vs interleaved — `examples/bench --bin archetype-bench`).
//!
//! So the scan counts **runs**: how many times a key's value differs from the previous row's. That
//! needs row order, which is why the dictionary path below walks rows instead of just tallying
//! per-dictionary-entry counts as it used to. `prev` carries across batches, since row order
//! continues across them within one segment.

use std::fs::File;
use std::path::Path;

use arrow::array::{Array, DictionaryArray, StringArray};
use arrow::datatypes::{DataType, Int32Type};
use imbh_core::{AnyValue, Attributes};
use parquet::arrow::ProjectionMask;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use crate::accum::{Acc, AttrScope, value_text};

/// Fold one segment's attribute columns into every sink. Each sink must already have had
/// `begin_segment()` called for this segment.
///
/// The `resource`/`scope` columns are `Dictionary(Int32, Utf8)`, so their distinct blobs are parsed
/// at most once each and reused from a cache as row order revisits them. The plain-`Utf8`
/// `attributes` column coalesces consecutive equal blobs, which is the common shape for metric
/// segments sorted by time within a series.
pub fn scan_segment(
    path: &Path,
    scopes: &[AttrScope],
    batch_size: usize,
    sinks: &mut [&mut Acc],
) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let mask = ProjectionMask::columns(builder.parquet_schema(), scopes.iter().map(|s| s.column()));
    let reader = builder
        .with_projection(mask)
        .with_batch_size(batch_size)
        .build()?;

    let mut name = String::new();
    // One "previous row's attributes" per scope, carried across batches so a run is not falsely
    // restarted at every batch boundary.
    let mut prev: Vec<Option<Attributes>> = vec![None; scopes.len()];
    for batch in reader {
        let batch = batch?;
        let rows = batch.num_rows() as u64;
        for sink in sinks.iter_mut() {
            sink.add_rows(rows);
        }
        for (slot, &scope) in scopes.iter().enumerate() {
            let Some(column) = batch.column_by_name(scope.column()) else {
                continue;
            };
            match column.data_type() {
                DataType::Utf8 => {
                    let array = column
                        .as_any()
                        .downcast_ref::<StringArray>()
                        .ok_or("attribute column is Utf8 but not a StringArray")?;
                    scan_utf8(array, scope, sinks, &mut name, &mut prev[slot]);
                }
                DataType::Dictionary(_, _) => {
                    let dict = column
                        .as_any()
                        .downcast_ref::<DictionaryArray<Int32Type>>()
                        .ok_or("attribute column is a dictionary with non-Int32 keys")?;
                    let values = dict
                        .values()
                        .as_any()
                        .downcast_ref::<StringArray>()
                        .ok_or("attribute dictionary values are not UTF-8")?;
                    scan_dictionary(dict, values, scope, sinks, &mut name, &mut prev[slot]);
                }
                // A schema this tool does not know how to read is skipped rather than guessed at;
                // the caller reports the count so a silent gap is impossible.
                _ => {
                    return Err(format!(
                        "column {} has unexpected type {:?}",
                        scope.column(),
                        column.data_type()
                    )
                    .into());
                }
            }
        }
    }
    Ok(())
}

fn scan_utf8(
    array: &StringArray,
    scope: AttrScope,
    sinks: &mut [&mut Acc],
    name: &mut String,
    prev: &mut Option<Attributes>,
) {
    let mut run_start: Option<&str> = None;
    let mut run: u64 = 0;
    for i in 0..array.len() {
        if array.is_null(i) {
            if let Some(p) = run_start.take() {
                apply(p, scope, sinks, name, run, prev);
            }
            run = 0;
            continue;
        }
        let s = array.value(i);
        match run_start {
            Some(p) if p == s => run += 1,
            Some(p) => {
                apply(p, scope, sinks, name, run, prev);
                run_start = Some(s);
                run = 1;
            }
            None => {
                run_start = Some(s);
                run = 1;
            }
        }
    }
    if let Some(p) = run_start {
        apply(p, scope, sinks, name, run, prev);
    }
}

/// Walk the dictionary column **in row order**, coalescing consecutive equal indices into runs.
///
/// The previous version tallied a count per dictionary entry and applied each entry once, which is
/// cheaper but discards order — and order is exactly what the run statistic needs. Parses are cached
/// per dictionary index, so a value revisited later in the column is still parsed only once.
fn scan_dictionary(
    dict: &DictionaryArray<Int32Type>,
    values: &StringArray,
    scope: AttrScope,
    sinks: &mut [&mut Acc],
    name: &mut String,
    prev: &mut Option<Attributes>,
) {
    let keys = dict.keys();
    let mut run_idx: Option<usize> = None;
    let mut run: u64 = 0;
    let mut flush = |idx: usize, run: u64, prev: &mut Option<Attributes>| {
        if run > 0 && idx < values.len() && values.is_valid(idx) {
            apply(values.value(idx), scope, sinks, name, run, prev);
        }
    };
    for i in 0..keys.len() {
        if !keys.is_valid(i) {
            if let Some(idx) = run_idx.take() {
                flush(idx, run, prev);
            }
            run = 0;
            continue;
        }
        let idx = keys.value(i) as usize;
        match run_idx {
            Some(p) if p == idx => run += 1,
            Some(p) => {
                flush(p, run, prev);
                run_idx = Some(idx);
                run = 1;
            }
            None => {
                run_idx = Some(idx);
                run = 1;
            }
        }
    }
    if let Some(idx) = run_idx {
        flush(idx, run, prev);
    }
}

/// Parse one canonical-JSON attribute blob and fold each of its pairs into every sink, `count` times.
///
/// The parser is `imbh_core::Attributes::from_canonical_json`, which is the same
/// `imbh_core::json::parse_object` that `json_get` (and therefore `lookup_promoted` and the
/// `json_get_str` UDF) uses — so "the key is present with a string value" means here exactly what it
/// means to the query layer.
///
/// `prev` is the previous row's attributes. A key starts a **new run** when its value differs from
/// what that same key held on the previous row — which is not the same as the blob changing: two
/// different blobs can agree on any given key, and a key absent from `prev` starts a run too.
fn apply(
    json: &str,
    scope: AttrScope,
    sinks: &mut [&mut Acc],
    name: &mut String,
    count: u64,
    prev: &mut Option<Attributes>,
) {
    let attrs = Attributes::from_canonical_json(json);
    for (key, value) in attrs.iter() {
        let new_run = prev
            .as_ref()
            .is_none_or(|p| p.get(key) != Some::<&AnyValue>(value));
        name.clear();
        name.push_str(scope.prefix());
        name.push_str(key);
        let text = value_text(value);
        for sink in sinks.iter_mut() {
            sink.observe(scope, name, value, &text, count, new_run);
        }
    }
    *prev = Some(attrs);
}
