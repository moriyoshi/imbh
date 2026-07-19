//! Explicit-bucket histogram quantile math (ARCHITECTURE.md §10.8).
//!
//! Pure, arrow-free so both the `imbh-query` `histogram_quantile` UDF and the `imbh`
//! typed `metrics().histogram_quantile` surface (which merges bucket vectors in Rust) share one
//! implementation.

/// Prometheus-style quantile estimate over OTLP explicit buckets. `bounds` = N ascending upper
/// bounds; `counts` = N+1 per-bucket counts (last = `+Inf`). Returns `NaN` for an empty histogram,
/// `-inf`/`+inf` for `phi` outside `[0,1]`, and the largest finite bound when the quantile lands in
/// the `+Inf` overflow bucket. Non-negative observations are assumed (the first bucket's lower
/// bound is `min(bounds[0], 0)`), matching the classic-histogram convention.
pub fn histogram_quantile(phi: f64, bounds: &[f64], counts: &[u64]) -> f64 {
    if phi.is_nan() {
        return f64::NAN;
    }
    if phi < 0.0 {
        return f64::NEG_INFINITY;
    }
    if phi > 1.0 {
        return f64::INFINITY;
    }
    // Saturating so an adversarial OTLP payload with near-`u64::MAX` bucket counts can neither panic
    // (debug overflow-check) nor wrap to a wrong total (release); the quantile stays well-defined.
    let total: u64 = counts.iter().copied().fold(0u64, u64::saturating_add);
    if total == 0 {
        return f64::NAN;
    }
    let rank = phi * total as f64;
    let n = bounds.len(); // finite buckets 0..n; bucket n is the +Inf overflow

    let mut cumulative: u64 = 0;
    let mut b = 0usize;
    while b < counts.len() {
        cumulative = cumulative.saturating_add(counts[b]);
        if cumulative as f64 >= rank {
            break;
        }
        b += 1;
    }
    if b >= counts.len() {
        b = counts.len().saturating_sub(1);
    }
    // Overflow bucket: clamp to the largest finite bound (Prometheus behavior).
    if b >= n {
        return bounds.last().copied().unwrap_or(f64::INFINITY);
    }

    let upper = bounds[b];
    let lower = if b == 0 {
        bounds[0].min(0.0)
    } else {
        bounds[b - 1]
    };
    let in_bucket = counts[b];
    if in_bucket == 0 {
        return upper;
    }
    let cumulative_before = cumulative - in_bucket;
    lower + (upper - lower) * ((rank - cumulative_before as f64) / in_bucket as f64)
}

/// Prometheus-style quantile estimate over an OTLP **exponential** (base-2) histogram data point
/// (ARCHITECTURE.md §10.8). Boundaries are reconstructed from `scale`: `base = 2^(2^-scale)`, and bucket
/// `index` spans `(base^index, base^(index+1)]`. `positive_counts[i]` is bucket `positive_offset + i`
/// (values `> 0`); `negative_counts[i]` is bucket `negative_offset + i` on the absolute-value scale
/// (values `< 0`); `zero_count` holds the near-zero bucket. Same edge-case contract as
/// [`histogram_quantile`]: `NaN` for empty, `-inf`/`+inf` for `phi` outside `[0,1]`. Interpolation
/// is linear within the matched bucket's boundaries.
#[allow(clippy::too_many_arguments)]
pub fn exp_histogram_quantile(
    phi: f64,
    scale: i32,
    zero_count: u64,
    positive_offset: i32,
    positive_counts: &[u64],
    negative_offset: i32,
    negative_counts: &[u64],
) -> f64 {
    if phi.is_nan() {
        return f64::NAN;
    }
    if phi < 0.0 {
        return f64::NEG_INFINITY;
    }
    if phi > 1.0 {
        return f64::INFINITY;
    }
    // Saturating (see `histogram_quantile`): untrusted counts must not panic or wrap the total.
    let total: u64 = positive_counts
        .iter()
        .chain(negative_counts.iter())
        .copied()
        .fold(zero_count, u64::saturating_add);
    if total == 0 {
        return f64::NAN;
    }
    let rank = phi * total as f64;
    // bound(index) = base^index = 2^(index * 2^-scale). Index math is done in i64 so adversarial
    // `offset + i` values near i32::MAX cannot overflow (they'd only ever produce a finite bound).
    // `-(scale as f64)` (not `powi(-scale)`) avoids the negation overflow at `scale == i32::MIN`.
    let factor = 2f64.powf(-(scale as f64));
    let bound = |index: i64| -> f64 { 2f64.powf(index as f64 * factor) };
    // Linear interpolation within a bucket. At extreme (but OTLP-valid) scales a bucket edge can
    // overflow to ±inf (e.g. `scale = -10` → `bound(1) = 2^1024 = inf`); the raw formula would then
    // yield `inf*0 = NaN` (phi at a bucket start) or `inf`. Fall back to the finite edge so the
    // estimate stays finite and ordered.
    let interp = |lower: f64, upper: f64, before: u64, c: u64| -> f64 {
        let frac = (rank - before as f64) / c as f64;
        let est = lower + (upper - lower) * frac;
        if est.is_finite() {
            est
        } else if lower.is_finite() {
            lower
        } else {
            upper
        }
    };

    let mut cumulative: u64 = 0;
    // 1. Negative buckets, most-negative first (highest index → lowest): value increases toward 0.
    for i in (0..negative_counts.len()).rev() {
        let c = negative_counts[i];
        if c == 0 {
            continue;
        }
        cumulative = cumulative.saturating_add(c);
        if cumulative as f64 >= rank {
            let idx = negative_offset as i64 + i as i64;
            let lower = -bound(idx + 1); // most negative
            let upper = -bound(idx); // closest to zero
            let before = cumulative - c;
            return interp(lower, upper, before, c);
        }
    }
    // 2. Zero bucket.
    if zero_count > 0 {
        cumulative = cumulative.saturating_add(zero_count);
        if cumulative as f64 >= rank {
            return 0.0;
        }
    }
    // 3. Positive buckets, lowest index first.
    for (i, &c) in positive_counts.iter().enumerate() {
        if c == 0 {
            continue;
        }
        cumulative = cumulative.saturating_add(c);
        if cumulative as f64 >= rank {
            let idx = positive_offset as i64 + i as i64;
            let lower = bound(idx);
            let upper = bound(idx + 1);
            let before = cumulative - c;
            return interp(lower, upper, before, c);
        }
    }
    // Fallback for fp drift: the top positive boundary (or 0 if there are no positive buckets).
    if positive_counts.is_empty() {
        0.0
    } else {
        bound(positive_offset as i64 + positive_counts.len() as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_histogram_quantile_interpolates() {
        // scale 0 (base=2), one bucket [index 0] = (1, 2] with 4 values, no zeros/negatives.
        let q = |p: f64| exp_histogram_quantile(p, 0, 0, 0, &[4], 0, &[]);
        assert!((q(0.0) - 1.0).abs() < 1e-9, "p0={}", q(0.0)); // bound(0)
        assert!((q(0.5) - 1.5).abs() < 1e-9, "p50={}", q(0.5)); // midpoint of (1,2]
        assert!((q(1.0) - 2.0).abs() < 1e-9, "p100={}", q(1.0)); // bound(1)

        // A zero bucket the rank lands in → 0.0.
        assert_eq!(exp_histogram_quantile(0.1, 0, 8, 0, &[2], 0, &[]), 0.0);

        // Two positive buckets: [0]=(1,2] x2, [1]=(2,4] x2; p75 (rank 3) lands in bucket 1.
        let v = exp_histogram_quantile(0.75, 0, 0, 0, &[2, 2], 0, &[]);
        assert!(v > 2.0 && v <= 4.0, "p75={v} should be in (2,4]");
        assert!((v - 3.0).abs() < 1e-9, "p75={v}"); // 2 + (4-2)*((3-2)/2)

        // A negative-only histogram: bucket index 0 = [-2, -1) x4; p50 → -1.5.
        let n = exp_histogram_quantile(0.5, 0, 0, 0, &[], 0, &[4]);
        assert!((n - (-1.5)).abs() < 1e-9, "neg p50={n}");

        // Degenerate.
        assert!(exp_histogram_quantile(0.5, 0, 0, 0, &[], 0, &[]).is_nan());
        assert!(exp_histogram_quantile(-0.1, 0, 0, 0, &[1], 0, &[]).is_sign_negative());
        assert!(exp_histogram_quantile(1.1, 0, 0, 0, &[1], 0, &[]).is_infinite());
    }

    #[test]
    fn exp_histogram_quantile_extreme_scale_stays_finite() {
        // scale = -10 → factor 2^10 = 1024, so bound(1) = 2^1024 = +inf. The bucket-start quantile
        // must not become `inf*0 = NaN`; it falls back to the finite lower edge (bound(0) = 1.0).
        let v = exp_histogram_quantile(0.0, -10, 0, 0, &[4], 0, &[]);
        assert!(v.is_finite(), "p0 at scale=-10 must be finite, got {v}");
        assert!(
            (v - 1.0).abs() < 1e-9,
            "p0 at scale=-10 = {v}, want the finite lower edge 1.0"
        );
        // A phi inside the overflowing bucket also stays finite (no NaN).
        assert!(
            exp_histogram_quantile(0.5, -10, 0, 0, &[4], 0, &[]).is_finite(),
            "p50 at scale=-10 must be finite"
        );
    }

    #[test]
    fn histogram_quantile_interpolates() {
        // bounds=[1,5], counts=[2,3,2] (N+1=3), total=7.
        let bounds = [1.0, 5.0];
        let counts = [2u64, 3, 2];
        let q = |p: f64| histogram_quantile(p, &bounds, &counts);

        // p0 → bottom of the first bucket (lower=0).
        assert!((q(0.0) - 0.0).abs() < 1e-9, "p0={}", q(0.0));
        // p10: rank=0.7 lands in bucket 0 (0,1]; 0 + 1*(0.7/2) = 0.35.
        assert!((q(0.1) - 0.35).abs() < 1e-9, "p10={}", q(0.1));
        // p50: rank=3.5 lands in bucket 1 (1,5]; 1 + 4*((3.5-2)/3) = 3.0.
        assert!((q(0.5) - 3.0).abs() < 1e-9, "p50={}", q(0.5));
        // p99: rank in the +Inf overflow bucket → clamp to the top finite bound.
        assert!((q(0.99) - 5.0).abs() < 1e-9, "p99={}", q(0.99));
        assert!((q(1.0) - 5.0).abs() < 1e-9, "p100={}", q(1.0));

        // Degenerate inputs.
        assert!(q(-0.1).is_infinite() && q(-0.1) < 0.0);
        assert!(q(1.1).is_infinite() && q(1.1) > 0.0);
        assert!(histogram_quantile(0.5, &bounds, &[0, 0, 0]).is_nan());
        assert!(histogram_quantile(f64::NAN, &bounds, &counts).is_nan());
        // Quantiles are monotonic non-decreasing in phi.
        assert!(q(0.25) <= q(0.75));
    }
}
