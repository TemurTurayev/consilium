//! Descriptive statistics for eval metrics. Deliberately small and honest: a
//! summary always states its sample size, and it reports a spread only when
//! there is more than one sample to compute it from.

use serde::Serialize;

/// Mean, median and (when it exists) sample standard deviation of `n` samples.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MetricSummary {
    /// How many samples the figures below were computed from.
    pub n: usize,
    pub mean: f64,
    pub median: f64,
    /// Sample standard deviation (`n - 1` denominator). `None` for fewer than
    /// two samples: one run says nothing about run-to-run variation.
    pub std_dev: Option<f64>,
}

/// Summarize a metric's samples. An empty slice gives `n = 0` and zeroes.
pub fn summarize(values: &[u64]) -> MetricSummary {
    let n = values.len();
    if n == 0 {
        return MetricSummary {
            n,
            mean: 0.0,
            median: 0.0,
            std_dev: None,
        };
    }
    let samples: Vec<f64> = values.iter().map(|&v| v as f64).collect();
    let mean = samples.iter().sum::<f64>() / n as f64;
    MetricSummary {
        n,
        mean,
        median: median(&samples),
        std_dev: sample_std_dev(&samples, mean),
    }
}

/// The middle sample, or the mean of the two middle samples for an even count.
/// `samples` is non-empty.
fn median(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

fn sample_std_dev(samples: &[f64], mean: f64) -> Option<f64> {
    if samples.len() < 2 {
        return None;
    }
    let squared_deviations: f64 = samples.iter().map(|x| (x - mean).powi(2)).sum();
    Some((squared_deviations / (samples.len() - 1) as f64).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variance_is_absent_for_n1_and_present_for_n2() {
        let one = summarize(&[100]);
        assert_eq!(one.n, 1);
        assert_eq!(one.std_dev, None);
        let two = summarize(&[100, 300]);
        assert_eq!(two.n, 2);
        assert_eq!(two.mean, 200.0);
        assert!(two.std_dev.unwrap() > 0.0);
    }

    #[test]
    fn std_dev_uses_the_sample_denominator() {
        // Deviations are ±100, so the sum of squares is 20000; divided by
        // n - 1 = 1 (not n = 2) that is sqrt(20000) ≈ 141.42, not 100.
        let spread = summarize(&[100, 300]).std_dev.unwrap();
        assert!((spread - 20000f64.sqrt()).abs() < 1e-9, "got {spread}");
        // identical samples have no spread, which is still a real zero
        assert_eq!(summarize(&[7, 7, 7]).std_dev, Some(0.0));
    }

    #[test]
    fn median_handles_odd_and_even_counts() {
        assert_eq!(summarize(&[5]).median, 5.0);
        assert_eq!(summarize(&[3, 1, 2]).median, 2.0);
        // even count: the mean of the two middle samples, not rounded down
        assert_eq!(summarize(&[1, 2, 3, 4]).median, 2.5);
    }

    #[test]
    fn nothing_summarizes_to_an_empty_sample() {
        let none = summarize(&[]);
        assert_eq!((none.n, none.mean, none.median), (0, 0.0, 0.0));
        assert_eq!(none.std_dev, None);
    }
}
