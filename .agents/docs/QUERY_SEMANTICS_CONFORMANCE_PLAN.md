# Query semantics conformance plan
> **Implementation status (2026-07-21):** P1/L1/T1 semantic models, bounded fetch plans,
> facade-backed execution, and hermetic conformance/regression tests are implemented. The optional
> external-daemon differential runner described in
> section 7 remains a profile-expansion tool; default verification is fully in-process.
>
> **Crate note (2026-07-21):** the models/evaluators now live in the `model` module (and the source
> adapter in the `source` feature) of the consolidated `imbh-lgtm` crate (LGTM-stack query languages);
> `imbh-semantics` no longer exists as a separate crate. References to `imbh-semantics` /
> `imbh-source` below are historical.
>

## 1. Rule

Semantic conformance precedes PromQL, LogQL, and TraceQL translation.

For every construct IMBH advertises as supported, parsing and execution must match the selected upstream language version. A translator may reject an unsupported construct, but it may not execute it with approximate semantics, attach a warning, and still call the result compatible.

The conformance surface is versioned and machine-readable. Expanding it is incremental; changing an already-supported behavior requires compatibility review and differential tests.

## 2. Why the current models are insufficient

The current builders are endpoint-shaped IMBH queries, not execution models for the three languages:

- MetricQuery buckets stored points directly. PromQL evaluates independently at each evaluation timestamp, applies lookback and range-vector windows, preserves label-set rules, and defines reset and boundary extrapolation.
- logs().volume_by() groups rows into display buckets. LogQL range functions evaluate a sliding log range at each step, distinguish stream labels from parsed labels, and preserve empty-vector and pipeline-error behavior.
- TraceQuery returns summaries for traces containing matching spans. TraceQL evaluates spansets per trace, preserves selected spans, distinguishes attribute scopes and types, and defines structural relationships.

The fix is a semantic execution layer with exact typed models. Parser adapters land only after this layer passes its conformance gate.

## 3. Architecture

Add lightweight, engine-independent semantic models in a new top-tier crate or facade module, with execution compiled through imbh-query and bounded post-processing where SQL is insufficient:

~~~text
language text
    |
    v
language parser (later)
    |
    v
conformant expression model
    |
    +--> DataFusion plans through imbh-query
    +--> bounded semantic evaluator where SQL is insufficient
    |
    v
language-shaped result plus annotations
~~~

Parser dependencies never enter imbh-query. The query engine may learn generic operations needed for exact execution, but language syntax remains in imbh-query-language.

Common concepts:

- explicit evaluation timestamps for instant and range queries;
- separate range-vector window and evaluation step;
- typed scalar, string, duration, status, and attribute values;
- label sets with absent-value semantics;
- stable result annotations and semantic errors;
- deterministic limits for samples, series, spans, recursion, and intermediate vectors;
- a capability/version identifier on every request.

Existing MetricQuery, LogQuery, and TraceQuery remain convenient native IMBH APIs. Language-compatible models are separate so existing behavior does not silently change.

## 4. PromQL conformance

### 4.1 Evaluation

Implement a request containing expression, instant or start/end timestamps, step, lookback delta, and limits. At each timestamp:

- instant selectors choose the latest eligible sample at or before the timestamp;
- range selectors use exact upstream interval boundary rules;
- absent or stale series are omitted rather than fabricated as zero;
- output timestamps are evaluation timestamps, not storage bucket starts;
- offsets and explicit evaluation timestamps are represented even if initially rejected.

Preserve metric names and labels according to each operator. Prometheus regex matchers are fully anchored, so lower them as full matches. Implement upstream missing-label behavior.

### 4.2 Counters and rate

Implement a conformant range-vector counter evaluator:

- require the minimum valid sample count;
- sort and validate samples deterministically;
- correct monotonicity breaks as resets;
- calculate observed increase;
- extrapolate to range boundaries with the upstream algorithm;
- divide by range duration for rate;
- apply rate before aggregation so resets remain visible per series;
- carry NaN, infinity, and histogram annotations correctly.

Do not map an OTLP delta sum directly to PromQL rate. Either reconstruct a cumulative series with an explicit continuity boundary or return an incompatibility error. Cumulative monotonic sums are the direct source.

Native IMBH rate() and rate_counter() retain their documented bucket semantics. PromQL uses a distinct conformant evaluator.

### 4.3 Aggregation and histograms

Implement exact sum, avg, min, max, and count label retention for by and without. Preserve empty-vector behavior and metric-name dropping.

Validate explicit and exponential histogram conversion against Prometheus rules. histogram_quantile must match boundary, monotonicity-repair, NaN, empty, and infinity behavior. Reject classic bucket-name assumptions unless a resolver supplies an exact mapping.

The first PromQL gate includes selectors, four label matchers, by/without aggregation, reset-aware extrapolated rate, and supported histogram quantiles. Nothing else is advertised before its evaluator and fixtures land.

## 5. LogQL conformance

### 5.1 Streams and labels

Define a host-supplied LogStreamSchema mapping Loki stream labels to exact OTel fields and scopes. Stream identity is the resulting complete label set; record attributes are not automatically all stream labels.

Implement all four label matchers with LogQL missing-label and regex behavior. Preserve original labels separately from extracted or mutated labels.

### 5.2 Lines and pipelines

Implement exact positive and negative substring and RE2 line filters over the log line. Never reuse tokenized full-text matching for substring semantics. Preserve left-to-right pipeline order.

Before parser stages are supported, define pipeline state containing:

- current line;
- stream labels;
- extracted labels;
- typed unwrapped value;
- the __error__ label and stage error;
- formatting output separate from stored data.

Failed stages follow LogQL error flow. Metric queries reject unfiltered pipeline errors where LogQL does.

### 5.3 Range metrics

Model range window and evaluation step as different fields. At each evaluation timestamp:

- count_over_time counts entries in the exact window;
- rate divides window count by window duration, not display step;
- missing results remain no data rather than becoming zero;
- grouping follows by and without label rules;
- offset shifts the range without changing evaluation timestamps.

LogMetricQuery models these semantics directly. It is not a rename of non-overlapping volume_by buckets.

The first LogQL gate includes stream selectors, exact line filters, AND pipelines, count_over_time, rate, and by/without vector aggregation. Other stages wait for their exact pipeline semantics.

## 6. TraceQL conformance

### 6.1 Data and types

Add explicit access to span, resource, instrumentation scope, event, and link attributes plus supported intrinsics. Preserve value types instead of comparing everything as strings. Implement nil, type compatibility, duration, status, kind, id, and regex behavior.

Events and links currently stored as JSON need bounded typed extraction or normalized query views before those scopes are advertised.

### 6.2 Spansets

A TraceQL query returns selected spansets associated with traces, not only trace summaries. Add a TraceSpanset result containing trace identity/summary, selected span ids, and optional selected fields.

Evaluate expressions per trace. A selector returns matching spans. Logical spanset operations preserve defined union/intersection behavior and never collapse distinct-span conditions into one predicate.

### 6.3 Structure and pipelines

Implement child, parent, descendant, ancestor, and sibling relationships from span_id and parent_span_id. Implement union-structural variants separately because their returned spans differ. Handle missing parents, multiple roots, cycles, and duplicate ids deterministically.

Structural operators return the side or union required by TraceQL, not merely a boolean trace match.

Implement the bounded spanset pipeline required for count() filtering. Grouping, selection, and trace metrics become supported only after their label and result semantics have fixtures.

The first TraceQL gate includes scoped typed predicates, spanset AND/OR, core structural and union-structural operators, and count() filtering.

## 7. Conformance harness

Create a versioned corpus for each language containing:

- expression and evaluation options;
- normalized input samples, logs, or traces;
- expected typed result, labels/spans, annotations, and errors;
- upstream product version and provenance;
- edge cases for empty input, missing labels, duplicate timestamps, malformed traces, NaN/infinity, escaping, and limits.

Default tests execute stored fixtures in-process without daemons or network. An opt-in differential harness runs the same fixtures against pinned Prometheus, Loki, and Tempo releases, normalizes responses, and reports deltas. Updating an oracle version requires review of every changed fixture.

Property and fuzz tests cover parser-independent evaluation, resource limits, and non-panics.

## 8. Delivery order

### S0: references and disparity matrix

- Select exact upstream versions.
- Inventory every construct in the first translator profiles.
- Record current IMBH behavior, upstream behavior, storage prerequisites, and exact target behavior.
- Correct documentation that presents an approximation as an equivalent.

Exit: no unresolved semantic question in the initial surfaces.

### S1: common evaluation and result model

- Add evaluation timestamps, windows, steps, typed values, labels, annotations, limits, and capability ids.
- Keep native IMBH builders source-compatible.
- Add serialization/proto mappings only after model review.

Exit: all evaluators share precise time, value, label, error, and limit vocabulary.

### S2: PromQL semantics

- Implement selectors, anchored matchers, label retention, reset-aware extrapolated rate, and histograms.
- Add fixtures and opt-in Prometheus differential tests.

Exit: zero unexplained oracle deltas in the initial PromQL surface.

### S3: LogQL semantics

- Implement stream schema, exact line filters, pipeline state, sliding ranges, rate/count, and aggregation.
- Add fixtures and opt-in Loki differential tests.

Exit: zero unexplained oracle deltas in the initial LogQL surface.

### S4: TraceQL semantics

- Implement typed scopes, spanset results, logical and structural operations, and initial aggregation.
- Add fixtures and opt-in Tempo differential tests.

Exit: zero unexplained oracle deltas in the initial TraceQL surface.

### S5: architecture and footprint gate

- Update ARCHITECTURE.md, APIs, proto surface, compatibility policy, and examples.
- Measure dependency, binary, and RSS impact by signal.
- Run format, build, clippy, workspace tests, feature matrix, license, and footprint gates.

Exit: conformance is an implemented, documented IMBH capability.

Only after S5 does parser selection and syntax lowering begin. Translators should be thin: parsing and source diagnostics live there, while semantic correctness stays in this conformance suite.
