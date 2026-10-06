//! Descriptive statistics: nearest-rank percentiles, the sample standard
//! deviation, and rounding half away from zero.

use anyhow::Context;
use serde::Serialize;

/// A decimal the SDK holds, as a float, through its printed form.
pub fn decimal(x: &impl std::fmt::Display) -> anyhow::Result<f64> {
    let text = x.to_string();
    text.parse()
        .with_context(|| format!("{text} is not a decimal"))
}

/// Rounds to `places` decimals, half away from zero, as jq's `round` does.
pub fn round_to(x: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (x * scale).round() / scale
}

/// Nearest-rank percentile, `p` in [0, 100]: the sorted value at index
/// round(p / 100 × (n − 1)).
pub fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    let last = sorted.len().checked_sub(1)?;
    // The index is in [0, last] because p is in [0, 100].
    let index = (p / 100.0 * last as f64).round() as usize;
    sorted.get(index).copied()
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Summary {
    pub n: usize,
    pub mean: Option<f64>,
    pub sd: Option<f64>,
    pub p05: Option<f64>,
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// Summary at 2 decimals; `sd` is the sample standard deviation (n − 1) and
/// is absent below two values.
pub fn summary(values: &[f64]) -> Summary {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    let mean = (n > 0).then(|| sorted.iter().sum::<f64>() / n as f64);
    let sd = mean
        .filter(|_| n > 1)
        .map(|m| (sorted.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (n - 1) as f64).sqrt());
    let r = |x: Option<f64>| x.map(|v| round_to(v, 2));
    Summary {
        n,
        mean: r(mean),
        sd: r(sd),
        p05: r(percentile(&sorted, 5.0)),
        p50: r(percentile(&sorted, 50.0)),
        p95: r(percentile(&sorted, 95.0)),
        min: r(sorted.first().copied()),
        max: r(sorted.last().copied()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_nearest_rank() {
        let sorted = [1.0, 2.0, 3.0, 4.0];
        // 0.5 × 3 = 1.5 rounds away from zero to index 2.
        assert_eq!(percentile(&sorted, 50.0), Some(3.0));
        assert_eq!(percentile(&sorted, 0.0), Some(1.0));
        assert_eq!(percentile(&sorted, 100.0), Some(4.0));
        assert_eq!(percentile(&[], 50.0), None);
    }

    #[test]
    fn rounding_is_half_away_from_zero() {
        assert_eq!(round_to(-1.125, 2), -1.13);
        assert_eq!(round_to(2.5, 0), 3.0);
    }

    #[test]
    fn summary_of_nothing_has_no_values() {
        let s = summary(&[]);
        assert_eq!((s.n, s.mean, s.p50, s.sd), (0, None, None, None));
    }

    #[test]
    fn sd_needs_two_values() {
        assert_eq!(summary(&[1.0]).sd, None);
        assert_eq!(summary(&[1.0, 3.0]).sd, Some(1.41));
    }
}
