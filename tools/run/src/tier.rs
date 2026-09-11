//! Owns the validation tiers, their budgets, and the segment set each tier runs.
//!
//! Every duration and every input limit the runner enforces is stated here once. No other
//! module defines a budget, and no other module decides what a tier runs.
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

/// What this revision's segment set actually covers.
///
/// The manifest carries this sentence so no reader mistakes a passing campaign for codec
/// coverage.
pub const COVERAGE: &str = "Workspace gates only. No codec exists at this revision, \
so no segment exercises a codec path and no test asserts codec behavior.";

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

    pub const fn segments(self) -> &'static [Segment] {
        match self {
            Self::Smoke => SMOKE_SEGMENTS,
            Self::Dev => DEV_SEGMENTS,
            Self::Gate | Self::Publication => SEGMENTED_GATES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SEGMENT_BUDGET, Tier};

    const TIERS: [Tier; 4] = [Tier::Smoke, Tier::Dev, Tier::Gate, Tier::Publication];

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
    fn the_dev_tier_runs_one_segment() {
        assert_eq!(Tier::Dev.segments().len(), 1);
    }

    #[test]
    fn every_tier_has_at_least_one_segment() {
        for tier in TIERS {
            assert!(!tier.segments().is_empty());
        }
    }

    #[test]
    fn every_segment_identifier_is_unique_within_its_tier() {
        for tier in TIERS {
            let mut ids: Vec<&str> = tier.segments().iter().map(|s| s.id).collect();
            let count = ids.len();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), count);
        }
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
