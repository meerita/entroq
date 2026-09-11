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

/// The wall clock one fuzz invocation gives libFuzzer.
///
/// Fuzzing is cumulative, so one invocation buys one bounded segment and stops. The value
/// sits well inside the segment budget, because the corpus load, the driver check, and the
/// process exit all happen inside the same segment.
pub const FUZZ_SEGMENT_SECONDS: u64 = 60;

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

/// One corpus segment: one action of the benchmark tool over one part of the registry.
///
/// Each group is its own segment, so one fetch fits the segment budget, one unreachable
/// upstream blocks one group, and a resumed campaign re-fetches only what did not pass.
macro_rules! corpus_step {
    ($action:literal, $selector:literal, $what:literal) => {
        Step {
            program: "cargo",
            args: &[
                "run",
                "--quiet",
                "--release",
                "--package",
                "entroq-bench",
                "--",
                "corpus",
                $action,
                $selector,
                $what,
            ],
        }
    };
}

const CORPUS_SEGMENTS: &[Segment] = &[
    Segment {
        id: "corpus-registry",
        description: "The registry states a source, a checksum, a size, a class, and a \
                      license for every entry.",
        steps: &[Step {
            program: "cargo",
            args: &[
                "run",
                "--quiet",
                "--release",
                "--package",
                "entroq-bench",
                "--",
                "corpus",
                "list",
                "--all",
            ],
        }],
    },
    Segment {
        id: "corpus-generate-project",
        description: "Every project entry generates from its recorded seed, and generates \
                      the same bytes twice.",
        steps: &[
            corpus_step!("build", "--group", "project"),
            corpus_step!("reproduce", "--group", "project"),
        ],
    },
    Segment {
        id: "corpus-fetch-enwik8",
        description: "The enwik8 snapshot fetches and matches its recorded checksum.",
        steps: &[corpus_step!("build", "--entry", "enwik8")],
    },
    Segment {
        id: "corpus-fetch-sourcecode",
        description: "The pinned source tree fetches and matches its recorded checksum.",
        steps: &[corpus_step!("build", "--group", "sourcecode")],
    },
    Segment {
        id: "corpus-fetch-gutenberg",
        description: "Every Project Gutenberg entry fetches and matches its recorded \
                      checksum.",
        steps: &[corpus_step!("build", "--group", "gutenberg")],
    },
];

/// One benchmark segment: one competitor, at one tier, over the size classes the tier names
/// or over the one class the segment names.
///
/// A segment is one harness process, so the resident-set figure it reports is its own and a
/// failure blocks one competitor rather than the campaign.
macro_rules! bench_segment {
    ($tier:literal, $codec:literal) => {
        Segment {
            id: concat!("bench-", $codec),
            description: concat!(
                "Every metric the ",
                $tier,
                " tier covers, for ",
                $codec,
                ", measured in-process through the library the laboratory built."
            ),
            steps: &[Step {
                program: "cargo",
                args: &[
                    "run",
                    "--quiet",
                    "--release",
                    "--package",
                    "entroq-bench",
                    "--",
                    "measure",
                    "--tier",
                    $tier,
                    "--codec",
                    $codec,
                ],
            }],
        }
    };
    ($tier:literal, $codec:literal, $class:literal) => {
        Segment {
            id: concat!("bench-", $codec, "-", $class),
            description: concat!(
                "Every metric the ",
                $tier,
                " tier covers, for ",
                $codec,
                " at the ",
                $class,
                " size class."
            ),
            steps: &[Step {
                program: "cargo",
                args: &[
                    "run",
                    "--quiet",
                    "--release",
                    "--package",
                    "entroq-bench",
                    "--",
                    "measure",
                    "--tier",
                    $tier,
                    "--codec",
                    $codec,
                    "--class",
                    $class,
                ],
            }],
        }
    };
}

/// The cheap tiers measure every class they cover in one segment per competitor, so a
/// campaign that has to fit one budget stays five processes rather than twenty.
const BENCH_SMOKE_SEGMENTS: &[Segment] = &[
    bench_segment!("smoke", "lz4"),
    bench_segment!("smoke", "zstd"),
    bench_segment!("smoke", "brotli"),
    bench_segment!("smoke", "snappy"),
    bench_segment!("smoke", "zlib"),
];

const BENCH_DEV_SEGMENTS: &[Segment] = &[
    bench_segment!("dev", "lz4"),
    bench_segment!("dev", "zstd"),
    bench_segment!("dev", "brotli"),
    bench_segment!("dev", "snappy"),
    bench_segment!("dev", "zlib"),
];

/// A segmented tier splits by size class as well, because the cost of a class grows
/// with the input it covers and a segment holds one budget.
const BENCH_GATE_SEGMENTS: &[Segment] = &[
    bench_segment!("gate", "lz4", "tiny"),
    bench_segment!("gate", "lz4", "small"),
    bench_segment!("gate", "lz4", "medium"),
    bench_segment!("gate", "lz4", "large"),
    bench_segment!("gate", "zstd", "tiny"),
    bench_segment!("gate", "zstd", "small"),
    bench_segment!("gate", "zstd", "medium"),
    bench_segment!("gate", "zstd", "large"),
    bench_segment!("gate", "brotli", "tiny"),
    bench_segment!("gate", "brotli", "small"),
    bench_segment!("gate", "brotli", "medium"),
    bench_segment!("gate", "brotli", "large"),
    bench_segment!("gate", "snappy", "tiny"),
    bench_segment!("gate", "snappy", "small"),
    bench_segment!("gate", "snappy", "medium"),
    bench_segment!("gate", "snappy", "large"),
    bench_segment!("gate", "zlib", "tiny"),
    bench_segment!("gate", "zlib", "small"),
    bench_segment!("gate", "zlib", "medium"),
    bench_segment!("gate", "zlib", "large"),
];

const BENCH_PUBLICATION_SEGMENTS: &[Segment] = &[
    bench_segment!("publication", "lz4", "tiny"),
    bench_segment!("publication", "lz4", "small"),
    bench_segment!("publication", "lz4", "medium"),
    bench_segment!("publication", "lz4", "large"),
    bench_segment!("publication", "lz4", "huge"),
    bench_segment!("publication", "zstd", "tiny"),
    bench_segment!("publication", "zstd", "small"),
    bench_segment!("publication", "zstd", "medium"),
    bench_segment!("publication", "zstd", "large"),
    bench_segment!("publication", "zstd", "huge"),
    bench_segment!("publication", "brotli", "tiny"),
    bench_segment!("publication", "brotli", "small"),
    bench_segment!("publication", "brotli", "medium"),
    bench_segment!("publication", "brotli", "large"),
    bench_segment!("publication", "brotli", "huge"),
    bench_segment!("publication", "snappy", "tiny"),
    bench_segment!("publication", "snappy", "small"),
    bench_segment!("publication", "snappy", "medium"),
    bench_segment!("publication", "snappy", "large"),
    bench_segment!("publication", "snappy", "huge"),
    bench_segment!("publication", "zlib", "tiny"),
    bench_segment!("publication", "zlib", "small"),
    bench_segment!("publication", "zlib", "medium"),
    bench_segment!("publication", "zlib", "large"),
    bench_segment!("publication", "zlib", "huge"),
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
    /// The corpus registry: the project corpus, and every registered public corpus.
    Corpus,
    /// The benchmark: every competitor measured in-process, one segment per competitor.
    Bench,
}

impl Suite {
    /// Parses the suite a `--suite` argument names.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "workspace" => Some(Self::Workspace),
            "lab" => Some(Self::Lab),
            "corpus" => Some(Self::Corpus),
            "bench" => Some(Self::Bench),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Lab => "lab",
            Self::Corpus => "corpus",
            Self::Bench => "bench",
        }
    }

    /// The record directory name a campaign takes when the caller names no topic.
    pub const fn default_topic(self) -> &'static str {
        match self {
            Self::Workspace => "workspace-gate",
            Self::Lab => "lab-build",
            Self::Corpus => "corpus-registry",
            Self::Bench => "bench-baseline",
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
            Self::Corpus => match tier {
                Tier::Smoke | Tier::Dev => &[],
                Tier::Gate | Tier::Publication => CORPUS_SEGMENTS,
            },
            Self::Bench => match tier {
                Tier::Smoke => BENCH_SMOKE_SEGMENTS,
                Tier::Dev => BENCH_DEV_SEGMENTS,
                Tier::Gate => BENCH_GATE_SEGMENTS,
                Tier::Publication => BENCH_PUBLICATION_SEGMENTS,
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
            Self::Corpus => {
                "Corpus registry only. No segment compresses a byte. One segment records \
                 the registry itself: the source, the checksum, the size, the class, and \
                 the license of every entry. Every other segment reports whether one part \
                 of the registry materialized into the cache and matched the checksum the \
                 registry pins, and whether a generated entry produces the same bytes \
                 twice from its recorded seed."
            }
            Self::Bench => {
                "Competitor measurement only. Each segment drives one competitor \
                 in-process, through the library the laboratory built, over the registered \
                 corpus entries its tier's budget reaches, and emits one machine-readable \
                 result. Entroq has no codec path at this revision, so every result carries \
                 an empty Entroq column and states why."
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
            Self::Corpus => {
                "The campaign ran on one host and one architecture. It measured no \
                 compression ratio and no throughput. A fetch segment proves that an \
                 upstream artifact still serves the recorded bytes; it proves nothing \
                 about how that artifact was produced, and its duration is a property of \
                 the network it ran on. An entry whose license cannot be established, or \
                 whose fetch cannot finish inside one segment budget, is not registered, \
                 so the registry is bounded by what is licensed and obtainable rather \
                 than by what exists."
            }
            Self::Bench => {
                "The campaign ran on one host and one architecture, and it compares no two \
                 environments. It measured no Entroq number, because no encoder and no \
                 decoder exist. A cycle count and an instruction count need a performance \
                 monitor unit this host does not grant, so all three counter metrics report \
                 unavailable with the reason and no number is derived from elapsed time. A \
                 tier's input budget bounds the entries a segment reads, and every entry a \
                 budget did not reach is named in the result rather than dropped in silence."
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
    use super::{Duration, SEGMENT_BUDGET, Suite, Tier};

    const TIERS: [Tier; 4] = [Tier::Smoke, Tier::Dev, Tier::Gate, Tier::Publication];
    const SUITES: [Suite; 4] = [Suite::Workspace, Suite::Lab, Suite::Corpus, Suite::Bench];

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
    fn the_corpus_suite_runs_one_segment_per_corpus_group() {
        let segments = Suite::Corpus.segments(Tier::Gate);
        let ids: Vec<&str> = segments.iter().map(|segment| segment.id).collect();
        for expected in [
            "corpus-registry",
            "corpus-generate-project",
            "corpus-fetch-enwik8",
            "corpus-fetch-sourcecode",
            "corpus-fetch-gutenberg",
        ] {
            assert!(ids.contains(&expected), "{expected} is not a segment");
        }
        assert_eq!(segments.len(), 5);
    }

    #[test]
    fn the_corpus_suite_has_no_segments_at_a_tier_that_does_not_segment() {
        assert!(Suite::Corpus.segments(Tier::Smoke).is_empty());
        assert!(Suite::Corpus.segments(Tier::Dev).is_empty());
    }

    #[test]
    fn the_corpus_suite_proves_generation_before_it_proves_a_fetch() {
        // A host with no network still runs the segments that need none, and a campaign
        // that stops at the first failure reports the cheapest one first.
        let ids: Vec<&str> = Suite::Corpus
            .segments(Tier::Gate)
            .iter()
            .map(|segment| segment.id)
            .collect();
        let generate = ids.iter().position(|id| *id == "corpus-generate-project");
        let fetch = ids.iter().position(|id| id.starts_with("corpus-fetch-"));
        assert!(generate < fetch);
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
        let mut topics: Vec<&str> = SUITES.iter().map(|suite| suite.default_topic()).collect();
        let count = topics.len();
        topics.sort_unstable();
        topics.dedup();
        assert_eq!(topics.len(), count);
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
    fn the_bench_suite_runs_one_segment_per_competitor_at_a_tier_that_holds_one_budget() {
        for tier in [Tier::Smoke, Tier::Dev] {
            assert_eq!(Suite::Bench.segments(tier).len(), 5, "{}", tier.name());
        }
    }

    #[test]
    fn a_segmented_bench_tier_splits_by_size_class_as_well() {
        assert_eq!(Suite::Bench.segments(Tier::Gate).len(), 20);
        assert_eq!(Suite::Bench.segments(Tier::Publication).len(), 25);
        let ids: Vec<&str> = Suite::Bench
            .segments(Tier::Gate)
            .iter()
            .map(|segment| segment.id)
            .collect();
        for expected in ["bench-lz4-tiny", "bench-brotli-large", "bench-zlib-medium"] {
            assert!(ids.contains(&expected), "{expected} is not a segment");
        }
    }

    #[test]
    fn every_bench_segment_names_one_competitor_and_its_tier() {
        for tier in TIERS {
            for segment in Suite::Bench.segments(tier) {
                let args: Vec<&str> = segment
                    .steps
                    .iter()
                    .flat_map(|step| step.args.iter().copied())
                    .collect();
                assert!(args.contains(&"measure"), "{}", segment.id);
                assert!(args.contains(&tier.name()), "{}", segment.id);
                assert!(args.contains(&"--codec"), "{}", segment.id);
            }
        }
    }

    #[test]
    fn the_bench_suite_says_the_entroq_column_is_empty_and_the_counter_is_not_granted() {
        assert!(Suite::Bench.coverage().contains("empty"));
        assert!(Suite::Bench.limits().contains("counter"));
        assert!(Suite::Bench.limits().contains("elapsed time"));
    }

    #[test]
    fn the_corpus_suite_says_it_measured_no_compression() {
        assert!(Suite::Corpus.coverage().contains("compresses a byte"));
        assert!(Suite::Corpus.limits().contains("no throughput"));
    }

    #[test]
    fn a_fuzz_invocation_stops_well_inside_the_segment_budget() {
        assert!(Duration::from_secs(super::FUZZ_SEGMENT_SECONDS) < SEGMENT_BUDGET);
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
