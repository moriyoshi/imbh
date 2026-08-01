//! Building and reading back PromQL: the queries the catalog emits, and recovering a metric name
//! from a query for the exemplar lookup.

use imbh_lgtm::{MetricKind, MetricResolution, TranslateContext};

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
/// per-second rate, histograms as a p95 over the bucket rate (aggregated by `le`).
pub(crate) fn build_metric_query(
    name: &str,
    kind: &str,
    matchers: &[(&str, &str)],
    group_by: Option<&str>,
) -> String {
    let braces = matcher_braces(matchers);
    match (kind, group_by) {
        ("histogram", Some(label)) => format!(
            "histogram_quantile(0.95, sum by ({label}, le) (rate({name}_bucket{braces}[5m])))"
        ),
        ("histogram", None) => {
            format!("histogram_quantile(0.95, sum by (le) (rate({name}_bucket{braces}[5m])))")
        }
        ("sum", Some(label)) => format!("sum by ({label}) (rate({name}{braces}[5m]))"),
        ("sum", None) => format!("rate({name}{braces}[5m])"),
        (_, Some(label)) => format!("avg by ({label}) ({name}{braces})"),
        (_, None) => format!("{name}{braces}"),
    }
}

/// The bare selector used to *discover* a metric's groupable dimensions: evaluated as an instant over
/// the metric's whole retained span, its returned series carry the full label set (data-point
/// attributes plus the resource `service`), which we read to build the tree. A plain selector (not a
/// rate) avoids depending on samples landing in a rate window.
pub(crate) fn discovery_promql(name: &str, kind: &str) -> String {
    match kind {
        "histogram" => format!("{name}_bucket"),
        _ => name.to_owned(),
    }
}

pub(crate) fn metric_context(catalog: &[imbh::MetricMeta]) -> TranslateContext {
    let mut metrics = Vec::new();
    for metric in catalog {
        let kind = match (metric.kind.as_str(), metric.temporality.as_deref()) {
            ("gauge", _) => Some(MetricKind::Gauge),
            ("sum", Some(temporality)) if temporality.eq_ignore_ascii_case("cumulative") => {
                Some(MetricKind::CumulativeCounter)
            }
            ("histogram", Some(temporality)) if temporality.eq_ignore_ascii_case("cumulative") => {
                Some(MetricKind::CumulativeHistogram)
            }
            _ => None,
        };
        let Some(kind) = kind else {
            continue;
        };
        metrics.push(MetricResolution {
            query_name: metric.metric.clone(),
            storage_name: metric.metric.clone(),
            kind,
        });
        if kind == MetricKind::CumulativeHistogram {
            metrics.push(MetricResolution {
                query_name: format!("{}_bucket", metric.metric),
                storage_name: metric.metric.clone(),
                kind,
            });
        }
    }
    TranslateContext { metrics }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_metric_query_covers_kinds_matchers_and_group_by() {
        // Whole metric (no matchers, no group-by) reproduces the kind's base expression.
        assert_eq!(
            build_metric_query("temperature", "gauge", &[], None),
            "temperature"
        );
        assert_eq!(
            build_metric_query("reqs", "sum", &[], None),
            "rate(reqs[5m])"
        );
        // Group-by.
        assert_eq!(
            build_metric_query("cpu", "gauge", &[], Some("host")),
            "avg by (host) (cpu)"
        );
        assert_eq!(
            build_metric_query("lat", "histogram", &[], Some("service")),
            "histogram_quantile(0.95, sum by (service, le) (rate(lat_bucket[5m])))"
        );
        // Matchers combine across axes.
        assert_eq!(
            build_metric_query("cpu", "gauge", &[("service", "cart"), ("host", "a")], None),
            "cpu{service=\"cart\",host=\"a\"}"
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
