# Query language translators plan
> **Implementation status (2026-07-21):** Q1-Q5 for the dependency-free P1/L1/T1 parser
> strategy are implemented. The adapter emits stable source ranges and
> rejects missing or ambiguous metric resolution. Valid syntax outside the matrices below remains
> deliberately unsupported.
>
> **Crate note (2026-07-21):** the translators now live in the `syntax` module of the consolidated
> `imbh-lgtm` crate (LGTM-stack query languages); `imbh-query-language` no longer exists as a separate
> crate. References to `imbh-query-language` below are historical.
>

> **Prerequisite:** implement and pass S0 through S5 in
> [QUERY_SEMANTICS_CONFORMANCE_PLAN.md](./QUERY_SEMANTICS_CONFORMANCE_PLAN.md). Translators parse and
> lower only after IMBH's execution models match the advertised upstream semantics.

## 1. Objective

After semantic conformance is implemented, add translators for the conformant subsets of PromQL, LogQL, and TraceQL. Each translator parses source text and lowers it into the already-conformant IMBH expression models. Unsupported constructs produce structured, source-positioned diagnostics.

A construct is advertised only when both its semantic evaluator and syntax lowering pass the pinned upstream conformance corpus. Semantic differences are errors, never warnings attached to approximate results.

## 2. Placement

Add an optional top-tier crate, tentatively `crates/imbh-query-language`:

```text
imbh-core <- ... <- imbh <- imbh-query-language <- imbh-tui
```

The adapter depends on the public `imbh` facade. No storage or query-engine crate depends on it. Gate parser dependencies independently with `promql`, `logql`, and `traceql` features, plus an `all` convenience feature for the TUI. Evaluate maintained upstream-compatible parsers before writing grammars locally, and record which upstream language versions each profile targets.

## 3. Common contract

Use one dispatch envelope around the conformant IMBH expression models:

```text
TranslatedQuery
  model: ImbhQueryModel
  source_language: PromQL | LogQL | TraceQL
  annotations: [Diagnostic]

ImbhQueryModel
  Prom(PromExpr)
  Log(LogExpr)
  Trace(TraceExpr)
```

Keep this envelope in the adapter crate initially. Promote it into the facade only if another independent consumer proves it is a stable IMBH contract.

Translation context supplies information absent from query text:

```text
TranslateContext
  mode: Instant | Range
  time_range
  step
  result_limit
  metric_resolver
  label_mapping
```

The metric resolver maps a name to table kind, unit, temporality, and monotonicity using `metrics().catalog()` plus host aliases. PromQL syntax alone cannot identify the IMBH metric table, and `rate()` cannot safely choose delta versus cumulative behavior without metadata. Ambiguity returns `NeedsResolution`; it must never guess.

Diagnostics carry a stable code, severity, source byte range, message, unsupported construct, and an optional exact alternative. Distinguish syntax errors, unsupported valid syntax, missing model capability, missing metadata, and semantic mismatch.

## 4. Conformant lowering targets

### 4.1 Logs

Use the conformant `LogMetricQuery`, which separates evaluation step from range window and preserves stream labels, missing results, offsets, and LogQL range semantics. It is the common target for LogQL metric expressions and TUI log-derived charts.

Lower line filters into the conformant log expression predicates. LogQL `|=` is substring matching and must never lower to native IMBH token-AND `matches()`.

### 4.2 Metrics

Lower into `PromExpr` nodes whose evaluators passed conformance. Native `MetricQuery`, `HistogramQuery`, and `ExpHistogramQuery` remain separate convenience APIs and are not substitutes for PromQL evaluation. Binary operations, vector matching, subqueries, and functions remain parser errors until their execution nodes pass the same gate.

### 4.3 Traces

Lower into conformant `TraceExpr` nodes with explicit span, resource, instrumentation-scope, event, and link scopes. TraceQL `resource.*` must never lower to the span `attributes` column. `TraceSpanset` is the result model for spanset logic, structural relationships, and pipeline aggregation; native `TraceQuery` remains a separate convenience API.

Every facade change must update `ARCHITECTURE.md`, proto mappings where applicable, and compatibility tests.

## 5. Initial profiles

### 5.1 PromQL P1

Support:

- instant and range selectors by metric name;
- `=`, `!=`, `=~`, and `!~` label matchers;
- `sum`, `avg`, `min`, `max`, and `count`;
- `by(...)` and `without(...)` grouping;
- `rate()` when catalog metadata resolves temporality and monotonicity;
- canonical `histogram_quantile()` expressions that resolve to an IMBH histogram;
- semantically neutral parentheses.

Defer `increase`, `irate`, binary and set operators, vector matching, subqueries, `offset`, `@`, label mutation, native-histogram arithmetic, and arbitrary nesting until each evaluator passes conformance. P1 `rate()` uses the reset-aware, boundary-extrapolated PromQL evaluator; native IMBH bucket-rate methods are never used as substitutes.

### 5.2 LogQL L1

Support:

- stream selectors with all four label matcher operators;
- exact line filters `|=`, `!=`, `|~`, and `!~`;
- AND-combined label and line filters;
- `count_over_time` and `rate` lowered to `LogMetricQuery`;
- `sum by(...)` around a supported log metric expression;
- time range, step, direction, and limit from translation context.

Defer JSON/logfmt/regexp/pattern parser stages, extracted labels, `unwrap`, formatting, label mutation, pipeline errors, boolean OR, bytes functions, quantiles, and binary expressions.

Allow hosts to map Loki-style labels such as `job`, `app`, or `namespace` to OTel keys. Unknown labels remain ordinary attributes and are never assigned an attribute scope by guesswork.

### 5.3 TraceQL T1

Support conformant spanset expressions containing:

- span name, duration, status, and kind intrinsics;
- trace duration, root service, and root name where exact equivalents exist;
- equality, inequality, regex, and numeric comparisons on explicitly scoped attributes;
- time range and trace limit from translation context;
- spanset AND/OR;
- child, parent, ancestor, descendant, sibling, and union-structural operators;
- `count()` filtering over a spanset.

Defer `by`, `coalesce`, `select`, arithmetic, trace-metric functions beyond the conformant initial set, and experimental operators until their evaluators pass conformance.

## 6. Delivery order

The semantic milestones S0 through S5 in `QUERY_SEMANTICS_CONFORMANCE_PLAN.md` precede Q0.

### Q0: parser spike

- Evaluate parser grammar coverage, maintenance, license, unsafe code, source spans, crate count, and binary size.
- Verify every conformant corpus expression has an AST with accurate source ranges.
- Record upstream language and compatibility-profile versions.

Exit: selected parser strategy with no semantic work hidden in the parser layer.

### Q1: translation contract

- Implement diagnostics, source ranges, context, capability reporting, and `ImbhQueryModel` dispatch.
- Bind translation capabilities to the semantic evaluator's capability/version id.
- Reject any AST node whose evaluator capability is absent.

Exit: translation can target every conformant model and cannot target a non-conformant one.

### Q2: PromQL P1

- Integrate the selected parser behind `promql`.
- Lower selectors, aggregations, rates, and histogram quantiles.
- Test catalog ambiguity and reset-aware rate lowering against the conformance corpus.

Exit: common metric explorer expressions execute against OTLP fixtures.

### Q3: LogQL L1

- Integrate the selected parser behind `logql`.
- Lower selectors, exact line filters, count/rate-over-time, and grouping.
- Test missing labels, regex anchoring, negative filters, quiet buckets, and escaping.

Exit: common log exploration and derived-metric expressions translate exactly.

### Q4: TraceQL T1

- Integrate the selected parser behind `traceql`.
- Lower supported intrinsics, scoped attributes, spanset logic, structural operators, and count pipelines.
- Produce dedicated diagnostics for constructs outside the conformant capability set.
- Test attribute scope, duration units, returned-span semantics, and trace limits.

Exit: the initial conformant TraceQL surface translates without collapsing spanset semantics.

### Q5: integration and footprint

- Add a small translation/execution example or CLI.
- Add fuzz/property tests for non-panics and diagnostic ranges.
- Measure every language feature separately and together.
- Run the Rust, license, duplicate-version, and footprint gates.
- Publish the syntax capability matrix and pinned semantic-conformance version.

Exit: stable adapter API, hermetic tests, unchanged default IMBH graph, and documented footprint.

Only after both S5 and Q5 should the TUI accept these query languages as primary inputs.

## 7. Testing

- Golden tests assert exact query-model fields, not generated SQL strings.
- Execution tests compare translated queries with direct typed-builder calls.
- Upstream-engine differential tests are opt-in; default tests remain daemon-free and network-free.
- Every supported operator covers missing labels, empty results, escaping, regexes, and time boundaries.
- Every unsupported AST node has a stable code and accurate source range.
- Fuzz parsing and lowering for panics, recursion limits, and uncontrolled allocation.
- Run dependency and footprint gates for each feature combination.

## 8. TUI integration

Each screen gets a language-aware query bar and a structured editor. Both produce the same `ImbhQueryModel`; terminal state contains no parser logic. Diagnostics appear below the editor with the source range highlighted. Approximate semantic results are never displayed as compatible query results.

- Metrics accepts `Prom(PromExpr)`.
- Logs accepts `Log(LogExpr)`.
- Traces accepts `Trace(TraceExpr)`.

The translators remain independently reusable by servers, CLIs, and bindings.
