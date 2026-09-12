//! Owns the comparison of one recorded campaign against another: which measured rows the two
//! records share, which metrics agree, and what the tolerance for agreement came from.
//!
//! A reproduction is not a rerun that printed the same text. It is a rerun whose numbers land
//! inside the variance the first record states. So a tolerance is never invented here. A
//! metric that is a property of the bytes has no variance, and any difference in it is a
//! finding. A metric that is a timing is judged against the spread the first record recorded
//! for the block it came from. A metric that moves between runs and that no record states a
//! variance for is reported with its difference and no verdict, because a verdict nothing
//! licenses is a guess.
//!
//! Two rows are compared only when they measured the same bytes, at the same operating point,
//! through the same competitor version, under the same integrity setting. A pair that fails
//! any of those is reported as not comparable rather than compared anyway.
//!
//! This module does not own how a result is read, and it emits no document.

use crate::error::{Error, Result};
use crate::parse::{Document, Host, Reading, Row};

/// Which timed pass a metric's tolerance is read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    Encode,
    Decode,
}

/// Where the tolerance for one metric comes from.
pub enum Tolerance {
    /// The metric is a property of the bytes. The same library over the same input produces
    /// the same number, so the tolerance is zero and any difference is a finding.
    Exact,
    /// The metric is a timing. The tolerance is the spread the first record states for the
    /// sampling block the number came from.
    Spread(Pass),
    /// The metric moves between runs and no record states by how much, so no verdict is
    /// reached and the difference is reported alone.
    Unrecorded(&'static str),
}

impl Tolerance {
    /// The one line the report states for this tolerance.
    pub fn source(&self) -> String {
        match self {
            Self::Exact => String::from(
                "zero. The number is a property of the bytes, so the same library over the \
                 same input produces it again.",
            ),
            Self::Spread(Pass::Encode) => {
                String::from("the encode spread the first record states for this block.")
            }
            Self::Spread(Pass::Decode) => {
                String::from("the decode spread the first record states for this block.")
            }
            Self::Unrecorded(reason) => format!("none. {reason}"),
        }
    }
}

/// The tolerance every metric a result document carries is judged against.
///
/// Every metric the parser declares appears here exactly once. A metric with no entry would
/// be a metric the comparison silently skipped, and a skipped metric reads as an agreeing
/// one.
const POLICY: &[(&str, Tolerance)] = &[
    ("compressed_bytes", Tolerance::Exact),
    ("compression_ratio", Tolerance::Exact),
    ("encode_throughput", Tolerance::Spread(Pass::Encode)),
    ("decode_throughput", Tolerance::Spread(Pass::Decode)),
    (
        "peak_rss",
        Tolerance::Unrecorded(
            "The figure is the peak resident set of the whole harness process, which depends \
             on every entry the segment read rather than on this row.",
        ),
    ),
    ("codec_owned_bytes", Tolerance::Exact),
    ("decoder_owned_bytes", Tolerance::Exact),
    ("allocations", Tolerance::Exact),
    (
        "encode_first_output_latency",
        Tolerance::Unrecorded(
            "The number comes from one streamed pass, taken once per size class, and no \
             record states a spread for it.",
        ),
    ),
    (
        "encode_streaming_latency",
        Tolerance::Unrecorded(
            "The number comes from one streamed pass, taken once per size class, and no \
             record states a spread for it.",
        ),
    ),
    (
        "parallel_scaling",
        Tolerance::Unrecorded(
            "The number is a ratio of two timed passes, each taken once, and no record states \
             a spread for it.",
        ),
    ),
    (
        "encode_cycles_per_byte",
        Tolerance::Unrecorded(COUNTER_ABSENT),
    ),
    (
        "decode_cycles_per_byte",
        Tolerance::Unrecorded(COUNTER_ABSENT),
    ),
    (
        "instructions_per_byte",
        Tolerance::Unrecorded(COUNTER_ABSENT),
    ),
    ("random_range_latency", Tolerance::Unrecorded(RANGE_ABSENT)),
    ("range_amplification", Tolerance::Unrecorded(RANGE_ABSENT)),
];

const COUNTER_ABSENT: &str = "The metric needs a performance monitor unit the process may read, and both records state \
     whether the host granted it.";

/// The host fields a comparison reads, in the order `values` returns them.
const FIELDS: &[&str] = &[
    "os",
    "os_version",
    "arch",
    "cpu_model",
    "cores",
    "memory",
    "rustc",
    "build_profile",
    "harness_version",
    "counter_backend",
    "counter_granted",
];

const RANGE_ABSENT: &str =
    "No codec path in either record produces a range read, so both records state a reason.";

/// What a comparison of one metric across two records concluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Both records carry a number and the difference is inside the tolerance.
    Agrees,
    /// Both records carry a number and the difference is outside the tolerance.
    Differs,
    /// Both records carry a number and no record states a variance to judge it against.
    Unjudged,
    /// Both records state a reason rather than a number.
    Absent,
    /// One record carries a number and the other states a reason.
    Changed,
}

impl Verdict {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Agrees => "agrees",
            Self::Differs => "differs",
            Self::Unjudged => "unjudged",
            Self::Absent => "absent in both",
            Self::Changed => "changed state",
        }
    }

    /// Whether this verdict is one the reproduction has to explain.
    pub const fn is_finding(self) -> bool {
        matches!(self, Self::Differs | Self::Changed)
    }
}

/// One metric, as the two records recorded it.
pub struct Comparison {
    pub metric: &'static str,
    pub baseline: Option<f64>,
    pub subject: Option<f64>,
    /// The difference over the first record's number, when both carry one.
    pub relative_difference: Option<f64>,
    pub tolerance: Option<f64>,
    pub tolerance_source: String,
    pub verdict: Verdict,
}

/// What identifies one measured row across two campaigns.
///
/// Two rows with this key measured the same competitor at the same operating point over the
/// same corpus entry with the same thread count. Nothing else in a record identifies a row.
#[derive(Clone, PartialEq, Eq)]
pub struct Key {
    pub codec: String,
    pub operating_point: String,
    pub entry: String,
    pub class: String,
    pub threads: u64,
}

impl Key {
    fn of(row: &Row) -> Self {
        Self {
            codec: row.codec.clone(),
            operating_point: row.operating_point.clone(),
            entry: row.entry.clone(),
            class: row.class.clone(),
            threads: row.threads,
        }
    }

    /// The one label a report names this row by.
    pub fn label(&self) -> String {
        format!(
            "{} {} over {} [{}, {} thread(s)]",
            self.codec, self.operating_point, self.entry, self.class, self.threads
        )
    }
}

/// One row the two records share.
pub struct Pair {
    pub key: Key,
    pub baseline_segment: String,
    pub subject_segment: String,
    pub baseline_encode_spread: Option<f64>,
    pub subject_encode_spread: Option<f64>,
    pub baseline_decode_spread: Option<f64>,
    pub subject_decode_spread: Option<f64>,
    /// Why the two rows do not measure equivalent work, when they do not. A blocked pair
    /// carries no comparison: a comparison of two different workloads is not a comparison.
    pub blocked: Option<String>,
    pub comparisons: Vec<Comparison>,
}

impl Pair {
    /// Every metric of this pair the reproduction has to explain.
    pub fn findings(&self) -> impl Iterator<Item = &Comparison> {
        self.comparisons
            .iter()
            .filter(|comparison| comparison.verdict.is_finding())
    }
}

/// One field two records disagree about.
pub struct Divergence {
    pub field: &'static str,
    pub baseline: String,
    pub subject: String,
}

/// What one campaign reproduced of another.
pub struct Reproduction {
    pub pairs: Vec<Pair>,
    pub only_in_baseline: Vec<Key>,
    pub only_in_subject: Vec<Key>,
    /// Every field of the host the two records disagree about. A difference here does not
    /// stop the comparison; it is stated, because a number measured on another machine is a
    /// number about another machine.
    pub host: Vec<Divergence>,
}

impl Reproduction {
    /// How many of this reproduction's comparisons reached each verdict.
    pub fn count(&self, verdict: Verdict) -> usize {
        self.pairs
            .iter()
            .flat_map(|pair| pair.comparisons.iter())
            .filter(|comparison| comparison.verdict == verdict)
            .fold(0, |total, _| total.saturating_add(1))
    }

    pub fn blocked(&self) -> impl Iterator<Item = &Pair> {
        self.pairs.iter().filter(|pair| pair.blocked.is_some())
    }

    /// Whether every row matched, every pair was comparable, and every judged metric agreed.
    pub fn reproduced(&self) -> bool {
        self.only_in_baseline.is_empty()
            && self.only_in_subject.is_empty()
            && self.blocked().count() == 0
            && self.count(Verdict::Differs) == 0
            && self.count(Verdict::Changed) == 0
    }

    /// The one line that states what this reproduction found.
    pub fn statement(&self) -> String {
        if self.reproduced() {
            return format!(
                "Every one of the {} shared rows was comparable, and every judged metric \
                 landed inside the variance the first record states.",
                self.pairs.len()
            );
        }
        format!(
            "{} rows compared. {} metrics differ, {} changed state, {} rows are not \
             comparable, {} rows are in the first record alone, and {} are in the second \
             alone.",
            self.pairs.len(),
            self.count(Verdict::Differs),
            self.count(Verdict::Changed),
            self.blocked().count(),
            self.only_in_baseline.len(),
            self.only_in_subject.len(),
        )
    }
}

/// Compares a second campaign against the first, row by row and metric by metric.
///
/// # Errors
///
/// Fails when either side holds one row twice, because two measurements of the same work are
/// two measurements and a comparison that held both would compare a run against itself.
pub fn compare(baseline: &[Document], subject: &[Document]) -> Result<Reproduction> {
    let first = index("the first record", baseline)?;
    let second = index("the second record", subject)?;

    let mut pairs: Vec<Pair> = Vec::new();
    let mut only_in_baseline: Vec<Key> = Vec::new();
    let mut matched: Vec<usize> = Vec::new();

    for entry in &first {
        let found = second
            .iter()
            .enumerate()
            .find(|(index, other)| other.key == entry.key && !matched.contains(index));
        match found {
            Some((index, other)) => {
                matched.push(index);
                pairs.push(pair(entry, other));
            }
            None => only_in_baseline.push(entry.key.clone()),
        }
    }

    let only_in_subject = second
        .iter()
        .enumerate()
        .filter(|(index, _)| !matched.contains(index))
        .map(|(_, entry)| entry.key.clone())
        .collect();

    Ok(Reproduction {
        pairs,
        only_in_baseline,
        only_in_subject,
        host: hosts(baseline, subject),
    })
}

/// The tolerance policy, as a report states it.
pub fn policy() -> impl Iterator<Item = (&'static str, String)> {
    POLICY
        .iter()
        .map(|(metric, tolerance)| (*metric, tolerance.source()))
}

/// One row of one record, with what identifies it and where it came from.
struct Indexed<'a> {
    key: Key,
    row: &'a Row,
    segment: String,
}

/// Every measured row of one side, rejecting a side that holds one row twice.
fn index<'a>(side: &str, documents: &'a [Document]) -> Result<Vec<Indexed<'a>>> {
    let mut rows: Vec<Indexed<'a>> = Vec::new();
    for document in documents {
        for row in &document.rows {
            let key = Key::of(row);
            if rows.iter().any(|held| held.key == key) {
                return Err(Error::report(
                    String::from(side),
                    format!(
                        "holds {} twice. Two measurements of one operating point are two \
                         measurements, and a comparison cannot read one of them as the other.",
                        key.label()
                    ),
                ));
            }
            rows.push(Indexed {
                key,
                row,
                segment: document.segment.clone(),
            });
        }
    }
    if rows.is_empty() {
        return Err(Error::report(
            String::from(side),
            String::from("holds no measured row, so there is nothing to compare"),
        ));
    }
    Ok(rows)
}

fn pair(first: &Indexed<'_>, second: &Indexed<'_>) -> Pair {
    let blocked = equivalent(first.row, second.row);
    let comparisons = if blocked.is_some() {
        Vec::new()
    } else {
        POLICY
            .iter()
            .map(|(metric, tolerance)| comparison(metric, tolerance, first.row, second.row))
            .collect()
    };
    Pair {
        key: first.key.clone(),
        baseline_segment: first.segment.clone(),
        subject_segment: second.segment.clone(),
        baseline_encode_spread: first.row.encode_spread,
        subject_encode_spread: second.row.encode_spread,
        baseline_decode_spread: first.row.decode_spread,
        subject_decode_spread: second.row.decode_spread,
        blocked,
        comparisons,
    }
}

/// Why two rows do not measure equivalent work, when they do not.
///
/// A comparison measures equivalent work or it measures nothing. Different input bytes, a
/// different competitor version, a different integrity setting, and an unverified round trip
/// each break that, and each one is stated rather than absorbed into a tolerance.
fn equivalent(first: &Row, second: &Row) -> Option<String> {
    if first.input_digest != second.input_digest {
        return Some(format!(
            "the two records measured different bytes for this entry: {} against {}",
            first.input_digest, second.input_digest
        ));
    }
    if first.input_bytes != second.input_bytes {
        return Some(format!(
            "the two records measured {} bytes against {} bytes",
            first.input_bytes, second.input_bytes
        ));
    }
    if first.version != second.version {
        return Some(format!(
            "the two records measured different competitor versions: {} against {}",
            first.version, second.version
        ));
    }
    if first.integrity != second.integrity {
        return Some(format!(
            "the two records ran different integrity settings: {} against {}",
            first.integrity, second.integrity
        ));
    }
    if !first.round_trip_verified || !second.round_trip_verified {
        return Some(String::from(
            "a round trip was not verified, so no number from this row means anything",
        ));
    }
    None
}

fn comparison(
    metric: &'static str,
    tolerance: &Tolerance,
    first: &Row,
    second: &Row,
) -> Comparison {
    let baseline = first.reading(metric).and_then(Reading::value);
    let subject = second.reading(metric).and_then(Reading::value);
    let source = tolerance.source();

    let (Some(recorded), Some(measured)) = (baseline, subject) else {
        let verdict = if baseline.is_none() && subject.is_none() {
            Verdict::Absent
        } else {
            Verdict::Changed
        };
        return Comparison {
            metric,
            baseline,
            subject,
            relative_difference: None,
            tolerance: None,
            tolerance_source: source,
            verdict,
        };
    };

    let relative = relative_difference(recorded, measured);
    let limit = match tolerance {
        Tolerance::Exact => Some(0.0),
        Tolerance::Spread(Pass::Encode) => first.encode_spread,
        Tolerance::Spread(Pass::Decode) => first.decode_spread,
        Tolerance::Unrecorded(_) => None,
    };
    let verdict = match (limit, relative) {
        (Some(limit), Some(relative)) => {
            if relative <= limit {
                Verdict::Agrees
            } else {
                Verdict::Differs
            }
        }
        (Some(limit), None) => {
            // The first record carries zero, so a relative difference has no denominator.
            if (measured - recorded).abs() <= limit {
                Verdict::Agrees
            } else {
                Verdict::Differs
            }
        }
        (None, _) => Verdict::Unjudged,
    };
    Comparison {
        metric,
        baseline,
        subject,
        relative_difference: relative,
        tolerance: limit,
        tolerance_source: source,
        verdict,
    }
}

/// The difference over the first record's number, or none when that number is zero.
fn relative_difference(recorded: f64, measured: f64) -> Option<f64> {
    if recorded == 0.0 {
        return None;
    }
    Some((measured - recorded).abs() / recorded.abs())
}

/// Every field of the host the two records disagree about.
///
/// Each document carries its own host, so a side is read as every value its documents state
/// for a field rather than as the first one. A side that states two is a campaign measured
/// across two machines, and it is reported here rather than reduced to whichever document
/// came first.
fn hosts(baseline: &[Document], subject: &[Document]) -> Vec<Divergence> {
    let mut found: Vec<Divergence> = Vec::new();
    for (index, field) in FIELDS.iter().enumerate() {
        let recorded = stated(baseline, index);
        let measured = stated(subject, index);
        // A side that states two values for one field disagrees with itself, which is as
        // much a divergence as the two sides disagreeing with each other.
        if recorded != measured || recorded.len() > 1 || measured.len() > 1 {
            found.push(Divergence {
                field,
                baseline: recorded.join(", "),
                subject: measured.join(", "),
            });
        }
    }
    found
}

/// Every value one side's documents state for one field, in the order they first state it.
fn stated(documents: &[Document], index: usize) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    for document in documents {
        if let Some(value) = values_of(&document.host).into_iter().nth(index)
            && !values.contains(&value)
        {
            values.push(value);
        }
    }
    values
}

/// One host, at the depth a comparison has to agree about, in the order `FIELDS` names.
///
/// A field is read by its position in both lists, so the two orders are one order.
fn values_of(host: &Host) -> Vec<String> {
    vec![
        host.os.clone(),
        host.os_version.clone(),
        host.arch.clone(),
        host.cpu_model.clone(),
        host.cores.clone(),
        host.memory.clone(),
        host.rustc.clone(),
        host.build_profile.clone(),
        host.harness_version.clone(),
        host.counter_backend.clone(),
        host.counter_granted.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::Value;

    use super::{POLICY, Reproduction, Verdict, compare, relative_difference};
    use crate::parse::{self, METRICS, fixture};

    fn parsed(value: &Value) -> parse::Document {
        let read = parse::document(Path::new("fixture.json"), value);
        assert!(read.is_ok(), "the fixture must parse");
        read.unwrap_or_else(|_| unreachable!("checked above"))
    }

    /// One row of one competitor, with a ratio, an encode rate, and the spread the record
    /// states for the block that rate came from.
    fn one(ratio: f64, throughput: f64, spread: f64) -> parse::Document {
        let metrics = fixture::metrics(&[
            ("compression_ratio", fixture::measured(ratio, "ratio")),
            (
                "encode_throughput",
                fixture::measured(throughput, "bytes per second"),
            ),
            ("peak_rss", fixture::measured(4_096.0, "bytes")),
        ]);
        let mut row = fixture::row("lz4", "fast", "text-a", &metrics);
        if let Some(object) = row.as_object_mut() {
            let _ = object.insert(String::from("encode_samples"), fixture::samples(spread));
        }
        parsed(&fixture::document("gate", &[row]))
    }

    fn compared(first: parse::Document, second: parse::Document) -> Option<Reproduction> {
        compare(&[first], &[second]).ok()
    }

    #[test]
    fn the_policy_covers_every_metric_a_result_carries_exactly_once() {
        for metric in METRICS {
            let held = POLICY
                .iter()
                .filter(|(name, _)| name == metric)
                .fold(0_usize, |total, _| total.saturating_add(1));
            assert_eq!(held, 1, "{metric} has {held} tolerances");
        }
        assert_eq!(POLICY.len(), METRICS.len());
    }

    #[test]
    fn a_rerun_inside_the_recorded_spread_reproduces() {
        let found = compared(one(2.0, 1_000.0, 0.10), one(2.0, 1_050.0, 0.10));
        assert!(found.is_some_and(|found| found.reproduced()));
    }

    #[test]
    fn a_timing_outside_the_recorded_spread_is_a_finding() {
        let found = compared(one(2.0, 1_000.0, 0.05), one(2.0, 1_500.0, 0.05));
        assert_eq!(found.map(|f| f.count(Verdict::Differs)), Some(1));
    }

    #[test]
    fn a_ratio_that_moves_at_all_is_a_finding() {
        let found = compared(one(2.0, 1_000.0, 0.50), one(2.0001, 1_000.0, 0.50));
        assert_eq!(found.map(|f| f.count(Verdict::Differs)), Some(1));
    }

    #[test]
    fn a_metric_no_record_states_a_variance_for_reaches_no_verdict() {
        let found = compared(one(2.0, 1_000.0, 0.10), one(2.0, 2_000.0, 0.10));
        assert_eq!(
            found.map(|f| f.count(Verdict::Unjudged)),
            Some(1),
            "the resident set is the one metric with a number and no recorded variance"
        );
    }

    #[test]
    fn a_metric_absent_from_both_records_is_neither_agreement_nor_finding() {
        let found = compared(one(2.0, 1_000.0, 0.10), one(2.0, 1_000.0, 0.10));
        assert_eq!(found.map(|f| f.count(Verdict::Absent)), Some(13));
    }

    #[test]
    fn a_metric_that_gained_a_number_is_a_finding() {
        let first = one(2.0, 1_000.0, 0.10);
        let metrics = fixture::metrics(&[
            ("compression_ratio", fixture::measured(2.0, "ratio")),
            (
                "encode_throughput",
                fixture::measured(1_000.0, "bytes per second"),
            ),
            ("peak_rss", fixture::measured(4_096.0, "bytes")),
            ("allocations", fixture::measured(7.0, "allocations")),
        ]);
        let mut row = fixture::row("lz4", "fast", "text-a", &metrics);
        if let Some(object) = row.as_object_mut() {
            let _ = object.insert(String::from("encode_samples"), fixture::samples(0.10));
        }
        let second = parsed(&fixture::document("gate", &[row]));
        let found = compared(first, second);
        assert_eq!(found.map(|f| f.count(Verdict::Changed)), Some(1));
    }

    #[test]
    fn a_row_only_one_record_holds_is_reported_rather_than_dropped() {
        let metrics = fixture::metrics(&[("compression_ratio", fixture::measured(3.0, "ratio"))]);
        let other = fixture::row("zstd", "3", "text-a", &metrics);
        let second = parsed(&fixture::document("gate", &[other]));
        let found = compared(one(2.0, 1_000.0, 0.10), second);
        assert_eq!(found.as_ref().map(|f| f.only_in_baseline.len()), Some(1));
        assert_eq!(found.as_ref().map(|f| f.only_in_subject.len()), Some(1));
        assert!(found.is_some_and(|found| !found.reproduced()));
    }

    #[test]
    fn two_rows_over_different_bytes_are_not_compared() {
        let metrics = fixture::metrics(&[("compression_ratio", fixture::measured(2.0, "ratio"))]);
        let mut row = fixture::row("lz4", "fast", "text-a", &metrics);
        if let Some(object) = row.as_object_mut() {
            let _ = object.insert(
                String::from("input_digest"),
                Value::from("sha256:another-corpus"),
            );
        }
        let second = parsed(&fixture::document("gate", &[row]));
        let found = compared(one(2.0, 1_000.0, 0.10), second);
        assert_eq!(found.as_ref().map(|f| f.blocked().count()), Some(1));
        assert!(found.is_some_and(|found| !found.reproduced()));
    }

    #[test]
    fn one_record_holding_a_row_twice_is_refused() {
        let metrics = fixture::metrics(&[("compression_ratio", fixture::measured(2.0, "ratio"))]);
        let row = fixture::row("lz4", "fast", "text-a", &metrics);
        let twice = parsed(&fixture::document("gate", &[row.clone(), row]));
        assert!(compare(&[twice], &[one(2.0, 1_000.0, 0.10)]).is_err());
    }

    #[test]
    fn the_host_field_names_and_their_values_are_one_order() {
        let document = one(2.0, 1_000.0, 0.10);
        assert_eq!(super::FIELDS.len(), super::values_of(&document.host).len());
    }

    #[test]
    fn two_records_on_two_hosts_name_every_field_they_disagree_about() {
        let mut value = fixture::document("gate", &[]);
        if let Some(environment) = value
            .as_object_mut()
            .and_then(|d| d.get_mut("environment"))
            .and_then(Value::as_object_mut)
        {
            let _ = environment.insert(String::from("cpu_model"), Value::from("another CPU"));
        }
        let elsewhere = parsed(&value);
        let found = compare(
            &[one(2.0, 1_000.0, 0.10)],
            &[elsewhere, one(2.0, 1_000.0, 0.10)],
        );
        let fields: Vec<&str> = found
            .as_ref()
            .map(|f| f.host.iter().map(|d| d.field).collect())
            .unwrap_or_default();
        assert_eq!(fields, vec!["cpu_model"], "{fields:?}");
    }

    #[test]
    fn a_relative_difference_against_zero_has_no_denominator() {
        assert_eq!(relative_difference(0.0, 1.0), None);
        assert_eq!(relative_difference(2.0, 3.0), Some(0.5));
    }
}
