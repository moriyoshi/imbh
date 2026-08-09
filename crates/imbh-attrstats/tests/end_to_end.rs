//! The measurement against real, hand-built databases.
//!
//! Every test here seals actual Parquet segments through `imbh_storage::Storage` and then measures
//! them the way a caller would — through the manifest, the Parquet reader, and the public
//! [`analyze`] entry point — rather than driving the accumulator's own API (`accum`'s unit tests do
//! that). Fixtures are chosen so every asserted number is derivable by hand from the doc comment.

use imbh_attrstats::report::Report;
// The flattened JSON document is the `serde` feature's, so the assertions on it are too — the crate
// builds and measures without it.
#[cfg(feature = "serde")]
use imbh_attrstats::report::to_json;
use imbh_attrstats::{AttrScope, Options, analyze, promote_verdict, text};
use imbh_core::{
    AnyValue, Compression, LogRow, MemoryBudget, Retention, WalMode, canonical_json_object,
};
use imbh_storage::Storage;

fn attrs(pairs: &[(&str, &str)]) -> String {
    let owned: Vec<(String, AnyValue)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), AnyValue::Str((*v).to_owned())))
        .collect();
    canonical_json_object(&owned)
}

fn log_row(time: i64, attributes: String, resource: String) -> LogRow {
    LogRow {
        time_unix_nano: time,
        observed_time_unix_nano: None,
        service: Some("cart".to_owned()),
        severity_number: 9,
        severity_text: None,
        body: "hello".to_owned(),
        attributes,
        resource,
        scope: "{}".to_owned(),
        trace_id: None,
        span_id: None,
        flags: 0,
    }
}

/// Seal `segments` segments of `rows` log rows each, with the attributes `attrs_of(segment, row)`
/// gives, timestamped `base(segment) + row`.
fn build(
    dir: &std::path::Path,
    segments: i64,
    rows: i64,
    base: impl Fn(i64) -> i64,
    attrs_of: impl Fn(i64, i64) -> (String, String),
) {
    let storage = Storage::open(
        dir,
        Compression::default(),
        WalMode::Off,
        Retention::none(),
        MemoryBudget::default(),
    )
    .expect("open storage");
    for seg in 0..segments {
        let batch: Vec<LogRow> = (0..rows)
            .map(|r| {
                let (attributes, resource) = attrs_of(seg, r);
                log_row(base(seg) + r, attributes, resource)
            })
            .collect();
        storage.append_logs(batch);
        storage.seal().expect("seal");
    }
}

fn logs_unit(report: &Report) -> &imbh_attrstats::UnitReport {
    report.table("logs").expect("logs unit")
}

/// End-to-end over a real, hand-built four-segment database: the tool must walk the manifest,
/// read the Parquet attribute columns (plain `Utf8`) and the dictionary-encoded `resource`
/// column, and land on the sigma values the fixture was built to have.
///
/// By construction, across 4 sealed `logs` segments:
/// - `env=prod`            in all 4 -> sigma 1.00
/// - `pod=pod-<i>`         in 1 each -> sigma 0.25
/// - `resource:host.name`  2 distinct values, each in 2 segments -> sigma 0.50
#[test]
fn measures_a_four_segment_database_end_to_end() {
    let dir = tempfile::tempdir().expect("tempdir");
    build(
        dir.path(),
        4,
        5,
        |seg| seg * 1_000,
        |seg, _| {
            (
                attrs(&[("env", "prod"), ("pod", &format!("pod-{seg}"))]),
                attrs(&[("host.name", &format!("host-{}", seg % 2))]),
            )
        },
    );

    let report = analyze(dir.path(), &Options::default()).expect("analyze");
    let logs = logs_unit(&report);
    assert_eq!(logs.segments, 4, "one segment per seal");
    assert_eq!(logs.rows, 20);

    let key = |name: &str| {
        logs.key(name)
            .unwrap_or_else(|| panic!("{name} missing from the report"))
    };

    let pod = key("pod");
    let sigma = pod.sigma.as_ref().expect("pod sigma");
    assert_eq!(pod.distinct_est, 4.0, "one pod name per segment");
    assert_eq!(sigma.max, 0.25, "a value in 1 of 4 segments has sigma 0.25");
    assert_eq!(sigma.p50, 0.25);
    assert_eq!(sigma.mean, 0.25);
    assert_eq!(pod.postings_est, 4.0, "4 values x 1 segment each");
    assert_eq!(pod.rows_string, 20);

    let env = key("env");
    assert_eq!(env.distinct_est, 1.0);
    assert_eq!(env.sigma.as_ref().expect("env sigma").max, 1.0);
    assert_eq!(env.postings_est, 4.0, "1 value present in all 4 segments");

    // The dictionary-encoded `resource` column must be read, and its keys prefixed.
    let host = key("resource:host.name");
    assert_eq!(host.scope, AttrScope::Resource);
    assert_eq!(host.distinct_est, 2.0);
    assert_eq!(host.sigma.as_ref().expect("host sigma").mean, 0.5);
    assert_eq!(host.rows_string, 20);

    // Promotion is DB-wide and record-`attributes`-scoped: `host.name` lives in `resource`, so it
    // must NOT appear as a promotion candidate under its bare name.
    let promo: Vec<&str> = report
        .promotion_candidates()
        .iter()
        .map(|k| k.name.as_str())
        .collect();
    assert!(promo.contains(&"env"));
    assert!(promo.contains(&"pod"));
    assert!(!promo.iter().any(|k| k.contains("host.name")));

    // 5 rows per segment cannot exhibit the repetition the cheap verdict needs, so the classifier
    // declines to judge rather than blaming the key for the corpus.
    let rps = report.global.rows_per_segment();
    assert_eq!(rps, 5.0);
    assert_eq!(promote_verdict(key("env"), report.global.rows, rps), "-");
    // `env=prod` is in all 4 segments, so no index scale prunes it.
    assert_eq!(report.index_scale("env"), None);
    assert_eq!(report.pending_wal_frames, 0);
    assert!(report.segments_skipped.is_empty());
    assert!(report.is_exact(), "nothing near the caps");
    // The JSON view must be buildable and carry the same numbers.
    #[cfg(feature = "serde")]
    {
        let json = to_json(&report);
        assert_eq!(json["tables"][0]["label"], "logs");
    }
}

/// The window ladder end-to-end, through the manifest and real Parquet files rather than the
/// accumulator's own API.
///
/// 8 sealed `logs` segments, one every 30s. `env=prod` is in all of them; `pod` takes a fresh
/// value in each. Against a 60s / 120s ladder that pins every point of both curves:
///
/// | key | C(seg) | C(60s) | C(120s) | C(all) | loc |
/// |-----|--------|--------|---------|--------|-----|
/// | env | 1      | 1      | 1       | 1      | 1.0 |
/// | pod | 1      | 2      | 4       | 8      | 8.0 |
///
/// `env` and `pod` have the *same* per-segment cardinality, so C(seg) alone cannot tell them
/// apart — which is the whole reason the ladder exists.
#[test]
fn the_cardinality_curve_separates_a_flat_key_from_a_localized_one() {
    const SEC: i64 = 1_000_000_000;
    let dir = tempfile::tempdir().expect("tempdir");
    build(
        dir.path(),
        8,
        5,
        |seg| seg * 30 * SEC,
        |seg, _| {
            (
                attrs(&[("env", "prod"), ("pod", &format!("pod-{seg}"))]),
                attrs(&[("host.name", "host-a")]),
            )
        },
    );

    let options = Options {
        scopes: vec![AttrScope::Attributes],
        ..Options::default()
    }
    .with_window_spec("60s,120s")
    .expect("ladder");
    let report = analyze(dir.path(), &options).expect("analyze");
    let logs = logs_unit(&report);
    assert_eq!(logs.segments, 8);
    assert_eq!(logs.windows, vec![4, 2], "240s span = 4 x 60s = 2 x 120s");
    assert_eq!(logs.span_nanos, 7 * 30 * SEC + 4);

    let key = |name: &str| logs.key(name).expect(name);
    let env = key("env");
    assert_eq!(env.c_segment, Some(1.0));
    assert_eq!(env.curve, vec![Some(1.0), Some(1.0)]);
    assert_eq!(env.distinct_est, 1.0);
    assert_eq!(env.locality(), Some(1.0), "flat: nothing prunes");

    let pod = key("pod");
    assert_eq!(pod.c_segment, Some(1.0), "same per-segment count as env");
    assert_eq!(
        pod.curve,
        vec![Some(2.0), Some(4.0)],
        "grows with the window"
    );
    assert_eq!(pod.distinct_est, 8.0);
    assert_eq!(pod.locality(), Some(8.0), "localized: pruning removes 7/8");

    // 5 rows per segment, one posting per segment, so repetition is 5 for both keys.
    assert_eq!(pod.repetition, 5.0);
    assert_eq!(env.repetition, 5.0);

    // The curve must survive into the JSON view, innermost to outermost, with both endpoints.
    #[cfg(feature = "serde")]
    {
        let json = to_json(&report);
        let curve = &json["tables"][0]["keys"]
            .as_array()
            .unwrap()
            .iter()
            .find(|k| k["key"] == "pod")
            .expect("pod in json")["cardinality_curve"];
        let windows: Vec<&str> = curve
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["window"].as_str().unwrap())
            .collect();
        assert_eq!(windows, vec!["segment", "60s", "120s", "all"]);
        assert_eq!(curve[3]["distinct_values"], 8.0);
    }

    // The text renderer must produce every section, ladder on and off, without panicking.
    let rendered = text::render(&report, 5);
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("C(60s)") && line.contains("C(all)")),
        "the ladder is a column per rung: {rendered:#?}"
    );
    let flat = Options {
        scopes: vec![AttrScope::Attributes],
        windows: Vec::new(),
        ..Options::default()
    };
    let no_ladder = text::render(&analyze(dir.path(), &flat).expect("analyze"), 5);
    assert!(
        no_ladder.iter().any(|line| line.contains("Disabled")),
        "a disabled ladder says so rather than printing an empty curve"
    );
}

/// **The promotion verdict gates on in-segment repetition, not on global cardinality** — the
/// correction the `promote-cost` bench forced, pinned as behaviour.
///
/// Three keys over 6 segments x 200 rows, chosen so that cardinality and cost disagree:
///
/// | key      | distinct | postings | repetition | verdict  |
/// |----------|----------|----------|------------|----------|
/// | `env`    | 1        | 6        | 200        | `yes`    |
/// | `pod`    | 6        | 6        | 200        | `yes`    |
/// | `req_id` | 1,200    | 1,200    | 1          | `costly` |
///
/// `env` and `pod` differ 6x in global cardinality and cost **exactly the same** — one value on
/// 200 rows within any given segment, either way. `pod` is the case that matters: its values
/// never recur across segments, so a global-distinct gate scaled to production (`pod.name` over
/// weeks) would reject it, yet it is precisely the cheap case. `req_id` is expensive not because
/// its cardinality is high but because its values never repeat *within* a segment either.
///
/// The index verdict runs the other way: `pod` is confined to one segment in six, so pruning
/// pays; `env` is everywhere, so it never does.
#[test]
fn the_promotion_verdict_follows_repetition_not_cardinality() {
    let dir = tempfile::tempdir().expect("tempdir");
    build(
        dir.path(),
        6,
        200,
        |seg| seg * 10_000,
        |seg, r| {
            (
                attrs(&[
                    ("env", "prod"),
                    ("pod", &format!("pod-{seg}")),
                    ("req_id", &format!("r-{seg}-{r}")),
                ]),
                attrs(&[("host.name", "host-a")]),
            )
        },
    );
    let options = Options {
        scopes: vec![AttrScope::Attributes],
        ..Options::default()
    };
    let report = analyze(dir.path(), &options).expect("analyze");
    let rps = report.global.rows_per_segment();
    assert_eq!(rps, 200.0);
    let key = |name: &str| report.global.key(name).expect(name);

    assert_eq!(key("env").distinct_est, 1.0);
    assert_eq!(key("pod").distinct_est, 6.0);
    assert_eq!(key("req_id").distinct_est, 1_200.0);

    // 1,200 rows over 6 postings either way: `env` is one value in six segments, `pod` is six
    // values in one segment each. Identical cost, 6x apart in cardinality.
    assert_eq!(key("env").postings_est, 6.0);
    assert_eq!(key("pod").postings_est, 6.0);
    assert_eq!(key("env").repetition, 200.0);
    assert_eq!(key("pod").repetition, 200.0);
    assert_eq!(key("req_id").repetition, 1.0);

    assert_eq!(report.promote_verdict(key("env")), "yes");
    assert_eq!(
        report.promote_verdict(key("pod")),
        "yes",
        "a fresh value per segment is the CHEAP case, whatever its global cardinality"
    );
    assert_eq!(
        report.promote_verdict(key("req_id")),
        "costly",
        "unique per row is the +108 KB/key regime"
    );

    // The index verdict is independent and points the other way for `pod`.
    assert_eq!(
        report.index_scale("env"),
        None,
        "in every segment: no pruning"
    );
    assert_eq!(
        report.index_scale("pod").as_deref(),
        Some("all"),
        "one segment in six: sigma 0.167, pruning pays at every scale"
    );
}

/// The scope option must restrict the scan to the one column `promote` covers.
#[test]
fn the_scope_option_restricts_to_record_attributes() {
    let dir = tempfile::tempdir().expect("tempdir");
    build(
        dir.path(),
        1,
        1,
        |_| 1,
        |_, _| (attrs(&[("env", "prod")]), attrs(&[("host.name", "host-a")])),
    );
    let options = Options {
        scopes: vec![AttrScope::Attributes],
        ..Options::default()
    };
    let report = analyze(dir.path(), &options).expect("analyze");
    let names: Vec<&str> = logs_unit(&report)
        .keys
        .iter()
        .map(|k| k.name.as_str())
        .collect();
    assert_eq!(names, vec!["env"]);
}

/// The range filter is part of the measurement, not a shortcut: sigma is defined over "the segments
/// in a time range", so restricting the range changes the denominator — and it is what bounds the
/// work when a head asks for statistics over the window it is displaying.
///
/// 4 segments, one per 1,000ns bucket, each carrying its own `pod` value. A range covering the last
/// two must see 2 segments and 2 pod values, not 4.
#[test]
fn a_range_narrows_the_segment_set_and_the_sigma_denominator() {
    let dir = tempfile::tempdir().expect("tempdir");
    build(
        dir.path(),
        4,
        5,
        |seg| seg * 1_000,
        |seg, _| {
            (
                attrs(&[("env", "prod"), ("pod", &format!("pod-{seg}"))]),
                attrs(&[("host.name", "host-a")]),
            )
        },
    );

    let options = Options {
        scopes: vec![AttrScope::Attributes],
        range: Some((2_000, i64::MAX)),
        ..Options::default()
    };
    let report = analyze(dir.path(), &options).expect("analyze");
    let logs = logs_unit(&report);
    assert_eq!(logs.segments, 2, "segments 2 and 3 overlap [2000, ..]");
    assert_eq!(logs.rows, 10);
    let pod = logs.key("pod").expect("pod");
    assert_eq!(
        pod.distinct_est, 2.0,
        "only the values in range are counted"
    );
    assert_eq!(
        pod.sigma.as_ref().expect("sigma").mean,
        0.5,
        "1 of the 2 in-range segments, not 1 of 4"
    );
    assert_eq!(report.range, Some((2_000, i64::MAX)));
    // The header must say the scan was restricted, or the numbers read as whole-database ones.
    let rendered = text::render(&report, 5);
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("segments overlapping")),
        "{rendered:#?}"
    );

    // A range that matches nothing is an empty measurement, not an error.
    let empty = analyze(
        dir.path(),
        &Options {
            range: Some((i64::MAX - 1, i64::MAX)),
            ..Options::default()
        },
    )
    .expect("analyze");
    assert_eq!(empty.global.segments, 0);
    assert!(empty.global.keys.is_empty());
    assert!(!text::render(&empty, 5).is_empty(), "still renders");
}

/// A path that is not a directory is a failure, not an empty report.
///
/// `read_disk_snapshot` answers "no segments" for a directory with no manifest, which is correct for
/// a database that has not sealed one yet. For a mistyped path it is indistinguishable from a
/// database with no attributes — a wrong answer that reads like a measurement — so `analyze` refuses
/// it up front.
#[test]
fn a_missing_database_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(analyze(dir.path().join("nope"), &Options::default()).is_err());
    // An existing directory with no manifest stays a legitimate empty measurement: that is what a
    // database looks like before its first seal.
    let report = analyze(dir.path(), &Options::default()).expect("an empty directory measures");
    assert_eq!(report.global.segments, 0);
}
