//! Owns timing: the clock a measurement reads, how a batch is sized, and the statistics a
//! set of samples reports.
//!
//! Every clock on a development host quantizes, and the quantum is large next to the work a
//! small input costs. A single compression of one kilobyte lands within tens of clock ticks,
//! so a single-shot reading carries a quantization error of tens of percent. A measurement
//! below the batch threshold therefore times a repeated batch and divides, and the sample it
//! reports carries the repetition count. A sample with no repetition count cannot be
//! interpreted, so nothing here produces one.
//!
//! This module does not own what is measured, how many samples a tier takes, or whether the
//! spread between them is acceptable.

use std::time::Instant;

/// The wall-clock time one batch aims for.
///
/// It is four orders of magnitude above the 41.67 ns quantum measured on the development
/// host, so the quantization error of a batch is far below the spread between samples.
const BATCH_TARGET_NS: u64 = 1_000_000;

/// The largest batch a calibration may ask for.
///
/// It is what an operation of one nanosecond would buy, which is the most the target can
/// ask for. A measured cost of zero, which a clock too coarse to see the operation reports,
/// lands here rather than on an unbounded count.
const BATCH_LIMIT: u32 = 1_000_000;

/// How many repetitions one timing of `cost_ns` needs to reach the batch target.
///
/// Returns 1 when one repetition already reaches it, which is the case for every input from
/// the medium class upward.
pub fn repetitions(cost_ns: u64) -> u32 {
    if cost_ns >= BATCH_TARGET_NS {
        return 1;
    }
    let wanted = BATCH_TARGET_NS.div_euclid(cost_ns.max(1)).max(1);
    u32::try_from(wanted)
        .unwrap_or(BATCH_LIMIT)
        .min(BATCH_LIMIT)
}

/// Times one closure and reports the nanoseconds it took.
pub fn time<T>(work: impl FnOnce() -> T) -> (T, u64) {
    let started = Instant::now();
    let produced = work();
    let elapsed = started.elapsed();
    (
        produced,
        u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
    )
}

/// A set of timings of one operation, and the batch each one covered.
pub struct Samples {
    /// The nanoseconds one repetition took, one entry per sample.
    values_ns: Vec<u64>,
    /// How many repetitions each sample timed.
    repetitions: u32,
}

impl Samples {
    pub const fn new(repetitions: u32) -> Self {
        Self {
            values_ns: Vec::new(),
            repetitions,
        }
    }

    /// Records one batch, which took `batch_ns` for `self.repetitions` repetitions.
    pub fn push_batch(&mut self, batch_ns: u64) {
        let per = batch_ns.div_euclid(u64::from(self.repetitions).max(1));
        self.values_ns.push(per);
    }

    pub const fn count(&self) -> usize {
        self.values_ns.len()
    }

    fn sorted(&self) -> Vec<u64> {
        let mut values = self.values_ns.clone();
        values.sort_unstable();
        values
    }

    /// The statistics a result reports, or none when nothing was sampled.
    pub fn statistics(&self) -> Option<Statistics> {
        let sorted = self.sorted();
        let count = sorted.len();
        if count == 0 {
            return None;
        }
        let median = quantile(&sorted, 50)?;
        let min = *sorted.first()?;
        let max = *sorted.last()?;
        Some(Statistics {
            samples: count,
            repetitions: self.repetitions,
            min_ns: min,
            median_ns: median,
            p95_ns: quantile(&sorted, 95)?,
            max_ns: max,
            spread: spread(min, max, median),
        })
    }
}

/// What a set of samples came out to.
pub struct Statistics {
    pub samples: usize,
    pub repetitions: u32,
    pub min_ns: u64,
    pub median_ns: u64,
    pub p95_ns: u64,
    pub max_ns: u64,
    /// The distance from the fastest to the slowest sample, as a fraction of the median.
    ///
    /// A range rather than a standard deviation, because a benchmark's tail is what a
    /// reader needs and a handful of samples has no distribution to speak of.
    pub spread: f64,
}

impl Statistics {
    /// Bytes per second at the median, for an operation over `bytes` of input.
    ///
    /// A rate is a report, never an input to another measurement, so the precision a `f64`
    /// loses above 2^53 bytes or nanoseconds cannot reach a decision.
    #[allow(clippy::cast_precision_loss)]
    pub fn throughput(&self, bytes: u64) -> Option<f64> {
        if self.median_ns == 0 {
            return None;
        }
        Some(bytes as f64 * 1e9 / self.median_ns as f64)
    }
}

/// The value at a percentile of an already sorted set, by nearest rank.
fn quantile(sorted: &[u64], percent: u64) -> Option<u64> {
    let count = u64::try_from(sorted.len()).ok()?;
    if count == 0 {
        return None;
    }
    let rank = count
        .saturating_mul(percent)
        .div_euclid(100)
        .min(count.saturating_sub(1));
    sorted.get(usize::try_from(rank).ok()?).copied()
}

/// The precision a `f64` loses above 2^53 nanoseconds is 104 days, which no sample reaches.
#[allow(clippy::cast_precision_loss)]
fn spread(min: u64, max: u64, median: u64) -> f64 {
    if median == 0 {
        return 0.0;
    }
    max.saturating_sub(min) as f64 / median as f64
}

#[cfg(test)]
mod tests {
    use super::{BATCH_LIMIT, Samples, repetitions, time};

    #[test]
    fn an_operation_at_the_batch_target_runs_once() {
        assert_eq!(repetitions(1_000_000), 1);
        assert_eq!(repetitions(50_000_000), 1);
    }

    #[test]
    fn a_cheap_operation_buys_repetitions_up_to_the_limit() {
        assert_eq!(repetitions(1_000), 1_000);
        assert_eq!(repetitions(1), BATCH_LIMIT);
        assert_eq!(repetitions(0), BATCH_LIMIT);
        assert_eq!(repetitions(2), BATCH_LIMIT / 2);
    }

    #[test]
    fn a_sample_reports_the_cost_of_one_repetition_not_of_the_batch() {
        let mut samples = Samples::new(100);
        samples.push_batch(100_000);
        let statistics = samples.statistics();
        assert_eq!(statistics.as_ref().map(|s| s.median_ns), Some(1_000));
        assert_eq!(statistics.map(|s| s.repetitions), Some(100));
    }

    #[test]
    fn a_set_with_no_sample_has_no_statistics() {
        let samples = Samples::new(1);
        assert_eq!(samples.count(), 0);
        assert!(samples.statistics().is_none());
    }

    #[test]
    fn the_statistics_report_the_order_statistics_of_the_set() {
        let mut samples = Samples::new(1);
        for value in [40, 10, 30, 20, 50] {
            samples.push_batch(value);
        }
        let statistics = samples.statistics();
        assert_eq!(statistics.as_ref().map(|s| s.min_ns), Some(10));
        assert_eq!(statistics.as_ref().map(|s| s.median_ns), Some(30));
        assert_eq!(statistics.as_ref().map(|s| s.max_ns), Some(50));
        assert_eq!(statistics.as_ref().map(|s| s.p95_ns), Some(50));
        assert_eq!(statistics.map(|s| s.samples), Some(5));
    }

    #[test]
    fn the_spread_is_the_range_over_the_median() {
        let mut samples = Samples::new(1);
        for value in [100, 110, 120] {
            samples.push_batch(value);
        }
        let spread = samples.statistics().map_or(0.0, |s| s.spread);
        assert!((spread - 20.0 / 110.0).abs() < 1e-9, "{spread}");
    }

    #[test]
    fn a_set_of_one_sample_has_no_spread() {
        let mut samples = Samples::new(1);
        samples.push_batch(1_234);
        assert_eq!(samples.statistics().map(|s| s.spread), Some(0.0));
    }

    #[test]
    fn throughput_follows_the_median() {
        let mut samples = Samples::new(1);
        samples.push_batch(1_000_000_000);
        let rate = samples
            .statistics()
            .and_then(|s| s.throughput(1_000_000_000));
        assert_eq!(rate, Some(1e9));
    }

    #[test]
    fn a_timing_reports_what_the_work_produced() {
        let (produced, elapsed) = time(|| 21_u32.saturating_mul(2));
        assert_eq!(produced, 42);
        // An operation this small can land inside one clock tick, so the reading is a
        // duration rather than a positive one. That is what the batch calibration exists
        // for.
        assert!(elapsed < 1_000_000_000, "{elapsed}");
    }

    #[test]
    fn work_above_the_clock_quantum_reports_a_duration() {
        let (produced, elapsed) = time(|| {
            let mut total = 0_u64;
            for value in 0..200_000_u64 {
                total = total.wrapping_add(std::hint::black_box(value));
            }
            total
        });
        assert!(produced > 0);
        assert!(elapsed > 0, "a measurable loop reported no time");
    }
}
