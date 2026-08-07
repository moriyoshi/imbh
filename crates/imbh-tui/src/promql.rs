//! Building and reading back PromQL: the queries the catalog emits, and recovering a metric name
//! from a query for the exemplar lookup.
//!
//! Translation itself is not here: a query's *meaning* depends on the metric catalog's recorded kinds
//! and temporality, so it happens where the data is (`imbh_head::exec::metric_context`) rather than
//! against a copy of the catalog a head would have to keep fresh.

use crate::model::MetricDetail;
use crate::syntax::is_ident_char;

/// Best-effort OTLP metric name for an exemplar lookup: the series' `__name__` label when present (a
/// bare selector keeps it), else the first non-function identifier in the PromQL query (covers
/// `rate(name[..])`/`sum(name)` where PromQL drops `__name__`).
pub(crate) fn metric_name_from_detail(detail: &MetricDetail) -> Option<String> {
    for pair in detail.labels.split(',') {
        if let Some(value) = pair.strip_prefix("__name__=")
            && !value.is_empty()
        {
            return Some(value.to_owned());
        }
    }
    metric_ident_from_promql(&detail.query)
}

/// PromQL words that are never a metric selector: aggregation operators and the boolean/set keywords.
/// Encountering one means "keep scanning" — the metric name is elsewhere (e.g. inside `rate(…)`).
pub(crate) const PROMQL_RESERVED: &[&str] = &[
    "sum",
    "avg",
    "min",
    "max",
    "count",
    "count_values",
    "stddev",
    "stdvar",
    "group",
    "topk",
    "bottomk",
    "quantile",
    "and",
    "or",
    "unless",
    "bool",
    "offset",
    "atan2",
];

/// Grouping modifiers whose following `(labels…)` list must be skipped whole, so a grouping *label* is
/// never mistaken for the metric name.
pub(crate) const PROMQL_GROUPING: &[&str] = &[
    "by",
    "without",
    "on",
    "ignoring",
    "group_left",
    "group_right",
];

/// The metric selector in a PromQL string: the first identifier that is not an aggregation
/// operator/keyword, not a grouping label, and not a function call — e.g. `name_bucket` inside
/// `histogram_quantile(0.95, sum by (le) (rate(name_bucket[5m])))`. Best-effort; `None` if none found.
pub(crate) fn metric_ident_from_promql(query: &str) -> Option<String> {
    let bytes = query.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        if ch.is_ascii_alphabetic() || ch == '_' || ch == ':' {
            let start = i;
            while i < bytes.len() && is_ident_char(bytes[i] as char) {
                i += 1;
            }
            let ident = &query[start..i];
            // Peek past whitespace to see whether a call `(` or grouping list follows.
            let mut j = i;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            let next_paren = bytes.get(j).copied() == Some(b'(');
            if PROMQL_GROUPING.contains(&ident) {
                if next_paren {
                    i = skip_paren_group(bytes, j);
                }
                continue;
            }
            if PROMQL_RESERVED.contains(&ident) || next_paren {
                continue; // a keyword or a function call — the selector is further in
            }
            return Some(ident.to_owned());
        }
        i += 1;
    }
    None
}

/// The metric a catalog-built query visualizes, used to name its series when several queries are
/// shown together. PromQL aggregation drops `__name__` by design, so a `sum by (…)` or a
/// `histogram_quantile(…)` result cannot say which metric it came from and the series list would
/// show two selected metrics as indistinguishable rows.
///
/// The `_bucket` suffix on a histogram's selector is Prometheus' spelling for that metric's buckets,
/// not a metric in its own right, so it is trimmed back to the OTel name the catalog lists — which
/// is also the name the exemplar lookup needs.
pub(crate) fn query_metric_name(query: &str) -> Option<String> {
    let ident = metric_ident_from_promql(query)?;
    if query.trim_start().starts_with("histogram_quantile")
        && let Some(base) = ident.strip_suffix("_bucket")
    {
        return Some(base.to_owned());
    }
    Some(ident)
}

/// Return the index just past the `)` matching the `(` at `open`; the end of the slice if unbalanced.
pub(crate) fn skip_paren_group(bytes: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    i
}

/// Render checked `(label, value)` matchers as a PromQL selector suffix (`{a="1",b="2"}`, or `""`).
pub(crate) fn matcher_braces(matchers: &[(&str, &str)]) -> String {
    if matchers.is_empty() {
        return String::new();
    }
    let inner = matchers
        .iter()
        .map(|(label, value)| format!("{label}=\"{value}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{inner}}}")
}

/// Build the PromQL to visualize a metric of the given OTel kind, restricted to `matchers` and
/// optionally aggregated by `group_by`: gauges plot as-is (avg when grouped), cumulative sums as a
/// per-second rate, histograms as a p95 over the bucket rate.
///
/// `dimensions` are the metric's discovered label axes ([`crate::fetch::discover_dims`]) and matter
/// only to the histogram-without-`group_by` case. A gauge or sum selected whole keeps one series per
/// label set for free — the selector and `rate()` both carry every label through, `__name__`
/// included. A histogram cannot: its quantile is only expressible as an aggregation, so whatever the
/// `sum by (…)` list omits is summed away. Grouping by `le` alone therefore collapsed the whole
/// metric into a single anonymous `{}` series, which made several selected histograms
/// indistinguishable in the series list. Naming the metric's own axes (plus `__name__`) restores the
/// per-series, per-metric split the other kinds have.
pub(crate) fn build_metric_query(
    name: &str,
    kind: &str,
    matchers: &[(&str, &str)],
    group_by: Option<&str>,
    dimensions: &[&str],
) -> String {
    let braces = matcher_braces(matchers);
    match (kind, group_by) {
        ("histogram", Some(label)) => format!(
            "histogram_quantile(0.95, sum by ({label}, le) (rate({name}_bucket{braces}[5m])))"
        ),
        ("histogram", None) => {
            // `le` first (the bucket axis `histogram_quantile` consumes), then the identity and the
            // metric's own axes, so the resulting series read like a gauge's would.
            let mut labels = vec!["le", "__name__"];
            labels.extend(dimensions);
            format!(
                "histogram_quantile(0.95, sum by ({}) (rate({name}_bucket{braces}[5m])))",
                labels.join(", ")
            )
        }
        ("sum", Some(label)) => format!("sum by ({label}) (rate({name}{braces}[5m]))"),
        ("sum", None) => format!("rate({name}{braces}[5m])"),
        (_, Some(label)) => format!("avg by ({label}) ({name}{braces})"),
        (_, None) => format!("{name}{braces}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_metric_query_covers_kinds_matchers_and_group_by() {
        // Whole metric (no matchers, no group-by) reproduces the kind's base expression.
        assert_eq!(
            build_metric_query("temperature", "gauge", &[], None, &[]),
            "temperature"
        );
        assert_eq!(
            build_metric_query("reqs", "sum", &[], None, &[]),
            "rate(reqs[5m])"
        );
        // Group-by.
        assert_eq!(
            build_metric_query("cpu", "gauge", &[], Some("host"), &["host"]),
            "avg by (host) (cpu)"
        );
        assert_eq!(
            build_metric_query("lat", "histogram", &[], Some("service"), &["service"]),
            "histogram_quantile(0.95, sum by (service, le) (rate(lat_bucket[5m])))"
        );
        // Matchers combine across axes.
        assert_eq!(
            build_metric_query(
                "cpu",
                "gauge",
                &[("service", "cart"), ("host", "a")],
                None,
                &["service", "host"]
            ),
            "cpu{service=\"cart\",host=\"a\"}"
        );
    }

    /// A histogram selected *whole* must keep the same per-series, per-metric split a gauge or a sum
    /// gets for free. Its quantile is only expressible as an aggregation, so every label the
    /// `sum by (…)` list omits is summed away: grouping by `le` alone collapsed the metric to one
    /// anonymous `{}` series and made several selected histograms indistinguishable in the list.
    #[test]
    fn a_whole_histogram_keeps_its_identity_and_its_axes() {
        // No discovered axes: `__name__` alone still tells two selected histograms apart.
        assert_eq!(
            build_metric_query("lat", "histogram", &[], None, &[]),
            "histogram_quantile(0.95, sum by (le, __name__) (rate(lat_bucket[5m])))"
        );
        // With axes: one quantile series per label set, exactly as a gauge's bare selector gives.
        assert_eq!(
            build_metric_query("lat", "histogram", &[], None, &["route", "service"]),
            "histogram_quantile(0.95, sum by (le, __name__, route, service) \
             (rate(lat_bucket[5m])))"
        );
        // A checked value narrows through the matcher; the axes stay named, so the pinned value is
        // still visible on the resulting series' labels.
        assert_eq!(
            build_metric_query(
                "lat",
                "histogram",
                &[("route", "get")],
                None,
                &["route", "service"]
            ),
            "histogram_quantile(0.95, sum by (le, __name__, route, service) \
             (rate(lat_bucket{route=\"get\"}[5m])))"
        );
    }

    #[test]
    fn metric_ident_from_promql_finds_the_selector() {
        // The exact PromQL shapes `build_metric_query` emits from the catalog.
        assert_eq!(
            metric_ident_from_promql("up{a=\"1\"}").as_deref(),
            Some("up")
        );
        assert_eq!(
            metric_ident_from_promql("rate(http_requests_total{a=\"1\"}[5m])").as_deref(),
            Some("http_requests_total")
        );
        assert_eq!(
            metric_ident_from_promql("sum by (svc) (rate(errors[1m]))").as_deref(),
            Some("errors")
        );
        assert_eq!(
            metric_ident_from_promql("avg by (host) (cpu_usage)").as_deref(),
            Some("cpu_usage")
        );
        assert_eq!(
            metric_ident_from_promql(
                "histogram_quantile(0.95, sum by (le) (rate(latency_bucket[5m])))"
            )
            .as_deref(),
            Some("latency_bucket")
        );
        assert_eq!(metric_ident_from_promql("(((").as_deref(), None);
    }

    #[test]
    fn a_query_names_the_metric_it_visualizes() {
        assert_eq!(query_metric_name("cpu{host=\"a\"}").as_deref(), Some("cpu"));
        assert_eq!(
            query_metric_name("sum by (service) (rate(reqs[5m]))").as_deref(),
            Some("reqs")
        );
        // The `_bucket` suffix is Prometheus' spelling for a histogram's buckets, not a metric of
        // its own: the catalog (and the exemplar lookup) know it as `lat`.
        assert_eq!(
            query_metric_name(
                "histogram_quantile(0.95, sum by (le, __name__) (rate(lat_bucket[5m])))"
            )
            .as_deref(),
            Some("lat")
        );
        // ...and only there. A counter genuinely named `*_bucket` keeps its name.
        assert_eq!(
            query_metric_name("rate(token_bucket[5m])").as_deref(),
            Some("token_bucket")
        );
        assert_eq!(query_metric_name("((("), None);
    }

    #[test]
    fn metric_name_prefers_the_name_label_then_the_query() {
        let with_label = MetricDetail {
            labels: "__name__=req_total,service=api".to_owned(),
            query: "rate(other[5m])".to_owned(),
            points: Vec::new(),
        };
        assert_eq!(
            metric_name_from_detail(&with_label).as_deref(),
            Some("req_total")
        );
        let without_label = MetricDetail {
            labels: "service=api".to_owned(),
            query: "rate(bar[5m])".to_owned(),
            points: Vec::new(),
        };
        assert_eq!(
            metric_name_from_detail(&without_label).as_deref(),
            Some("bar")
        );
    }
}
