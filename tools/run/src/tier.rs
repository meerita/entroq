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

/// The wall clock a routine has for fuzzing, across every target it advances.
///
/// A campaign that names fuzzing as one segment runs every target inside that segment's
/// budget, and a full invocation each does not fit one. The figure is the segment budget less
/// the room the corpus load, the driver check, and the process exit of each target need.
const FUZZ_ROUTINE_BUDGET_SECONDS: u64 = 80;

/// The wall clock one target gets inside a routine that advances `targets` of them.
///
/// The share is computed rather than fixed, so a routine that gains a target shortens every
/// invocation instead of overrunning the segment they share. It never buys more than one full
/// invocation: a routine is a way to fit several targets into one segment, not a way to fuzz
/// one of them for longer than `fuzz run` would.
#[must_use]
pub const fn fuzz_routine_seconds(targets: u64) -> u64 {
    if targets == 0 {
        return 0;
    }
    let share = FUZZ_ROUTINE_BUDGET_SECONDS.div_euclid(targets);
    if share < FUZZ_SEGMENT_SECONDS {
        share
    } else {
        FUZZ_SEGMENT_SECONDS
    }
}

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

/// One benchmark segment: one codec, at one tier, over the size classes the tier names or
/// over the one class the segment names.
///
/// A segment is one harness process, so the resident-set figure it reports is its own and a
/// failure blocks one codec rather than the campaign. Every codec is driven in-process: a
/// competitor through the library the laboratory built, and the subject through the crate of
/// this workspace.
macro_rules! bench_segment {
    ($tier:literal, $codec:literal) => {
        Segment {
            id: concat!("bench-", $codec),
            description: concat!(
                "Every metric the ",
                $tier,
                " tier covers, for ",
                $codec,
                ", measured in-process."
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
    bench_segment!("smoke", "entroq"),
    bench_segment!("smoke", "lz4"),
    bench_segment!("smoke", "zstd"),
    bench_segment!("smoke", "brotli"),
    bench_segment!("smoke", "snappy"),
    bench_segment!("smoke", "zlib"),
];

const BENCH_DEV_SEGMENTS: &[Segment] = &[
    bench_segment!("dev", "entroq"),
    bench_segment!("dev", "lz4"),
    bench_segment!("dev", "zstd"),
    bench_segment!("dev", "brotli"),
    bench_segment!("dev", "snappy"),
    bench_segment!("dev", "zlib"),
];

/// One baseline segment: one competitor, at one group of its operating points, over one size
/// class.
///
/// The gate tier measures every point the catalog pins, and the cost of one point spans three
/// orders of magnitude inside one project. Zstandard 22 and Brotli q11 each cost more over
/// the large class than every cheaper point of the same competitor put together, so a segment
/// covers one group rather than one competitor. The group is what keeps a segment inside its
/// budget; the class is what keeps it inside the input the tier reaches.
///
/// The segment identifier is passed to the harness, so the result document names the segment
/// the runner recorded it under rather than a name it derived for itself.
macro_rules! baseline_segment {
    ($codec:literal, $group:literal, $class:literal) => {
        Segment {
            id: concat!("baseline-", $codec, "-", $group, "-", $class),
            description: concat!(
                "Every metric the gate tier covers, for ",
                $codec,
                " at its ",
                $group,
                " operating points, over the ",
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
                    "gate",
                    "--codec",
                    $codec,
                    "--points",
                    $group,
                    "--class",
                    $class,
                    "--segment",
                    concat!("baseline-", $codec, "-", $group, "-", $class),
                ],
            }],
        }
    };
}

/// The baseline. One segment per codec, operating point group, and size class, which is the
/// unit that holds one budget and that a resumed campaign re-runs alone.
///
/// The subject comes first, because a comparison with no subject in it is a table of
/// competitors. It has one operating point group holding both of its modes, because one
/// segment can afford them together.
const BENCH_GATE_SEGMENTS: &[Segment] = &[
    baseline_segment!("entroq", "default", "tiny"),
    baseline_segment!("entroq", "default", "small"),
    baseline_segment!("entroq", "default", "medium"),
    baseline_segment!("entroq", "default", "large"),
    baseline_segment!("lz4", "fast", "tiny"),
    baseline_segment!("lz4", "fast", "small"),
    baseline_segment!("lz4", "fast", "medium"),
    baseline_segment!("lz4", "fast", "large"),
    baseline_segment!("lz4", "hc", "tiny"),
    baseline_segment!("lz4", "hc", "small"),
    baseline_segment!("lz4", "hc", "medium"),
    baseline_segment!("lz4", "hc", "large"),
    baseline_segment!("zstd", "low", "tiny"),
    baseline_segment!("zstd", "low", "small"),
    baseline_segment!("zstd", "low", "medium"),
    baseline_segment!("zstd", "low", "large"),
    baseline_segment!("zstd", "high", "tiny"),
    baseline_segment!("zstd", "high", "small"),
    baseline_segment!("zstd", "high", "medium"),
    baseline_segment!("zstd", "high", "large"),
    baseline_segment!("zstd", "max", "tiny"),
    baseline_segment!("zstd", "max", "small"),
    baseline_segment!("zstd", "max", "medium"),
    baseline_segment!("zstd", "max", "large"),
    baseline_segment!("brotli", "low", "tiny"),
    baseline_segment!("brotli", "low", "small"),
    baseline_segment!("brotli", "low", "medium"),
    baseline_segment!("brotli", "low", "large"),
    baseline_segment!("brotli", "high", "tiny"),
    baseline_segment!("brotli", "high", "small"),
    baseline_segment!("brotli", "high", "medium"),
    baseline_segment!("brotli", "high", "large"),
    baseline_segment!("brotli", "max", "tiny"),
    baseline_segment!("brotli", "max", "small"),
    baseline_segment!("brotli", "max", "medium"),
    baseline_segment!("brotli", "max", "large"),
    baseline_segment!("snappy", "default", "tiny"),
    baseline_segment!("snappy", "default", "small"),
    baseline_segment!("snappy", "default", "medium"),
    baseline_segment!("snappy", "default", "large"),
    baseline_segment!("zlib", "low", "tiny"),
    baseline_segment!("zlib", "low", "small"),
    baseline_segment!("zlib", "low", "medium"),
    baseline_segment!("zlib", "low", "large"),
    baseline_segment!("zlib", "high", "tiny"),
    baseline_segment!("zlib", "high", "small"),
    baseline_segment!("zlib", "high", "medium"),
    baseline_segment!("zlib", "high", "large"),
];

const BENCH_PUBLICATION_SEGMENTS: &[Segment] = &[
    bench_segment!("publication", "entroq", "tiny"),
    bench_segment!("publication", "entroq", "small"),
    bench_segment!("publication", "entroq", "medium"),
    bench_segment!("publication", "entroq", "large"),
    bench_segment!("publication", "entroq", "huge"),
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

/// One skeleton segment: one test of the gate-scale proof binary.
///
/// Each proof is its own test, so a segment names one and a resumed campaign re-runs only the
/// proof that did not hold. The tests are ignored by default, because the dev tier proves the
/// same properties on a small input set and these are sized to the gate's.
macro_rules! skeleton_segment {
    ($id:literal, $test:literal, $description:literal) => {
        Segment {
            id: $id,
            description: $description,
            steps: &[Step {
                program: "cargo",
                args: &[
                    "test",
                    "--release",
                    "--quiet",
                    "--package",
                    "codec",
                    "--test",
                    "skeleton",
                    "--",
                    "--ignored",
                    "--nocapture",
                    "--exact",
                    $test,
                ],
            }],
        }
    };
}

/// The format skeleton's closing gate: every heavy proof of it, one per segment.
const SKELETON_SEGMENTS: &[Segment] = &[
    skeleton_segment!(
        "skeleton-roundtrip-tiny",
        "skeleton_roundtrip_tiny",
        "Every length below one kibibyte round-trips, through five content shapes, both \
         integrity modes, four chunkings, and a pseudo-random one."
    ),
    skeleton_segment!(
        "skeleton-roundtrip-small",
        "skeleton_roundtrip_small",
        "Every kibibyte boundary to sixty-four, and one byte either side of each, round-trips."
    ),
    skeleton_segment!(
        "skeleton-roundtrip-medium",
        "skeleton_roundtrip_medium",
        "The region boundary and the sizes around it round-trip."
    ),
    skeleton_segment!(
        "skeleton-roundtrip-large",
        "skeleton_roundtrip_large",
        "Multi-region inputs from four to twelve mebibytes round-trip."
    ),
    skeleton_segment!(
        "skeleton-truncation-matrix",
        "skeleton_truncation_matrix",
        "Every stream of the structural fixture set, cut at every byte offset, is refused \
         rather than read as a whole frame."
    ),
    skeleton_segment!(
        "skeleton-chunk-permutation-encode",
        "skeleton_chunk_permutation_encode",
        "One input encodes to the same bytes at every input and output chunk size."
    ),
    skeleton_segment!(
        "skeleton-chunk-permutation-decode",
        "skeleton_chunk_permutation_decode",
        "One stream decodes to the same bytes at every input and output chunk size."
    ),
    Segment {
        id: "skeleton-memory-curve-1gib",
        description: "The memory growth curve from one mebibyte to one gibibyte of logical \
                      input is flat against the criterion the tool states before it runs.",
        steps: &[Step {
            program: "cargo",
            args: &[
                "run",
                "--quiet",
                "--release",
                "--package",
                "entroq-proof",
                "--",
                "curve",
            ],
        }],
    },
    Segment {
        id: "skeleton-byteorder-cross-arch",
        description: "Two architectures write the same format vectors and each decodes the \
                      other's.",
        steps: &[Step {
            program: "ci/byteorder.sh",
            args: &["run"],
        }],
    },
    Segment {
        id: "skeleton-fuzz-routine",
        description: "Every runnable fuzz target that reaches codec code advances by one \
                      bounded invocation, and none leaves a reproducer behind.",
        steps: &[Step {
            program: "cargo",
            args: &[
                "run",
                "--quiet",
                "--release",
                "--package",
                "entroq-run",
                "--",
                "fuzz",
                "routine",
            ],
        }],
    },
];

/// One compressed segment: one test of the gate-scale proof binary of the compressed block.
///
/// The same shape as a skeleton segment and a separate macro, because the two name different
/// test binaries and a segment names its binary rather than deriving it.
macro_rules! compressed_segment {
    ($id:literal, $test:literal, $description:literal) => {
        Segment {
            id: $id,
            description: $description,
            steps: &[Step {
                program: "cargo",
                args: &[
                    "test",
                    "--release",
                    "--quiet",
                    "--package",
                    "codec",
                    "--test",
                    "compressed",
                    "--",
                    "--ignored",
                    "--nocapture",
                    "--exact",
                    $test,
                ],
            }],
        }
    };
}

/// The compressed block's closing gate: every heavy proof of it, one per segment.
const COMPRESSED_SEGMENTS: &[Segment] = &[
    compressed_segment!(
        "compressed-roundtrip-tiny",
        "compressed_roundtrip_tiny",
        "Every length below one kibibyte round-trips, through every content class, both \
         integrity modes, four chunkings, and a pseudo-random one."
    ),
    compressed_segment!(
        "compressed-roundtrip-small",
        "compressed_roundtrip_small",
        "Every kibibyte boundary to sixty-four, and one byte either side of each, round-trips."
    ),
    compressed_segment!(
        "compressed-roundtrip-medium",
        "compressed_roundtrip_medium",
        "The block boundary, the region boundary and the sizes around them round-trip, so the \
         tables in force and the offset cache are carried across one and discarded at the \
         other."
    ),
    compressed_segment!(
        "compressed-roundtrip-large",
        "compressed_roundtrip_large",
        "Multi-region inputs from four to twelve mebibytes round-trip."
    ),
    compressed_segment!(
        "compressed-chunk-permutation-encode",
        "compressed_chunk_permutation_encode",
        "One input encodes to the same bytes at every input and output chunk size."
    ),
    compressed_segment!(
        "compressed-chunk-permutation-decode",
        "compressed_chunk_permutation_decode",
        "One stream decodes to the same bytes at every input and output chunk size."
    ),
    compressed_segment!(
        "compressed-corruption-matrix",
        "compressed_corruption_matrix",
        "Every byte of every fixture stream, mutated in four byte classes, yields a typed \
         error of a declared class or a decode that holds the content length its frame \
         declares."
    ),
    compressed_segment!(
        "compressed-truncation-matrix",
        "compressed_truncation_matrix",
        "Every stream of the structural fixture set, cut at every byte offset, is refused \
         rather than read as a whole frame."
    ),
    Segment {
        id: "compressed-memory-curve-1gib",
        description: "The memory growth curve from one mebibyte to one gibibyte of logical \
                      input holds the five-clause criterion the tool states before it runs, \
                      including the table refusal that precedes the allocation counter.",
        steps: &[Step {
            program: "cargo",
            args: &[
                "run",
                "--quiet",
                "--release",
                "--package",
                "entroq-proof",
                "--",
                "curve",
            ],
        }],
    },
    compressed_segment!(
        "compressed-corpus-coverage",
        "compressed_corpus_coverage",
        "Every representative corpus entry round-trips, and the block type histogram of each \
         is recorded. A host that does not hold the corpus fails this segment rather than \
         passing on an empty set."
    ),
    compressed_segment!(
        "compressed-block-types-visible",
        "compressed_block_types_visible",
        "The type of every block of every frame is read from its header, without expanding a \
         payload, and the three types are each reached."
    ),
    Segment {
        id: "compressed-vectors-cross-arch",
        description: "Two architectures write the same format vectors, the compressed ones \
                      included, and each decodes the other's.",
        steps: &[Step {
            program: "ci/byteorder.sh",
            args: &["run"],
        }],
    },
    Segment {
        id: "compressed-fuzz-routine",
        description: "Every runnable fuzz target that reaches codec code advances by one \
                      bounded invocation, and none leaves a reproducer behind.",
        steps: &[Step {
            program: "cargo",
            args: &[
                "run",
                "--quiet",
                "--release",
                "--package",
                "entroq-run",
                "--",
                "fuzz",
                "routine",
            ],
        }],
    },
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
    /// The benchmark: every competitor measured in-process. A cheap tier runs one segment
    /// per competitor. The gate tier runs one per competitor, operating point group, and
    /// size class, because a segment holds one budget.
    Bench,
    /// The format skeleton's closing gate: every heavy proof the cheap tiers defer.
    Skeleton,
    /// The compressed block's closing gate: every heavy proof the cheap tiers defer.
    Compressed,
}

impl Suite {
    /// Parses the suite a `--suite` argument names.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "workspace" => Some(Self::Workspace),
            "lab" => Some(Self::Lab),
            "corpus" => Some(Self::Corpus),
            "bench" => Some(Self::Bench),
            "skeleton" => Some(Self::Skeleton),
            "compressed" => Some(Self::Compressed),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Lab => "lab",
            Self::Corpus => "corpus",
            Self::Bench => "bench",
            Self::Skeleton => "skeleton",
            Self::Compressed => "compressed",
        }
    }

    /// The record directory name a campaign takes when the caller names no topic.
    pub const fn default_topic(self) -> &'static str {
        match self {
            Self::Workspace => "workspace-gate",
            Self::Lab => "lab-build",
            Self::Corpus => "corpus-registry",
            Self::Bench => "bench-baseline",
            Self::Skeleton => "skeleton-gate",
            Self::Compressed => "compressed-gate",
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
            Self::Skeleton => match tier {
                Tier::Smoke | Tier::Dev => &[],
                Tier::Gate | Tier::Publication => SKELETON_SEGMENTS,
            },
            Self::Compressed => match tier {
                Tier::Smoke | Tier::Dev => &[],
                Tier::Gate | Tier::Publication => COMPRESSED_SEGMENTS,
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
                "Workspace gates only. The codec carries the format contract and the \
                 streaming pair that drives it, and no compression, so a segment \
                 round-trips stored content at every chunk size through its unit tests, \
                 asserts the error each malformed structure produces, and asserts the \
                 memory bound each direction declares. No segment compresses a byte and \
                 no segment fuzzes."
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
                "Measurement only. Each segment drives one codec in-process over the \
                 registered corpus entries its tier's budget reaches, and emits one \
                 machine-readable result. A competitor is driven through the library the \
                 laboratory built; the subject is driven through the crate of this \
                 workspace, at the revision the result records, so both sides cross one \
                 call and one process. A segmented tier covers one operating point group \
                 and one size class per segment, and the groups of a codec cover every \
                 point declared for it. The subject declares both of its modes in one \
                 group."
            }
            Self::Skeleton => {
                "The format skeleton's closing gate. Seven segments round-trip generated \
                 content across four size classes, cut every fixture stream at every byte \
                 offset, and drive both machines at every chunk size, all through the \
                 public API. One measures what the streaming pair holds as the logical \
                 input grows from one mebibyte to one gibibyte, reading the process's own \
                 allocator and resident set. One writes the format vectors on two \
                 architectures and has each decode the other's. One advances every runnable \
                 fuzz target that reaches codec code. No segment compresses a byte: every block this revision \
                 writes is stored."
            }
            Self::Compressed => {
                "The compressed block's closing gate. Eight segments drive the public API \
                 over generated content in nine classes across four size classes, at every \
                 chunk size and at a pseudo-random one, cut every fixture stream at every \
                 byte offset, mutate every byte of every fixture stream in four byte \
                 classes, and read the type of every block of every frame from its header \
                 without expanding a payload. One measures what the streaming pair holds as \
                 the logical input grows from one mebibyte to one gibibyte, and whether a \
                 block above the table ceiling is refused before the allocation counter \
                 moves. One round-trips the registered corpus entries and records the block \
                 type histogram of each. One writes the format vectors on two architectures \
                 and has each decode the other's. One advances every runnable fuzz target \
                 that reaches codec code. No segment measures a throughput and no segment \
                 measures a competitor."
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
                 environments. It measured no Entroq number, because the harness links no \
                 Entroq codec. A cycle count and an instruction count need a performance \
                 monitor unit this host does not grant, so all three counter metrics report \
                 unavailable with the reason and no number is derived from elapsed time. A \
                 tier's input budget bounds the entries a segment reads, and every entry a \
                 budget did not reach is named in the result rather than dropped in silence. \
                 Every competitor was measured with no integrity check: each one is driven \
                 through the format of its own project that carries no checksum, and each \
                 result names that setting and the container checksum it was not produced \
                 under. Every number is single threaded except the parallel scaling metric, \
                 which states its own worker count."
            }
            Self::Skeleton => {
                "The campaign measured no compression ratio and no throughput, and it \
                 compares no competitor. The memory figures are the declared bound, the \
                 process allocator counters, and the resident-set high-water mark of the \
                 child that performed each run; none of them attributes memory to one call. \
                 The cross-architecture segment runs one lane under emulation, which \
                 settles byte order and nothing about speed: no duration from either lane \
                 is a result. The fuzz segment shortens each target's invocation to fit one \
                 segment, so the accumulated figure is the one to read, not this run's. The \
                 round trips cover generated content, not the registered corpus."
            }
            Self::Compressed => {
                "The campaign ran on one host and one architecture. It measured no \
                 throughput and no competitor, and the compressed sizes it records are \
                 lengths rather than a ratio anyone compared. The memory figures are the \
                 declared bound, the process allocator counters, and the resident-set \
                 high-water mark of the child that performed each run; none of them \
                 attributes memory to one call, and the per-block allocation figure is a \
                 bounded cost rather than the absence of one. Version 1 computes no \
                 integrity field, so the corruption matrix records how many mutations were \
                 accepted with other content instead of asserting that none was. The \
                 cross-architecture segment runs one lane under emulation, which settles \
                 byte order and nothing about speed. The fuzz segment shortens each target's \
                 invocation to fit one segment, so the accumulated figure is the one to \
                 read, not this run's. The corpus segment reads a prefix of each entry rather \
                 than the whole of it, which is what keeps it inside one segment budget."
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
    const SUITES: [Suite; 6] = [
        Suite::Workspace,
        Suite::Lab,
        Suite::Corpus,
        Suite::Bench,
        Suite::Skeleton,
        Suite::Compressed,
    ];

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
    fn the_bench_suite_runs_one_segment_per_codec_at_a_tier_that_holds_one_budget() {
        // Five competitors and the subject.
        for tier in [Tier::Smoke, Tier::Dev] {
            assert_eq!(Suite::Bench.segments(tier).len(), 6, "{}", tier.name());
        }
    }

    #[test]
    fn the_baseline_splits_by_operating_point_group_and_by_size_class() {
        assert_eq!(Suite::Bench.segments(Tier::Gate).len(), 48);
        assert_eq!(Suite::Bench.segments(Tier::Publication).len(), 30);
        let ids: Vec<&str> = Suite::Bench
            .segments(Tier::Gate)
            .iter()
            .map(|segment| segment.id)
            .collect();
        for expected in [
            "baseline-entroq-default-tiny",
            "baseline-entroq-default-large",
            "baseline-lz4-fast-tiny",
            "baseline-brotli-max-large",
            "baseline-zstd-max-large",
            "baseline-snappy-default-medium",
            "baseline-zlib-high-small",
        ] {
            assert!(ids.contains(&expected), "{expected} is not a segment");
        }
    }

    #[test]
    fn a_baseline_segment_measures_under_the_identifier_the_runner_records_it_under() {
        for segment in Suite::Bench.segments(Tier::Gate) {
            let args: Vec<&str> = segment
                .steps
                .iter()
                .flat_map(|step| step.args.iter().copied())
                .collect();
            assert!(args.contains(&"--points"), "{}", segment.id);
            assert!(args.contains(&segment.id), "{}", segment.id);
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
    fn the_bench_suite_says_both_sides_cross_one_boundary_and_the_counter_is_not_granted() {
        assert!(Suite::Bench.coverage().contains("one call and one process"));
        assert!(Suite::Bench.limits().contains("counter"));
        assert!(Suite::Bench.limits().contains("elapsed time"));
    }

    #[test]
    fn every_bench_tier_measures_the_subject_and_not_only_its_competitors() {
        for tier in TIERS {
            let segments = Suite::Bench.segments(tier);
            assert!(
                segments.iter().any(|segment| segment.id.contains("entroq")),
                "the {} tier measures no Entroq segment",
                tier.name()
            );
        }
    }

    #[test]
    fn the_corpus_suite_says_it_measured_no_compression() {
        assert!(Suite::Corpus.coverage().contains("compresses a byte"));
        assert!(Suite::Corpus.limits().contains("no throughput"));
    }

    #[test]
    fn the_skeleton_suite_runs_every_segment_the_closing_gate_names() {
        let segments = Suite::Skeleton.segments(Tier::Gate);
        let ids: Vec<&str> = segments.iter().map(|segment| segment.id).collect();
        for expected in [
            "skeleton-roundtrip-tiny",
            "skeleton-roundtrip-small",
            "skeleton-roundtrip-medium",
            "skeleton-roundtrip-large",
            "skeleton-truncation-matrix",
            "skeleton-chunk-permutation-encode",
            "skeleton-chunk-permutation-decode",
            "skeleton-memory-curve-1gib",
            "skeleton-byteorder-cross-arch",
            "skeleton-fuzz-routine",
        ] {
            assert!(ids.contains(&expected), "{expected} is not a segment");
        }
        assert_eq!(segments.len(), 10);
    }

    #[test]
    fn the_skeleton_suite_has_no_segments_at_a_tier_that_does_not_segment() {
        assert!(Suite::Skeleton.segments(Tier::Smoke).is_empty());
        assert!(Suite::Skeleton.segments(Tier::Dev).is_empty());
    }

    #[test]
    fn every_skeleton_round_trip_segment_names_one_ignored_test_exactly() {
        for segment in Suite::Skeleton.segments(Tier::Gate) {
            if !segment.id.starts_with("skeleton-roundtrip") {
                continue;
            }
            let args: Vec<&str> = segment
                .steps
                .iter()
                .flat_map(|step| step.args.iter().copied())
                .collect();
            assert!(args.contains(&"--ignored"), "{}", segment.id);
            assert!(args.contains(&"--exact"), "{}", segment.id);
            assert!(args.contains(&"--release"), "{}", segment.id);
        }
    }

    #[test]
    fn the_skeleton_suite_says_it_compressed_nothing_and_claims_no_speed() {
        assert!(Suite::Skeleton.coverage().contains("compresses a byte"));
        assert!(Suite::Skeleton.limits().contains("no throughput"));
        assert!(Suite::Skeleton.limits().contains("emulation"));
    }

    #[test]
    fn the_compressed_suite_runs_every_segment_the_closing_gate_names() {
        let segments = Suite::Compressed.segments(Tier::Gate);
        let ids: Vec<&str> = segments.iter().map(|segment| segment.id).collect();
        for expected in [
            "compressed-roundtrip-tiny",
            "compressed-roundtrip-small",
            "compressed-roundtrip-medium",
            "compressed-roundtrip-large",
            "compressed-chunk-permutation-encode",
            "compressed-chunk-permutation-decode",
            "compressed-corruption-matrix",
            "compressed-truncation-matrix",
            "compressed-memory-curve-1gib",
            "compressed-corpus-coverage",
            "compressed-block-types-visible",
            "compressed-vectors-cross-arch",
            "compressed-fuzz-routine",
        ] {
            assert!(ids.contains(&expected), "{expected} is not a segment");
        }
        assert_eq!(segments.len(), 13);
    }

    #[test]
    fn the_compressed_suite_has_no_segments_at_a_tier_that_does_not_segment() {
        assert!(Suite::Compressed.segments(Tier::Smoke).is_empty());
        assert!(Suite::Compressed.segments(Tier::Dev).is_empty());
    }

    #[test]
    fn every_compressed_test_segment_names_one_ignored_test_of_its_own_binary() {
        for segment in Suite::Compressed.segments(Tier::Gate) {
            let args: Vec<&str> = segment
                .steps
                .iter()
                .flat_map(|step| step.args.iter().copied())
                .collect();
            if !args.contains(&"test") {
                continue;
            }
            assert!(args.contains(&"compressed"), "{}", segment.id);
            assert!(args.contains(&"--ignored"), "{}", segment.id);
            assert!(args.contains(&"--exact"), "{}", segment.id);
            assert!(args.contains(&"--release"), "{}", segment.id);
        }
    }

    #[test]
    fn the_compressed_suite_says_what_no_integrity_field_costs_and_claims_no_speed() {
        let limits = Suite::Compressed.limits();
        assert!(limits.contains("no integrity field"), "{limits}");
        assert!(limits.contains("no throughput"), "{limits}");
        assert!(
            Suite::Compressed
                .coverage()
                .contains("no segment measures a competitor"),
            "{}",
            Suite::Compressed.coverage()
        );
    }

    #[test]
    fn the_two_closing_gates_share_no_segment_identifier() {
        for skeleton in Suite::Skeleton.segments(Tier::Gate) {
            for compressed in Suite::Compressed.segments(Tier::Gate) {
                assert_ne!(skeleton.id, compressed.id);
            }
        }
    }

    #[test]
    fn a_routine_shares_one_segment_among_however_many_targets_it_advances() {
        for targets in 1_u64..=8 {
            let each = super::fuzz_routine_seconds(targets);
            assert!(
                Duration::from_secs(each.saturating_mul(targets)) < SEGMENT_BUDGET,
                "{targets} targets"
            );
            assert!(each <= super::FUZZ_SEGMENT_SECONDS, "{targets} targets");
        }
    }

    #[test]
    fn a_routine_with_no_target_asks_for_no_time() {
        assert_eq!(super::fuzz_routine_seconds(0), 0);
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
