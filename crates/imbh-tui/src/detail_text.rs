//! The plain-text bodies of the detail views (log entry, span summary, span fields).

use crate::model::LogRecord;
use crate::time::{format_duration_ns, format_timestamp_ns};
use crate::ui::glyphs::Glyphs;
use crate::waterfall::SpanRecord;

/// The lines of the log-entry detail view (header fields, body, then attribute sections).
pub(crate) fn log_detail_lines(record: &LogRecord) -> Vec<String> {
    let mut lines = vec![
        format!("Time      {}", format_timestamp_ns(record.time_ns)),
        format!("Severity  {}", record.severity),
        format!("Service   {}", record.service.as_deref().unwrap_or("-")),
        format!(
            "Trace ID  {}",
            record.trace_id.as_deref().unwrap_or("(none)")
        ),
        format!(
            "Span ID   {}",
            record.span_id.as_deref().unwrap_or("(none)")
        ),
        String::new(),
        "Body".to_owned(),
    ];
    if record.body.is_empty() {
        lines.push("  (empty)".to_owned());
    } else {
        lines.extend(record.body.lines().map(|line| format!("  {line}")));
    }
    push_attr_section(&mut lines, "Attributes", &record.attributes);
    push_attr_section(&mut lines, "Resource", &record.resource);
    push_attr_section(&mut lines, "Scope", &record.scope);
    lines
}

/// Append a titled attribute section (`Title (n)` then `  key = value` rows) when non-empty.
pub(crate) fn push_attr_section(lines: &mut Vec<String>, title: &str, pairs: &[(String, String)]) {
    if pairs.is_empty() {
        return;
    }
    lines.push(String::new());
    lines.push(format!("{title} ({})", pairs.len()));
    for (key, value) in pairs {
        lines.push(format!("  {key} = {value}"));
    }
}

/// The compact summary of the span under the trace detail's cursor (the bottom pane).
pub(crate) fn span_summary_lines(span: &SpanRecord, g: &Glyphs) -> Vec<String> {
    let sep = g.sep;
    let mut status = span.status_code.clone();
    if let Some(message) = &span.status_message {
        status.push_str(&format!(" {sep} {message}"));
    }
    if span.malformed {
        status.push_str(&format!(" {sep} {} malformed parent chain", g.warn));
    }
    vec![
        format!(
            "Span    {} {sep} {} {sep} {}",
            span.name,
            span.service.as_deref().unwrap_or("(no service)"),
            if span.kind.is_empty() {
                "(no kind)"
            } else {
                &span.kind
            }
        ),
        format!(
            "IDs     {} {sep} parent {}",
            span.span_id,
            span.parent_span_id.as_deref().unwrap_or("(root)")
        ),
        format!(
            "Timing  +{} into trace {sep} {} long",
            format_duration_ns(span.offset_ns.max(0) as u64, g.ascii),
            format_duration_ns(span.duration_ns, g.ascii),
        ),
        format!("Status  {status}"),
        format!(
            "Fields  attrs {} {sep} resource {} {sep} scope {}{}{}",
            span.attributes.len(),
            span.resource.len(),
            span.scope.len(),
            if span.events.is_some() {
                format!(" {sep} events")
            } else {
                String::new()
            },
            if span.links.is_some() {
                format!(" {sep} links")
            } else {
                String::new()
            },
        ),
    ]
}

/// The lines of the span field detail: header fields, then the raw events/links JSON and the attribute
/// sections (the same layout as [`log_detail_lines`]).
pub(crate) fn span_detail_lines(trace_id: &str, span: &SpanRecord, g: &Glyphs) -> Vec<String> {
    let mut lines = vec![
        format!("Trace ID  {trace_id}"),
        format!("Span ID   {}", span.span_id),
        format!(
            "Parent    {}",
            span.parent_span_id.as_deref().unwrap_or("(root)")
        ),
        format!("Name      {}", span.name),
        format!("Service   {}", span.service.as_deref().unwrap_or("-")),
        format!(
            "Kind      {}",
            if span.kind.is_empty() {
                "-"
            } else {
                &span.kind
            }
        ),
        format!("Status    {}", span.status_code),
    ];
    if let Some(message) = &span.status_message {
        lines.push(format!("Message   {message}"));
    }
    lines.push(format!(
        "Start     {}",
        format_timestamp_ns(span.start_time_ns)
    ));
    lines.push(format!(
        "Offset    +{} into the trace",
        format_duration_ns(span.offset_ns.max(0) as u64, g.ascii)
    ));
    lines.push(format!(
        "Duration  {}",
        format_duration_ns(span.duration_ns, g.ascii)
    ));
    if span.malformed {
        lines.push(format!(
            "{}  parent chain is broken (orphan or cycle) {} shown as a malformed root",
            g.warn, g.dash
        ));
    }
    push_attr_section(&mut lines, "Attributes", &span.attributes);
    push_attr_section(&mut lines, "Resource", &span.resource);
    push_attr_section(&mut lines, "Scope", &span.scope);
    // Events/links are stored as canonical JSON (ARCHITECTURE.md §6.3); show them verbatim rather than
    // half-parsing them here.
    for (title, json) in [("Events", &span.events), ("Links", &span.links)] {
        if let Some(json) = json.as_deref().filter(|json| !json.is_empty()) {
            lines.push(String::new());
            lines.push(title.to_owned());
            lines.push(format!("  {json}"));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{sample_log_record, sample_trace};
    use crate::waterfall::build_trace_detail;

    #[test]
    fn span_detail_lines_show_ids_timing_and_every_section() {
        let detail = build_trace_detail(&sample_trace(), true);
        let g = Glyphs::new(true);
        let text = span_detail_lines(&detail.trace_id, &detail.spans[2], &g).join("\n");
        assert!(text.contains(&detail.trace_id));
        assert!(text.contains(&detail.spans[2].span_id));
        assert!(text.contains("Status    ERROR"));
        assert!(text.contains("Message   boom"));
        assert!(text.contains("Offset    +600.000us into the trace"));
        assert!(text.contains("Duration  100.000us"));
        assert!(
            text.contains("parent chain is broken"),
            "malformed note: {text}"
        );
        assert!(text.contains("Events"));
        assert!(text.contains("exception"));

        // The child's attributes render as a titled section.
        let child = span_detail_lines(&detail.trace_id, &detail.spans[1], &g).join("\n");
        assert!(child.contains("Attributes (1)"));
        assert!(child.contains("db.system = postgres"));
        // A root span says so rather than showing an empty parent.
        let root = span_detail_lines(&detail.trace_id, &detail.spans[0], &g).join("\n");
        assert!(root.contains("Parent    (root)"));
    }

    #[test]
    fn log_detail_lines_show_the_trace_and_body() {
        let lines = log_detail_lines(&sample_log_record(Some("abc123")));
        let text = lines.join("\n");
        assert!(text.contains("Trace ID  abc123"));
        assert!(text.contains("Span ID   aabbccdd11223344"));
        assert!(text.contains("Severity  INFO (9)"));
        // Body split across lines and indented.
        assert!(lines.iter().any(|l| l == "  hello"));
        assert!(lines.iter().any(|l| l == "  world"));
        // Attribute section present.
        assert!(text.contains("Attributes (1)"));
        assert!(text.contains("  http.method = GET"));

        // No trace id renders "(none)".
        let none = log_detail_lines(&sample_log_record(None)).join("\n");
        assert!(none.contains("Trace ID  (none)"));
    }
}
