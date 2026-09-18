//! Owns what a benchmark tier covers: which size classes, which operating points, how much
//! input, how many samples, and what spread the tier accepts.
//!
//! A tier is a budget, not an ambition. A class whose registered entries exceed the budget
//! is covered as far as the budget reaches, and the entries the budget left out are named in
//! the result so nobody reads a partial pass as a full corpus.
//!
//! The tiers here are the validation tiers, applied to a measurement. What each one means
//! for a benchmark is stated once, below, and nowhere else.
//!
//! This module does not own what is measured, how it is timed, or where it is written.

use std::path::PathBuf;
use std::time::Duration;

use crate::catalog::PointGroup;
use crate::registry::{Entry, SizeClass};
use crate::subject::Subject;

/// The chunk one streamed measurement feeds at a time.
///
/// It is large enough that a chunk is real work for every competitor and small enough that a
/// medium input is many chunks, so a per-chunk latency is a distribution rather than one
/// reading.
pub const STREAM_CHUNK: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Smoke,
    Dev,
    Gate,
    Publication,
}

/// Which operating points a tier measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Points {
    /// The point the subject selects when a caller states no level.
    Default,
    /// Every point the catalog pins, which is what a frontier needs.
    Every,
}

impl Tier {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "smoke" => Some(Self::Smoke),
            "dev" => Some(Self::Dev),
            "gate" => Some(Self::Gate),
            "publication" => Some(Self::Publication),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Dev => "dev",
            Self::Gate => "gate",
            Self::Publication => "publication",
        }
    }

    /// Whether a number from this tier may leave the repository.
    ///
    /// Exactly one tier may, and a result states which tier produced it, so a development
    /// number can never be read as a published one.
    pub const fn is_publication(self) -> bool {
        matches!(self, Self::Publication)
    }

    /// The size classes this tier measures.
    ///
    /// Behavior changes with size, so the classes are the axis a tier grows along. The huge
    /// class is streamed and belongs to the publication tier alone.
    pub const fn classes(self) -> &'static [SizeClass] {
        match self {
            Self::Smoke => &[SizeClass::Tiny, SizeClass::Small],
            Self::Dev => &[SizeClass::Tiny, SizeClass::Small, SizeClass::Medium],
            Self::Gate => &[
                SizeClass::Tiny,
                SizeClass::Small,
                SizeClass::Medium,
                SizeClass::Large,
            ],
            Self::Publication => &[
                SizeClass::Tiny,
                SizeClass::Small,
                SizeClass::Medium,
                SizeClass::Large,
                SizeClass::Huge,
            ],
        }
    }

    pub const fn points(self) -> Points {
        match self {
            // A cheap tier reports direction, and direction does not need a frontier. It
            // measures what a caller who states no level actually gets.
            Self::Smoke | Self::Dev => Points::Default,
            Self::Gate | Self::Publication => Points::Every,
        }
    }

    /// The distinct input bytes one segment may read, per size class.
    pub const fn input_budget(self) -> u64 {
        match self {
            Self::Smoke => 1_048_576,
            Self::Dev => 10_485_760,
            Self::Gate => 104_857_600,
            // The publication tier measures the full corpora, and a segment that cannot be
            // split states its reason before it runs.
            Self::Publication => u64::MAX,
        }
    }

    /// How many samples one measurement aims for.
    pub const fn samples(self) -> usize {
        match self {
            Self::Smoke => 3,
            Self::Dev => 5,
            Self::Gate => 11,
            Self::Publication => 31,
        }
    }

    /// The wall clock one measurement may spend collecting samples.
    ///
    /// A measurement always takes at least one sample. This budget decides how many more it
    /// can afford, so an expensive operating point reports one sample and says so instead of
    /// pushing its segment over the budget.
    pub const fn sample_budget(self) -> Duration {
        match self {
            Self::Smoke => Duration::from_millis(500),
            Self::Dev => Duration::from_secs(2),
            Self::Gate => Duration::from_secs(6),
            Self::Publication => Duration::from_secs(20),
        }
    }

    /// The spread a measurement may show before the result calls it unacceptable.
    ///
    /// The spread is the distance from the fastest to the slowest sample over the median. A
    /// measurement of one sample has no spread to judge, and the result says so rather than
    /// reporting zero variance.
    pub const fn accepted_spread(self) -> Option<f64> {
        match self {
            // Smoke ignores its numbers, so it judges no spread.
            Self::Smoke => None,
            Self::Dev => Some(0.35),
            Self::Gate => Some(0.15),
            Self::Publication => Some(0.10),
        }
    }

    /// What a result from this tier may be read as.
    pub const fn licence(self) -> &'static str {
        match self {
            Self::Smoke => {
                "The smoke tier proves the harness runs and produces a parseable result. Its \
                 numbers are ignored. No smoke number may be compared, published, or used to \
                 decide anything."
            }
            Self::Dev => {
                "The dev tier measures direction on one machine with few samples. It \
                 measures each competitor at the operating point its own project defaults \
                 to, not across its operating points, so it describes no frontier. No dev \
                 number appears in documentation, in a release note, or in a comparison \
                 claim."
            }
            Self::Gate => {
                "The gate tier measures every pinned operating point across the size classes \
                 its budget reaches, with repeated samples and a reported spread. It gates a \
                 milestone. It is not a published number."
            }
            Self::Publication => {
                "The publication tier is the only tier a number may leave the repository \
                 from, and only from a sealed record."
            }
        }
    }
}

/// What one segment was asked to measure.
pub struct Request {
    pub tier: Tier,
    pub subject: Subject,
    /// The one size class this segment covers, or every class the tier names.
    pub class: Option<SizeClass>,
    /// The one operating point group this segment covers, or every point the tier names.
    pub points: Option<&'static PointGroup>,
    /// The identifier the runner knows this segment by.
    pub segment: String,
    pub lab: Option<PathBuf>,
    pub corpus: Option<PathBuf>,
}

/// The operating points one segment measures.
///
/// A named group is what the segment covers. Without one, the tier decides: a cheap tier
/// measures the point the subject defaults to, and a tier that describes a frontier measures
/// every point the subject declares.
pub fn points(request: &Request) -> Vec<&'static str> {
    if let Some(group) = request.points {
        return group.points.to_vec();
    }
    match request.tier.points() {
        Points::Default => vec![request.subject.default_point()],
        Points::Every => request.subject.operating_points(),
    }
}

/// What a segment's input selection came to, including what it left out.
pub struct Selection {
    pub selected: Vec<&'static Entry>,
    /// Entries of the same classes that the budget did not reach, largest cost first in
    /// registry order.
    pub excluded: Vec<&'static Entry>,
    pub bytes: u64,
    pub budget_per_class: u64,
}

/// Chooses the entries one segment measures.
///
/// Entries are taken in registry order, per size class, until that class's budget is spent.
/// An entry that does not fit what remains is left out and the next one is tried, so one
/// large entry does not starve a class of everything behind it. Every entry left out is
/// named, because a class covered in part and a class covered in full are different results.
pub fn select(tier: Tier, classes: &[SizeClass], entries: &'static [Entry]) -> Selection {
    let budget = tier.input_budget();
    let mut selected = Vec::new();
    let mut excluded = Vec::new();
    let mut total = 0_u64;
    for class in classes {
        let mut spent = 0_u64;
        for entry in entries.iter().filter(|entry| entry.class() == *class) {
            let after = spent.saturating_add(entry.bytes);
            if after > budget {
                excluded.push(entry);
                continue;
            }
            spent = after;
            total = total.saturating_add(entry.bytes);
            selected.push(entry);
        }
    }
    Selection {
        selected,
        excluded,
        bytes: total,
        budget_per_class: budget,
    }
}

#[cfg(test)]
mod tests {
    use super::{Points, Request, Selection, Tier, points, select};
    use crate::registry::{ENTRIES, SizeClass};
    use crate::subject::Subject;

    fn request(tier: Tier, subject: Subject, group: Option<&str>) -> Request {
        Request {
            tier,
            subject,
            class: None,
            points: group.and_then(|name| subject.group(name)),
            segment: String::from("a test"),
            lab: None,
            corpus: None,
        }
    }

    /// The subject a test names, which this workspace declares.
    fn codec(name: &str) -> Subject {
        let Some(found) = Subject::parse(name) else {
            unreachable!("{name} is a subject this workspace declares")
        };
        found
    }

    const TIERS: [Tier; 4] = [Tier::Smoke, Tier::Dev, Tier::Gate, Tier::Publication];

    fn selection(tier: Tier) -> Selection {
        select(tier, tier.classes(), ENTRIES)
    }

    #[test]
    fn every_tier_name_round_trips() {
        for tier in TIERS {
            assert_eq!(Tier::parse(tier.name()).map(Tier::name), Some(tier.name()));
        }
        assert!(Tier::parse("quick").is_none());
    }

    #[test]
    fn only_one_tier_may_leave_the_repository() {
        let publishing: Vec<Tier> = TIERS.into_iter().filter(|t| t.is_publication()).collect();
        assert_eq!(publishing, vec![Tier::Publication]);
    }

    #[test]
    fn a_tier_covers_every_class_a_cheaper_tier_covers() {
        for pair in TIERS.windows(2) {
            let (Some(cheaper), Some(richer)) = (pair.first(), pair.get(1)) else {
                continue;
            };
            for class in cheaper.classes() {
                assert!(
                    richer.classes().contains(class),
                    "the {} tier drops the {} class",
                    richer.name(),
                    class.name()
                );
            }
        }
    }

    #[test]
    fn the_huge_class_belongs_to_the_publication_tier_alone() {
        for tier in TIERS {
            assert_eq!(
                tier.classes().contains(&SizeClass::Huge),
                tier.is_publication(),
                "{}",
                tier.name()
            );
        }
    }

    #[test]
    fn a_cheap_tier_measures_the_default_point_and_a_gate_measures_every_point() {
        assert_eq!(Tier::Smoke.points(), Points::Default);
        assert_eq!(Tier::Dev.points(), Points::Default);
        assert_eq!(Tier::Gate.points(), Points::Every);
        assert_eq!(Tier::Publication.points(), Points::Every);
    }

    #[test]
    fn a_named_group_decides_the_points_whatever_the_tier_would_have_measured() {
        assert_eq!(
            points(&request(Tier::Gate, codec("zstd"), Some("max"))),
            vec!["level-22"]
        );
    }

    #[test]
    fn a_segment_that_names_no_group_measures_what_its_tier_names() {
        assert_eq!(
            points(&request(Tier::Dev, codec("zstd"), None)),
            vec!["level-3"],
            "a cheap tier measures the point the project defaults to"
        );
        assert_eq!(
            points(&request(Tier::Gate, codec("zstd"), None)).len(),
            codec("zstd").operating_points().len()
        );
    }

    #[test]
    fn the_groups_of_one_subject_cover_every_point_its_gate_campaign_measures() {
        let mut subjects = vec![Subject::Entroq];
        subjects.extend(crate::catalog::CODECS.iter().map(Subject::Competitor));
        for subject in subjects {
            let mut every = points(&request(Tier::Gate, subject, None));
            let mut grouped: Vec<&str> = subject
                .point_groups()
                .iter()
                .flat_map(|group| points(&request(Tier::Gate, subject, Some(group.name))))
                .collect();
            grouped.sort_unstable();
            every.sort_unstable();
            assert_eq!(
                grouped,
                every,
                "{} leaves a point unmeasured",
                subject.name()
            );
        }
    }

    #[test]
    fn the_codec_measures_its_one_point_at_every_tier() {
        for tier in TIERS {
            assert_eq!(
                points(&request(tier, Subject::Entroq, None)),
                vec!["default"],
                "{}",
                tier.name()
            );
        }
    }

    #[test]
    fn the_input_budget_grows_with_the_tier() {
        assert!(Tier::Smoke.input_budget() < Tier::Dev.input_budget());
        assert!(Tier::Dev.input_budget() < Tier::Gate.input_budget());
        assert!(Tier::Gate.input_budget() < Tier::Publication.input_budget());
    }

    #[test]
    fn a_richer_tier_takes_more_samples_and_accepts_less_spread() {
        assert!(Tier::Gate.samples() > Tier::Dev.samples());
        let dev = Tier::Dev.accepted_spread().unwrap_or(0.0);
        let gate = Tier::Gate.accepted_spread().unwrap_or(0.0);
        assert!(gate < dev, "{gate} is not tighter than {dev}");
        assert!(Tier::Smoke.accepted_spread().is_none());
    }

    #[test]
    fn every_tier_states_what_its_numbers_may_be_read_as() {
        for tier in TIERS {
            assert!(!tier.licence().is_empty());
        }
        assert!(
            Tier::Dev.licence().contains("no frontier") || Tier::Dev.licence().contains("frontier")
        );
    }

    #[test]
    fn a_selection_stays_inside_the_budget_of_every_class_it_covers() {
        for tier in [Tier::Smoke, Tier::Dev, Tier::Gate] {
            for class in tier.classes() {
                let spent: u64 = selection(tier)
                    .selected
                    .iter()
                    .filter(|entry| entry.class() == *class)
                    .map(|entry| entry.bytes)
                    .sum();
                assert!(
                    spent <= tier.input_budget(),
                    "the {} tier spends {spent} on the {} class",
                    tier.name(),
                    class.name()
                );
            }
        }
    }

    #[test]
    fn a_selection_covers_every_class_its_tier_names() {
        for tier in TIERS {
            let selection = selection(tier);
            for class in tier.classes() {
                assert!(
                    selection.selected.iter().any(|e| e.class() == *class),
                    "the {} tier selects nothing in the {} class",
                    tier.name(),
                    class.name()
                );
            }
        }
    }

    #[test]
    fn a_selection_never_reaches_a_class_its_tier_does_not_name() {
        let smoke = selection(Tier::Smoke);
        assert!(
            smoke
                .selected
                .iter()
                .all(|e| Tier::Smoke.classes().contains(&e.class()))
        );
    }

    #[test]
    fn an_entry_the_budget_left_out_is_named_rather_than_dropped_in_silence() {
        let gate = selection(Tier::Gate);
        let names: Vec<&str> = gate.excluded.iter().map(|entry| entry.name).collect();
        assert!(names.contains(&"enwik8"), "{names:?}");
        assert!(!gate.selected.iter().any(|entry| entry.name == "enwik8"));
    }

    #[test]
    fn the_publication_tier_leaves_nothing_out() {
        let publication = selection(Tier::Publication);
        assert!(publication.excluded.is_empty());
        assert_eq!(publication.selected.len(), ENTRIES.len());
    }
}
