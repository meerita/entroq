//! Owns the validation tiers, their budgets, and the segment set each campaign runs.
//!
//! Every duration and every input limit the runner enforces is stated here once. No other
//! module defines a budget, and no other module decides what a campaign runs.
//!
//! A tier says how a campaign is bounded, recorded, and resumed. A suite says which segments
//! it runs. The two are independent: the same suite can be asked for at more than one tier,
//! and a suite states the tiers it has segments for.
//!
//! This module does not own what a segment proves. A segment names a command, and the tool
//! behind that command owns its own behavior.

use std::time::Duration;

/// The hard wall-clock limit on one segment.
///
/// A segment that exceeds it is reported as a timeout. The limit does not move to make a
/// segment pass: the cost is reduced, or the segment is split.
pub const SEGMENT_BUDGET: Duration = Duration::from_secs(120);

const SMOKE_CAMPAIGN_BUDGET: Duration = Duration::from_secs(30);
const DEV_CAMPAIGN_BUDGET: Duration = Duration::from_secs(120);

const SMOKE_INPUT_BYTES: u64 = 1_048_576; // 1 MiB total
const DEV_INPUT_BYTES: u64 = 10_485_760; // 10 MiB per codec path
const GATE_INPUT_BYTES: u64 = 104_857_600; // 100 MiB per codec path

/// One step of a segment: a program and its exact arguments.
///
/// The runner never passes these through a shell.
pub struct Step {
    pub program: &'static str,
    pub args: &'static [&'static str],
}

/// One unit of work, of failure, and of resume.
///
/// A segment's identifier is stable across campaigns, and a segment never depends on another
/// segment of the same campaign.
pub struct Segment {
    pub id: &'static str,
    pub description: &'static str,
    pub steps: &'static [Step],
}

/// How much input a tier may spend.
pub enum InputBudget {
    Bytes(u64),
    FullCorpora,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Smoke,
    Dev,
    Gate,
    Publication,
}

const FMT: Step = Step {
    program: "cargo",
    args: &["fmt", "--all", "--", "--check"],
};
const CLIPPY: Step = Step {
    program: "cargo",
    args: &["clippy", "--workspace", "--all-targets"],
};
const BUILD: Step = Step {
    program: "cargo",
    args: &["build", "--workspace"],
};
const CHECK: Step = Step {
    program: "cargo",
    args: &["check", "--workspace"],
};
const TEST: Step = Step {
    program: "cargo",
    args: &["test", "--workspace"],
};

const SEGMENT_FMT: Segment = Segment {
    id: "workspace-fmt",
    description: "The workspace is formatted.",
    steps: &[FMT],
};
const SEGMENT_CLIPPY: Segment = Segment {
    id: "workspace-clippy",
    description: "The workspace and its targets pass the lint policy.",
    steps: &[CLIPPY],
};
const SEGMENT_BUILD: Segment = Segment {
    id: "workspace-build",
    description: "The workspace compiles.",
    steps: &[BUILD],
};
const SEGMENT_CHECK: Segment = Segment {
    id: "workspace-check",
    description: "The workspace type-checks.",
    steps: &[CHECK],
};
const SEGMENT_TEST: Segment = Segment {
    id: "workspace-test",
    description: "The workspace test suite passes.",
    steps: &[TEST],
};

/// The dev tier runs one segment, so its segment carries every workspace gate.
const SEGMENT_WORKSPACE_GATE: Segment = Segment {
    id: "workspace-gate",
    description: "Every workspace gate, in one segment.",
    steps: &[FMT, CLIPPY, BUILD, CHECK, TEST],
};

const SMOKE_SEGMENTS: &[Segment] = &[SEGMENT_FMT, SEGMENT_CHECK];
const DEV_SEGMENTS: &[Segment] = &[SEGMENT_WORKSPACE_GATE];
const SEGMENTED_GATES: &[Segment] = &[
    SEGMENT_FMT,
    SEGMENT_CLIPPY,
    SEGMENT_BUILD,
    SEGMENT_CHECK,
    SEGMENT_TEST,
];

/// One competitor: build it into the laboratory, then rebuild it from its own record.
///
/// Each competitor is its own segment, so one build fits the segment budget, one failure
/// blocks one competitor, and a resumed campaign rebuilds only what did not pass.
macro_rules! lab_segment {
    ($id:literal, $codec:literal, $description:literal) => {
        Segment {
            id: $id,
            description: $description,
            steps: &[
                Step {
                    program: "cargo",
                    args: &[
                        "run",
                        "--quiet",
                        "--release",
                        "--package",
                        "entroq-bench",
                        "--",
                        "lab",
                        "build",
                        "--codec",
                        $codec,
                    ],
                },
                Step {
                    program: "cargo",
                    args: &[
                        "run",
                        "--quiet",
                        "--release",
                        "--package",
                        "entroq-bench",
                        "--",
                        "lab",
                        "verify",
                        "--codec",
                        $codec,
                    ],
                },
            ],
        }
    };
}

const LAB_SEGMENTS: &[Segment] = &[
    lab_segment!(
        "lab-build-lz4",
        "lz4",
        "LZ4 builds at its pinned commit and rebuilds from its own record."
    ),
    lab_segment!(
        "lab-build-zstd",
        "zstd",
        "Zstandard builds at its pinned commit and rebuilds from its own record."
    ),
    lab_segment!(
        "lab-build-brotli",
        "brotli",
        "Brotli builds at its pinned commit and rebuilds from its own record."
    ),
    lab_segment!(
        "lab-build-snappy",
        "snappy",
        "Snappy builds at its pinned commit and rebuilds from its own record."
    ),
    lab_segment!(
        "lab-build-zlib",
        "zlib",
        "zlib builds at its pinned commit and rebuilds from its own record."
    ),
];

/// A named set of segments, and what a campaign over that set does and does not cover.
///
/// A suite is what is run. A tier is how it is bounded and recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suite {
    /// The gates every revision of the repository must pass.
    Workspace,
    /// The competitor laboratory: one pinned build per competitor.
    Lab,
}

impl Suite {
    /// Parses the suite a `--suite` argument names.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "workspace" => Some(Self::Workspace),
            "lab" => Some(Self::Lab),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Lab => "lab",
        }
    }

    /// The record directory name a campaign takes when the caller names no topic.
    pub const fn default_topic(self) -> &'static str {
        match self {
            Self::Workspace => "workspace-gate",
            Self::Lab => "lab-build",
        }
    }

    /// The segments this suite runs at a tier, or none when it has none for that tier.
    ///
    /// The laboratory suite runs one segment per competitor, so it needs a tier that
    /// segments. Only a segmented tier offers one.
    pub const fn segments(self, tier: Tier) -> &'static [Segment] {
        match self {
            Self::Workspace => match tier {
                Tier::Smoke => SMOKE_SEGMENTS,
                Tier::Dev => DEV_SEGMENTS,
                Tier::Gate | Tier::Publication => SEGMENTED_GATES,
            },
            Self::Lab => match tier {
                Tier::Smoke | Tier::Dev => &[],
                Tier::Gate | Tier::Publication => LAB_SEGMENTS,
            },
        }
    }

    /// What a passing campaign over this suite actually covers.
    ///
    /// The manifest carries this sentence so no reader mistakes a passing campaign for
    /// codec coverage.
    pub const fn coverage(self) -> &'static str {
        match self {
            Self::Workspace => {
                "Workspace gates only. No codec exists at this revision, so no segment \
                 exercises a codec path and no test asserts codec behavior."
            }
            Self::Lab => {
                "Competitor laboratory builds only. No segment compresses a byte, and \
                 nothing in the repository links what these builds produce at this \
                 revision. A segment reports whether one competitor built at its pinned \
                 commit and rebuilt from its own recorded commands."
            }
        }
    }

    /// What no segment of this suite measured, stated so no reader infers it.
    pub const fn limits(self) -> &'static str {
        match self {
            Self::Workspace => {
                "The campaign ran on one host and one architecture. It measured no \
                 compression ratio, no throughput, and no competitor. A segment reports \
                 only whether its command succeeded within the segment budget."
            }
            Self::Lab => {
                "The campaign ran on one host and one architecture. It measured no \
                 compression ratio and no throughput. A rebuild compares the bytes a \
                 recorded command produced against the bytes the pinned build holds; it \
                 does not prove that the two builds behave identically, and a difference \
                 in those bytes is recorded rather than failed."
            }
        }
    }
}

impl Tier {
    /// Parses the tier a `--tier` argument names.
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

    /// Whether the tier leaves a record.
    ///
    /// Smoke is not recorded because repeating it is cheaper than reading a record.
    pub const fn is_recorded(self) -> bool {
        !matches!(self, Self::Smoke)
    }

    /// The wall-clock limit on the whole campaign, when the tier declares one.
    ///
    /// A segmented tier declares none. Each of its segments is bounded instead.
    pub const fn campaign_budget(self) -> Option<Duration> {
        match self {
            Self::Smoke => Some(SMOKE_CAMPAIGN_BUDGET),
            Self::Dev => Some(DEV_CAMPAIGN_BUDGET),
            Self::Gate | Self::Publication => None,
        }
    }

    pub const fn input_budget(self) -> InputBudget {
        match self {
            Self::Smoke => InputBudget::Bytes(SMOKE_INPUT_BYTES),
            Self::Dev => InputBudget::Bytes(DEV_INPUT_BYTES),
            Self::Gate => InputBudget::Bytes(GATE_INPUT_BYTES),
            Self::Publication => InputBudget::FullCorpora,
        }
    }

    /// Whether a campaign at this tier can be resumed.
    pub const fn is_resumable(self) -> bool {
        matches!(self, Self::Gate | Self::Publication)
    }
}

#[cfg(test)]
mod tests {
    use super::{SEGMENT_BUDGET, Suite, Tier};

    const TIERS: [Tier; 4] = [Tier::Smoke, Tier::Dev, Tier::Gate, Tier::Publication];
    const SUITES: [Suite; 2] = [Suite::Workspace, Suite::Lab];

    #[test]
    fn every_tier_name_round_trips() {
        for tier in TIERS {
            assert_eq!(Tier::parse(tier.name()).map(Tier::name), Some(tier.name()));
        }
    }

    #[test]
    fn an_unknown_tier_is_rejected() {
        assert!(Tier::parse("quick").is_none());
        assert!(Tier::parse("").is_none());
    }

    #[test]
    fn only_smoke_goes_unrecorded() {
        for tier in TIERS {
            assert_eq!(tier.is_recorded(), !matches!(tier, Tier::Smoke));
        }
    }

    #[test]
    fn every_suite_name_round_trips() {
        for suite in SUITES {
            assert_eq!(
                Suite::parse(suite.name()).map(Suite::name),
                Some(suite.name())
            );
        }
    }

    #[test]
    fn an_unknown_suite_is_rejected() {
        assert!(Suite::parse("competitors").is_none());
        assert!(Suite::parse("").is_none());
    }

    #[test]
    fn the_workspace_suite_runs_one_segment_at_the_dev_tier() {
        assert_eq!(Suite::Workspace.segments(Tier::Dev).len(), 1);
    }

    #[test]
    fn the_workspace_suite_has_segments_at_every_tier() {
        for tier in TIERS {
            assert!(!Suite::Workspace.segments(tier).is_empty());
        }
    }

    #[test]
    fn the_laboratory_suite_runs_one_segment_per_competitor() {
        let segments = Suite::Lab.segments(Tier::Gate);
        assert_eq!(segments.len(), 5);
        let ids: Vec<&str> = segments.iter().map(|segment| segment.id).collect();
        for expected in [
            "lab-build-lz4",
            "lab-build-zstd",
            "lab-build-brotli",
            "lab-build-snappy",
            "lab-build-zlib",
        ] {
            assert!(ids.contains(&expected), "{expected} is not a segment");
        }
    }

    #[test]
    fn the_laboratory_suite_has_no_segments_at_a_tier_that_does_not_segment() {
        assert!(Suite::Lab.segments(Tier::Smoke).is_empty());
        assert!(Suite::Lab.segments(Tier::Dev).is_empty());
    }

    #[test]
    fn every_segment_identifier_is_unique_within_its_campaign() {
        for suite in SUITES {
            for tier in TIERS {
                let mut ids: Vec<&str> = suite.segments(tier).iter().map(|s| s.id).collect();
                let count = ids.len();
                ids.sort_unstable();
                ids.dedup();
                assert_eq!(ids.len(), count, "{} at {}", suite.name(), tier.name());
            }
        }
    }

    #[test]
    fn no_two_suites_share_a_topic_so_no_resume_picks_the_wrong_record() {
        assert_ne!(Suite::Workspace.default_topic(), Suite::Lab.default_topic());
    }

    #[test]
    fn every_suite_states_what_it_covers_and_what_it_does_not() {
        for suite in SUITES {
            assert!(!suite.coverage().is_empty());
            assert!(!suite.limits().is_empty());
        }
    }

    #[test]
    fn the_laboratory_suite_says_it_measured_no_compression() {
        assert!(Suite::Lab.coverage().contains("compresses a byte"));
        assert!(Suite::Lab.limits().contains("no throughput"));
    }

    #[test]
    fn a_campaign_budget_never_exceeds_the_segment_budget() {
        for tier in TIERS {
            if let Some(budget) = tier.campaign_budget() {
                assert!(budget <= SEGMENT_BUDGET);
            }
        }
    }

    #[test]
    fn only_a_segmented_tier_resumes() {
        for tier in TIERS {
            assert_eq!(tier.is_resumable(), tier.campaign_budget().is_none());
        }
    }
}
