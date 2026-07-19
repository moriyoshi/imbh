# PromQL → imbh SQL recipes

imbh's native metric surface is SQL plus the typed `metrics()` builder (ARCHITECTURE.md §10.8); the
optional `imbh-lgtm` crate adds a *bounded*, versioned PromQL profile (`imbh.promql.p1.v1`) on top,
not a complete PromQL engine (OVERVIEW.md §3). This guide covers the SQL / typed path — for the
patterns outside that bounded profile, or that you'd simply rather express directly. The typed API
compiles to exactly the SQL shown, so use whichever reads better in your host.

## The metric tables

OTLP metric points normalize into one table per kind (ARCHITECTURE.md §6.4):

| Table | OTLP kind | Value columns |
|-------|-----------|---------------|
| `metrics_gauge` | Gauge | `value` (Float64) |
| `metrics_sum` | Sum (counter) | `value`, `temporality`, `is_monotonic` |
| `metrics_histogram` | explicit-bucket Histogram | `count`, `sum`, `min`, `max`, `explicit_bounds` (List), `bucket_counts` (List) |
| `metrics_exp_histogram` | exponential Histogram | `count`, `sum`, `min`, `max`, `scale`, `zero_count`, `positive_offset` + `positive_counts` (List), `negative_offset` + `negative_counts` (List) |
| `metrics_summary` | Summary | `count`, `sum`, `quantiles` (List), `values` (List) |

Every table shares the identity columns `time`, `metric`, `unit`, `service`, `attributes`
(canonical-JSON object), `resource`, `scope`. Read a label off `attributes` with
`json_get_str(attributes, 'key')`.

## Label selectors → `WHERE`

PromQL:

```promql
http_requests_total{service="cart", route="/checkout"}
```

imbh SQL:

```sql
SELECT * FROM metrics_sum
WHERE metric = 'http_requests_total'
  AND service = 'cart'
  AND json_get_str(attributes, 'route') = '/checkout'
```

Typed — all four PromQL matcher operators are supported:

```rust
MetricQuery::sum("http_requests_total")
    .filter("service", "cart")              // service="cart"  (equality)
    .filter_ne("env", "test")               // env!="test"     (series without `env` are kept)
    .filter_regex("route", "^/api/v[0-9]+") // route=~"…"      (RE2 regex)
    .filter_not_regex("route", "^/health")  // route!~"…"      (negated regex; missing label kept)
```

Positive selectors (`=`/`=~`) drop a series that lacks the label; the negations (`!=`/`!~`) keep it,
matching PromQL's absent-label = `""` semantics.

## Time bucketing (the range-vector step)

PromQL evaluates over a `[range]` at each `step`. In SQL, floor the timestamp to the step to form
buckets:

```sql
SELECT (CAST("time" AS BIGINT) / 60000000000) * 60000000000 AS bucket,  -- 60s in ns
       avg(value) AS v
FROM metrics_gauge
WHERE metric = 'process_cpu'
GROUP BY bucket
ORDER BY bucket
```

Typed: `MetricQuery::gauge("process_cpu").step(Duration::from_secs(60))` → a `Matrix`.

## `sum by` / `avg by` (aggregation over labels)

PromQL:

```promql
sum by (route) (rate(http_requests_total[5m]))
```

imbh groups by the label columns you name:

```sql
SELECT (CAST("time" AS BIGINT) / 300000000000) * 300000000000 AS bucket,
       json_get_str(attributes, 'route') AS route,
       sum(value) / 300.0 AS v            -- delta counter → per-second rate
FROM metrics_sum
WHERE metric = 'http_requests_total'
GROUP BY bucket, route
ORDER BY bucket
```

Typed:

```rust
MetricQuery::sum("http_requests_total")
    .rate()                            // delta-temporality per-second rate
    .group_by("route")
    .step(Duration::from_secs(300))
```

## `rate()` / `increase()`

How a counter's rate is computed depends on its OTLP temporality:

- **Delta** (each point is the increment since the last export): `rate = sum(value) / step_seconds`.
  Use `MetricQuery::rate()`.
- **Cumulative** (each point is the running total): `rate = (max(value) - min(value)) / step_seconds`
  within each bucket. Use `MetricQuery::rate_counter()`.

`increase()` is the same without dividing by `step_seconds` (drop the divisor, or use `sum(value)` /
`max(value) - min(value)` directly).

## `histogram_quantile`

PromQL:

```promql
histogram_quantile(0.95, sum by (le) (rate(http_duration_bucket[5m])))
```

imbh stores each histogram data point as one row with its full bucket vector, so the quantile is a
per-row function over the `explicit_bounds` / `bucket_counts` List columns:

```sql
SELECT CAST("time" AS BIGINT) AS t,
       histogram_quantile(0.95, explicit_bounds, bucket_counts) AS p95
FROM metrics_histogram
WHERE metric = 'http_duration'
ORDER BY t
```

Typed:

```rust
db.metrics()
  .histogram_quantile(HistogramQuery::new("http_duration").quantile(0.95).group_by("route"))
  .await?;   // -> Matrix of p95 series
```

`histogram_quantile` interpolates linearly inside the matched bucket (Prometheus-style): it returns
`NaN` for an empty histogram, clamps to the largest finite bound when the quantile lands in the
`+Inf` overflow bucket, and assumes non-negative observations. Merging bucket vectors across
multiple series or time buckets before taking the quantile (the `sum by (le)` in the PromQL above) is
a follow-up; today the typed surface is one quantile per stored data point.

## Instant vector (the latest sample)

PromQL bare selector `http_requests_total` returns the most recent sample per series. Use the typed
`instant()`, which keeps the last sample of each range series:

```rust
db.metrics().instant(MetricQuery::sum("http_requests_total").step(Duration::from_secs(60))).await?;
// -> Vector (one InstantSample per label set)
```

## Not covered by the typed builder

Cross-series / time-bucket merging for `histogram_quantile` and PromQL's boundary extrapolation for
`rate()` are later work (ARCHITECTURE.md §6.4 / §10.8). Exponential histograms
(`db.metrics().exp_histogram_quantile(...)`) and exemplars (`db.metrics().exemplars(...)`) already
have typed methods; summaries are queryable as the `metrics_summary` table (no typed quantile builder
yet). For anything the typed builder does not express, drop to `db.sql(...)` — the metric tables are
ordinary SQL.
