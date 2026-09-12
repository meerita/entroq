//! Owns the frontier: the axes a comparison is read on, which rows share a chart, and which
//! plotted point another point dominates.
//!
//! A point is dominated when another point on the same chart is equal or better in every
//! dimension of the axis and strictly better in at least one. Equal in every dimension is not
//! dominated, so a tie marks nothing.
//!
//! Two rows share a chart only when they measured the same bytes, at the same thread count,
//! at the same tier, on the same host. A chart that mixed two tiers would let a development
//! number be read beside a published one, and a chart that mixed two inputs would compare
//! work that is not the same work.
//!
//! An axis whose metric no row carries a number for reports no data, with the reasons the
//! results themselves gave. It never stands a different metric in its place: elapsed time
//! against ratio is already the throughput axis, and plotting it a second time under a
//! counter's name would invent a result.
//!
//! This module does not own how a result is read or how a report is written.

use crate::error::{Error, Result};
use crate::parse::{Document, Host, Reading, Row};

/// The dimension every axis shares. It is exact: a compressed length is what the library
/// produced, not a sample, so two ratios that differ differ outside any measurement noise.
pub const RATIO: &str = "compression_ratio";

/// Which direction is better on an axis.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Better {
    Higher,
    Lower,
}

impl Better {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Higher => "higher",
            Self::Lower => "lower",
        }
    }
}

/// Which sampling's reported spread bounds the noise in an axis's metric.
///
/// A metric a library reports directly, such as the size of its own state, carries none.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Noise {
    None,
    Encode,
    Decode,
}

/// One axis of the frontier: compression ratio against one other metric.
pub struct Axis {
    pub name: &'static str,
    pub title: &'static str,
    pub metric: &'static str,
    pub better: Better,
    pub noise: Noise,
}

/// The axes a comparison is read on.
///
/// The two counter axes exist and carry no data on a host that denies the performance
/// monitor unit. They stay declared so a reader sees the axis and the reason it is empty,
/// rather than a frontier that quietly has three dimensions instead of five.
pub const AXES: &[Axis] = &[
    Axis {
        name: "ratio-against-encode-throughput",
        title: "ratio against encode throughput",
        metric: "encode_throughput",
        better: Better::Higher,
        noise: Noise::Encode,
    },
    Axis {
        name: "ratio-against-decode-throughput",
        title: "ratio against decode throughput",
        metric: "decode_throughput",
        better: Better::Higher,
        noise: Noise::Decode,
    },
    Axis {
        name: "ratio-against-encode-cpu",
        title: "ratio against encode CPU",
        metric: "encode_cycles_per_byte",
        better: Better::Lower,
        noise: Noise::None,
    },
    Axis {
        name: "ratio-against-decode-cpu",
        title: "ratio against decode CPU",
        metric: "decode_cycles_per_byte",
        better: Better::Lower,
        noise: Noise::None,
    },
    // The memory dimension is the codec-owned figure, not the process resident set. A
    // resident set measured in one process per competitor carries the harness buffers and
    // the corpus entry as well, so it would compare the harness across charts.
    Axis {
        name: "ratio-against-memory",
        title: "ratio against memory",
        metric: "codec_owned_bytes",
        better: Better::Lower,
        noise: Noise::None,
    },
];

/// One plotted point, and what the frontier found about it.
pub struct Plotted {
    pub codec: String,
    pub display_name: String,
    pub version: String,
    pub operating_point: String,
    /// The call the result recorded for this axis's metric.
    pub method: String,
    /// The tier that produced this point, carried beside it so no chart can be read without
    /// it.
    pub tier: String,
    pub source: String,
    pub ratio: f64,
    pub value: f64,
    pub unit: String,
    /// The points that dominate this one, by label. Empty means it is on the frontier.
    pub dominated_by: Vec<String>,
    /// Whether every dominance verdict over this point rests on a margin no larger than the
    /// spread the samples behind it reported.
    pub inside_reported_spread: bool,
}

impl Plotted {
    pub fn label(&self) -> String {
        format!("{} {}", self.display_name, self.operating_point)
    }

    pub const fn dominated(&self) -> bool {
        !self.dominated_by.is_empty()
    }
}

/// A row that this axis could not plot, and the reason the result gave.
pub struct Absent {
    pub label: String,
    pub metric: &'static str,
    pub reason: String,
}

/// One axis of one chart.
pub struct Plot {
    pub axis: &'static Axis,
    pub points: Vec<Plotted>,
    pub absent: Vec<Absent>,
}

impl Plot {
    pub const fn has_data(&self) -> bool {
        !self.points.is_empty()
    }

    /// Why this axis carries no point, taken from the reasons the results recorded.
    pub fn no_data(&self) -> Option<String> {
        if self.has_data() {
            return None;
        }
        let mut reasons: Vec<&str> = Vec::new();
        for absent in &self.absent {
            if !reasons.contains(&absent.reason.as_str()) {
                reasons.push(absent.reason.as_str());
            }
        }
        if reasons.is_empty() {
            return Some(String::from(
                "no measured row reached this axis, so it carries no point.",
            ));
        }
        Some(reasons.join(" "))
    }
}

/// Every point on one chart shares these, so a comparison across them is a comparison of the
/// same work.
pub struct Subject {
    pub tier: String,
    pub tier_is_publication: bool,
    pub tier_licence: String,
    pub entry: String,
    pub group: String,
    pub class: String,
    pub input_bytes: u64,
    pub input_digest: String,
    pub threads: u64,
    pub host: Host,
}

/// One chart: one subject, and the axes read over it.
pub struct Chart {
    pub subject: Subject,
    pub plots: Vec<Plot>,
}

/// One parsed row, with the dimension every axis is read against.
struct Candidate<'a> {
    row: &'a Row,
    tier: &'a str,
    source: String,
    ratio: f64,
}

/// The rows of one chart, and what they all share.
struct Group<'a> {
    subject: Subject,
    members: Vec<Candidate<'a>>,
}

/// Builds every chart the parsed documents support.
///
/// Rows are grouped by tier, host, corpus entry, and thread count. A row whose result
/// recorded no compression ratio reaches no axis, because every axis is read against it.
///
/// # Errors
///
/// Fails when two rows would put one operating point on one chart twice. Two measurements of
/// the same work are two measurements, and a frontier that held both would compare a codec
/// against itself.
pub fn charts(documents: &[Document]) -> Result<Vec<Chart>> {
    let mut groups: Vec<Group> = Vec::new();
    for document in documents {
        for row in &document.rows {
            let Some(ratio) = row.reading(RATIO).and_then(Reading::value) else {
                continue;
            };
            let candidate = Candidate {
                row,
                tier: document.tier.as_str(),
                source: document.path.display().to_string(),
                ratio,
            };
            match groups.iter_mut().find(|group| holds(group, document, row)) {
                Some(group) => {
                    if let Some(twin) = group.members.iter().find(|member| {
                        member.row.codec == row.codec
                            && member.row.operating_point == row.operating_point
                    }) {
                        return Err(duplicate(&candidate, twin));
                    }
                    group.members.push(candidate);
                }
                None => groups.push(Group {
                    subject: subject(document, row),
                    members: vec![candidate],
                }),
            }
        }
    }
    let mut charts: Vec<Chart> = groups
        .into_iter()
        .map(|group| Chart {
            plots: AXES.iter().map(|axis| plot(axis, &group.members)).collect(),
            subject: group.subject,
        })
        .collect();
    charts.sort_by_key(|chart| order(&chart.subject));
    Ok(charts)
}

fn duplicate(candidate: &Candidate, twin: &Candidate) -> Error {
    Error::report(
        format!(
            "{} at {} over {}",
            candidate.row.display_name, candidate.row.operating_point, candidate.row.entry
        ),
        format!(
            "is measured twice in the results given: once in {} and once in {}. Two \
             measurements of the same work are two measurements, and a frontier cannot hold \
             one operating point twice. Name one campaign, or one attempt inside it.",
            twin.source, candidate.source
        ),
    )
}

fn order(subject: &Subject) -> (String, String, String, u64) {
    (
        subject.tier.clone(),
        subject.class.clone(),
        subject.entry.clone(),
        subject.threads,
    )
}

fn holds(group: &Group, document: &Document, row: &Row) -> bool {
    group.subject.tier == document.tier
        && group.subject.entry == row.entry
        && group.subject.input_digest == row.input_digest
        && group.subject.threads == row.threads
        && host_key(&group.subject.host) == host_key(&document.host)
}

/// The host facts two results must agree on before their numbers may share a chart.
pub fn host_key(host: &Host) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
        host.os,
        host.os_version,
        host.arch,
        host.cpu_model,
        host.cores,
        host.memory,
        host.rustc,
        host.build_profile,
        host.harness_version
    )
}

fn subject(document: &Document, row: &Row) -> Subject {
    Subject {
        tier: document.tier.clone(),
        tier_is_publication: document.tier_is_publication,
        tier_licence: document.tier_licence.clone(),
        entry: row.entry.clone(),
        group: row.group.clone(),
        class: row.class.clone(),
        input_bytes: row.input_bytes,
        input_digest: row.input_digest.clone(),
        threads: row.threads,
        host: document.host.clone(),
    }
}

fn plot(axis: &'static Axis, group: &[Candidate]) -> Plot {
    let mut points = Vec::new();
    let mut absent = Vec::new();
    for candidate in group {
        // A number from a round trip nobody verified describes no codec, so it is named
        // rather than plotted.
        if !candidate.row.round_trip_verified {
            absent.push(Absent {
                label: label(candidate),
                metric: axis.metric,
                reason: String::from(
                    "the result records that this row's round trip was not verified, so no \
                     number from it is plotted",
                ),
            });
            continue;
        }
        match candidate.row.reading(axis.metric) {
            Some(Reading::Measured {
                value,
                unit,
                method,
            }) => points.push(Plotted {
                codec: candidate.row.codec.clone(),
                display_name: candidate.row.display_name.clone(),
                version: candidate.row.version.clone(),
                operating_point: candidate.row.operating_point.clone(),
                method: method.clone(),
                tier: String::from(candidate.tier),
                source: candidate.source.clone(),
                ratio: candidate.ratio,
                value: *value,
                unit: unit.clone(),
                dominated_by: Vec::new(),
                inside_reported_spread: false,
            }),
            Some(Reading::Unavailable { reason }) => absent.push(Absent {
                label: label(candidate),
                metric: axis.metric,
                reason: reason.clone(),
            }),
            None => absent.push(Absent {
                label: label(candidate),
                metric: axis.metric,
                reason: String::from("the result carries no reading under this name"),
            }),
        }
    }
    let spreads: Vec<Option<f64>> = group
        .iter()
        .filter(|candidate| {
            candidate.row.round_trip_verified
                && matches!(
                    candidate.row.reading(axis.metric),
                    Some(Reading::Measured { .. })
                )
        })
        .map(|candidate| match axis.noise {
            Noise::None => None,
            Noise::Encode => candidate.row.encode_spread,
            Noise::Decode => candidate.row.decode_spread,
        })
        .collect();
    mark(&mut points, &spreads, axis.better);
    points.sort_by_key(Plotted::label);
    absent.sort_by(|a, b| a.label.cmp(&b.label));
    Plot {
        axis,
        points,
        absent,
    }
}

/// Marks every point another point dominates.
fn mark(points: &mut [Plotted], spreads: &[Option<f64>], better: Better) {
    let mut verdicts: Vec<(Vec<String>, bool)> = Vec::with_capacity(points.len());
    for (index, point) in points.iter().enumerate() {
        let mut by = Vec::new();
        let mut inside = true;
        for (other_index, other) in points.iter().enumerate() {
            if other_index == index || !dominates(other, point, better) {
                continue;
            }
            let noise = spread_of(spreads, index).max(spread_of(spreads, other_index));
            if !decided_inside(other, point, better, noise) {
                inside = false;
            }
            by.push(other.label());
        }
        verdicts.push((by, inside));
    }
    for (point, (by, inside)) in points.iter_mut().zip(verdicts) {
        point.inside_reported_spread = !by.is_empty() && inside;
        point.dominated_by = by;
    }
}

fn label(candidate: &Candidate) -> String {
    format!(
        "{} {}",
        candidate.row.display_name, candidate.row.operating_point
    )
}

fn spread_of(spreads: &[Option<f64>], index: usize) -> f64 {
    spreads.get(index).copied().flatten().unwrap_or(0.0)
}

/// Whether `a` is equal or better in both dimensions and strictly better in at least one.
fn dominates(a: &Plotted, b: &Plotted, better: Better) -> bool {
    let ratio_not_worse = a.ratio >= b.ratio;
    let value_not_worse = match better {
        Better::Higher => a.value >= b.value,
        Better::Lower => a.value <= b.value,
    };
    let strictly = a.ratio > b.ratio || strictly_better(a.value, b.value, better);
    ratio_not_worse && value_not_worse && strictly
}

fn strictly_better(a: f64, b: f64, better: Better) -> bool {
    match better {
        Better::Higher => a > b,
        Better::Lower => a < b,
    }
}

/// Whether every dimension that decided this verdict moved by less than the measurement
/// spread behind it.
///
/// The ratio dimension is exact, so a verdict that ratio decided is never inside noise.
fn decided_inside(a: &Plotted, b: &Plotted, better: Better, noise: f64) -> bool {
    if a.ratio > b.ratio {
        return false;
    }
    if !strictly_better(a.value, b.value, better) {
        return false;
    }
    relative(a.value, b.value) <= noise
}

fn relative(a: f64, b: f64) -> f64 {
    let reference = a.abs().max(b.abs());
    if reference == 0.0 {
        return 0.0;
    }
    (a - b).abs() / reference
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::{Value, json};

    use super::{AXES, Better, Chart, Plot, RATIO, charts};
    use crate::parse::{self, fixture};

    /// One row at an operating point, with a ratio, an encode rate, and a state size.
    fn row(codec: &str, ratio: f64, encode: f64, memory: f64) -> Value {
        fixture::row(
            codec,
            "default",
            "fixture-entry",
            &fixture::metrics(&[
                (RATIO, fixture::measured(ratio, "ratio")),
                (
                    "encode_throughput",
                    fixture::measured(encode, "bytes per second"),
                ),
                ("codec_owned_bytes", fixture::measured(memory, "bytes")),
            ]),
        )
    }

    fn parsed(tier: &str, rows: &[Value]) -> parse::Document {
        let value = fixture::document(tier, rows);
        let read = parse::document(Path::new("fixture.json"), &value);
        assert!(read.is_ok(), "the fixture must parse");
        read.unwrap_or_else(|_| unreachable!("checked above"))
    }

    fn built(documents: &[parse::Document]) -> Vec<Chart> {
        charts(documents).unwrap_or_default()
    }

    fn axis<'a>(chart: &'a Chart, name: &str) -> Option<&'a Plot> {
        chart.plots.iter().find(|plot| plot.axis.name == name)
    }

    /// Every point of one axis, as a label and whether it is marked dominated.
    fn marks(charts: &[Chart], name: &str) -> Vec<(String, bool)> {
        charts
            .first()
            .and_then(|chart| axis(chart, name))
            .map(|plot| {
                plot.points
                    .iter()
                    .map(|point| (point.label(), point.dominated()))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn the_five_axes_are_ratio_against_five_distinct_metrics() {
        assert_eq!(AXES.len(), 5);
        for (index, axis) in AXES.iter().enumerate() {
            assert_ne!(
                axis.metric, RATIO,
                "{} plots ratio against itself",
                axis.name
            );
            assert!(
                !AXES
                    .iter()
                    .skip(index.saturating_add(1))
                    .any(|other| other.metric == axis.metric),
                "{} is plotted twice",
                axis.metric
            );
        }
    }

    #[test]
    fn a_cpu_axis_reads_a_counter_and_never_a_time() {
        for name in ["ratio-against-encode-cpu", "ratio-against-decode-cpu"] {
            let metric = AXES
                .iter()
                .find(|axis| axis.name == name)
                .map(|axis| axis.metric);
            assert!(
                metric.is_some_and(|metric| metric.ends_with("_cycles_per_byte")),
                "{name} reads {metric:?}"
            );
        }
        for name in ["encode_throughput", "decode_throughput"] {
            let axes = AXES.iter().filter(|axis| axis.metric == name).count();
            assert_eq!(axes, 1, "{name} appears on more than one axis");
        }
    }

    #[test]
    fn a_point_worse_in_both_dimensions_is_the_only_one_marked() {
        // Fast is faster, Dense compresses harder, and Poor is beaten by both.
        let charts = built(&[parsed(
            "dev",
            &[
                row("fast", 2.0, 900.0, 100.0),
                row("dense", 4.0, 100.0, 100.0),
                row("poor", 1.5, 50.0, 100.0),
            ],
        )]);
        let marked = marks(&charts, "ratio-against-encode-throughput");
        assert_eq!(
            marked,
            vec![
                (String::from("dense default"), false),
                (String::from("fast default"), false),
                (String::from("poor default"), true),
            ]
        );
    }

    #[test]
    fn a_point_equal_in_every_dimension_is_not_dominated() {
        let charts = built(&[parsed(
            "dev",
            &[row("one", 2.0, 100.0, 100.0), row("two", 2.0, 100.0, 100.0)],
        )]);
        let marked = marks(&charts, "ratio-against-encode-throughput");
        assert!(
            marked.iter().all(|(_, dominated)| !dominated),
            "a tie marked something: {marked:?}"
        );
    }

    #[test]
    fn a_point_better_in_one_dimension_and_equal_in_the_other_dominates() {
        let charts = built(&[parsed(
            "dev",
            &[
                row("same-ratio-faster", 2.0, 200.0, 100.0),
                row("same-ratio-slower", 2.0, 100.0, 100.0),
            ],
        )]);
        let marked = marks(&charts, "ratio-against-encode-throughput");
        assert_eq!(
            marked,
            vec![
                (String::from("same-ratio-faster default"), false),
                (String::from("same-ratio-slower default"), true),
            ]
        );
    }

    #[test]
    fn a_lower_is_better_axis_marks_the_point_that_holds_more_memory() {
        let charts = built(&[parsed(
            "dev",
            &[
                row("small-state", 2.0, 100.0, 1_000.0),
                row("large-state", 2.0, 100.0, 9_000.0),
            ],
        )]);
        let marked = marks(&charts, "ratio-against-memory");
        assert_eq!(
            marked,
            vec![
                (String::from("large-state default"), true),
                (String::from("small-state default"), false),
            ]
        );
        let better = AXES
            .iter()
            .find(|axis| axis.name == "ratio-against-memory")
            .map(|axis| axis.better);
        assert!(better.is_some_and(|better| better == Better::Lower));
    }

    #[test]
    fn an_axis_no_result_measured_reports_no_data_and_the_reason_the_result_gave() {
        let charts = built(&[parsed("dev", &[row("one", 2.0, 100.0, 100.0)])]);
        for name in ["ratio-against-encode-cpu", "ratio-against-decode-cpu"] {
            let plot = charts.first().and_then(|chart| axis(chart, name));
            assert!(plot.is_some_and(|plot| !plot.has_data()), "{name} has data");
            let reason = plot.and_then(Plot::no_data).unwrap_or_default();
            assert!(reason.contains("states no number"), "{name}: {reason}");
            assert!(
                plot.is_some_and(|plot| plot.absent.len() == 1),
                "{name} names nobody"
            );
        }
    }

    #[test]
    fn a_dev_point_and_a_publication_point_never_share_a_chart() {
        let charts = built(&[
            parsed("dev", &[row("one", 2.0, 100.0, 100.0)]),
            parsed("publication", &[row("one", 2.0, 100.0, 100.0)]),
        ]);
        assert_eq!(charts.len(), 2);
        let tiers: Vec<&str> = charts
            .iter()
            .map(|chart| chart.subject.tier.as_str())
            .collect();
        assert_eq!(tiers, vec!["dev", "publication"]);
        for chart in &charts {
            for plot in &chart.plots {
                for point in &plot.points {
                    assert_eq!(point.tier, chart.subject.tier);
                }
            }
        }
    }

    #[test]
    fn two_hosts_never_share_a_chart() {
        let mut other = fixture::document("dev", &[row("one", 2.0, 100.0, 100.0)]);
        if let Some(environment) = other.get_mut("environment").and_then(Value::as_object_mut) {
            let _ = environment.insert(String::from("cpu_model"), json!("another CPU"));
        }
        let read = parse::document(Path::new("other.json"), &other);
        let documents = vec![parsed("dev", &[row("one", 2.0, 100.0, 100.0)])];
        let mut documents = documents;
        if let Ok(other) = read {
            documents.push(other);
        }
        assert_eq!(built(&documents).len(), 2);
    }

    #[test]
    fn one_operating_point_measured_twice_is_rejected_rather_than_plotted_twice() {
        let documents = vec![
            parsed("dev", &[row("one", 2.0, 100.0, 100.0)]),
            parsed("dev", &[row("one", 2.0, 110.0, 100.0)]),
        ];
        let failure = charts(&documents);
        let message = failure.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("measured twice"), "{message}");
    }

    #[test]
    fn a_row_whose_round_trip_was_not_verified_is_named_rather_than_plotted() {
        let mut value = fixture::document(
            "dev",
            &[
                row("good", 2.0, 100.0, 100.0),
                row("unverified", 9.0, 900.0, 10.0),
            ],
        );
        let row = value
            .get_mut("measurements")
            .and_then(|rows| rows.get_mut(1))
            .and_then(Value::as_object_mut);
        if let Some(object) = row {
            let _ = object.insert(String::from("round_trip_verified"), json!(false));
        }
        let read = parse::document(Path::new("fixture.json"), &value);
        let documents: Vec<parse::Document> = read.into_iter().collect();
        let marked = marks(&built(&documents), "ratio-against-encode-throughput");
        assert_eq!(marked, vec![(String::from("good default"), false)]);
    }

    #[test]
    fn a_verdict_inside_the_reported_spread_says_so() {
        let mut value = fixture::document(
            "dev",
            &[
                row("ahead", 2.0, 101.0, 100.0),
                row("behind", 2.0, 100.0, 100.0),
            ],
        );
        // Both rows reported a spread far wider than the one percent between them.
        if let Some(rows) = value.get_mut("measurements").and_then(Value::as_array_mut) {
            for row in rows.iter_mut() {
                if let Some(object) = row.as_object_mut() {
                    let _ = object.insert(String::from("encode_samples"), fixture::samples(0.30));
                }
            }
        }
        let read = parse::document(Path::new("fixture.json"), &value);
        let documents: Vec<parse::Document> = read.into_iter().collect();
        let charts = built(&documents);
        let inside: Vec<(String, bool)> = charts
            .first()
            .and_then(|chart| axis(chart, "ratio-against-encode-throughput"))
            .map(|plot| {
                plot.points
                    .iter()
                    .map(|point| (point.label(), point.inside_reported_spread))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            inside,
            vec![
                (String::from("ahead default"), false),
                (String::from("behind default"), true),
            ]
        );
    }

    #[test]
    fn a_verdict_a_ratio_decided_is_never_called_noise() {
        let mut value = fixture::document(
            "dev",
            &[
                row("ahead", 2.01, 100.0, 100.0),
                row("behind", 2.00, 100.0, 100.0),
            ],
        );
        if let Some(rows) = value.get_mut("measurements").and_then(Value::as_array_mut) {
            for row in rows.iter_mut() {
                if let Some(object) = row.as_object_mut() {
                    let _ = object.insert(String::from("encode_samples"), fixture::samples(0.90));
                }
            }
        }
        let read = parse::document(Path::new("fixture.json"), &value);
        let documents: Vec<parse::Document> = read.into_iter().collect();
        let charts = built(&documents);
        let behind = charts
            .first()
            .and_then(|chart| axis(chart, "ratio-against-encode-throughput"))
            .and_then(|plot| {
                plot.points
                    .iter()
                    .find(|point| point.label() == "behind default")
            })
            .map(|point| (point.dominated(), point.inside_reported_spread));
        assert_eq!(behind, Some((true, false)));
    }
}
