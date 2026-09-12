//! Owns the fuzz target list, and the one bounded segment an invocation advances.
//!
//! Fuzzing here is cumulative, not long. One invocation is one bounded segment, and the
//! corpus directory persists between invocations, outside the repository, so the next
//! segment continues from the accumulated corpus instead of starting cold. A corpus is never
//! deleted to start clean: it is coverage that many segments across many days paid for, and
//! deleting it discards every hour already spent.
//!
//! A crash reproducer is run output, so it lands beside the corpus, outside the repository
//! too.
//!
//! This module does not own what a target proves. A target that reads a parser or a decoder
//! arrives with the code it covers.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::campaign::Status;
use crate::clock::Utc;
use crate::error::{Error, Result};
use crate::exec::{self, Invocation, Output};
use crate::record::{Entry, FuzzManifest, Record, Status as AttemptStatus};
use crate::report;
use crate::tier::{FUZZ_SEGMENT_SECONDS, SEGMENT_BUDGET, Tier, fuzz_routine_seconds};
use crate::workspace;

/// The tier a fuzz segment is recorded at.
///
/// One invocation is one recorded, bounded, single-segment run, which is the dev shape. A
/// fuzz record is not a gate result, and no gate campaign reuses one.
const TIER: Tier = Tier::Dev;

/// The directory that holds every target's corpus, under the run root.
const CORPUS_ROOT: &str = "fuzz-corpus";

/// The directory a reproducer lands in, under the target's corpus directory.
const ARTIFACTS: &str = "artifacts";

/// What a passing fuzz segment covers.
const COVERAGE: &str = "One bounded segment of one fuzz target. The segment continues from \
                        the corpus the earlier segments left, and leaves its own additions \
                        behind for the next one.";

/// What no fuzz segment establishes.
const LIMITS: &str = "A segment that finds no crash proves nothing about the target. \
                      Coverage comes from accumulated time across many segments, so the \
                      accumulated figure is the number to read, not this segment's duration. \
                      The driver is built with the sanitizer set to none, because the pinned \
                      toolchain is stable and AddressSanitizer needs nightly, so a memory \
                      error that only a sanitizer detects is not detected here. The input \
                      digest names the repository inputs; the fuzz inputs are the corpus \
                      directory this record names and whatever libFuzzer derived from it.";

/// Whether a target has a driver at this revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// The list names it. The code it covers does not exist, so no driver does either.
    Declared,
    /// A driver exists and one segment can advance it.
    Runnable,
}

impl State {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Runnable => "runnable",
        }
    }
}

/// One fuzz target: its name, what it covers, and whether a driver exists for it.
pub struct Target {
    pub name: &'static str,
    pub covers: &'static str,
    pub state: State,
    /// Whether the driver reaches codec code.
    ///
    /// A target that exercises the runner rather than the codec is still worth running, and
    /// still not codec coverage. A routine that fuzzes on a codec campaign's behalf advances
    /// only the targets that reach the codec, so none of its budget buys coverage of itself.
    pub codec: bool,
}

const fn declared(name: &'static str, covers: &'static str) -> Target {
    Target {
        name,
        covers,
        state: State::Declared,
        codec: true,
    }
}

const fn runnable_target(name: &'static str, covers: &'static str) -> Target {
    Target {
        name,
        covers,
        state: State::Runnable,
        codec: true,
    }
}

/// Every fuzz target the project carries.
///
/// The eleven entry-point entries are the parsers and decoders. Each one becomes runnable
/// when the code it covers exists, so the set is fixed before any driver is written and no
/// driver appears without a place in it.
///
/// `mechanism` is the odd one. It covers no Entroq code at all. It proves that a bounded
/// segment runs, stops inside its budget, and keeps its corpus, which is a property of the
/// runner rather than of the codec. A passing segment on it is not coverage of anything the
/// project ships, and it stays in the list so the runner itself keeps being exercised.
const TARGETS: &[Target] = &[
    runnable_target("frame-parser", "the frame header parser"),
    declared("region-parser", "the region header parser"),
    declared("block-parser", "the block header parser"),
    declared("entropy-table-parser", "the entropy table parser"),
    declared("entropy-decoder", "the entropy decoder"),
    declared("sequence-decoder", "the sequence decoder"),
    declared("full-decoder", "the full decode path"),
    runnable_target("streaming-decoder", "the streaming decode path"),
    declared("index-parser", "the index parser"),
    declared("range-decoder", "the range decoder"),
    declared("round-trip", "encode followed by decode"),
    Target {
        name: "mechanism",
        covers: "the runner, the segment budget, and the persistent corpus. No Entroq code",
        state: State::Runnable,
        codec: false,
    },
];

/// What one fuzz segment did, in the shape a report states it.
pub struct Outcome {
    pub campaign: String,
    pub target: &'static str,
    pub revision: String,
    pub segment: String,
    pub attempt_status: AttemptStatus,
    pub duration_s: f64,
    pub status: Status,
    pub record: PathBuf,
    pub corpus: PathBuf,
    pub files_before: usize,
    pub files_after: usize,
    pub artifacts: Vec<String>,
    pub segments_total: usize,
    pub seconds_total: f64,
    pub fuzz_seconds: u64,
    pub note: Option<String>,
}

/// The declared target list, one line per target.
pub fn list() -> String {
    let mut lines = vec![format!(
        "{:<22} {:<9} {}",
        "target", "state", "what a driver covers"
    )];
    lines.extend(TARGETS.iter().map(|target| {
        format!(
            "{:<22} {:<9} {}",
            target.name,
            target.state.as_str(),
            target.covers
        )
    }));
    lines.push(String::new());
    format!("{}\n", lines.join("\n"))
}

/// Builds the driver for one target.
///
/// The build stands outside the segment, so the segment spends its budget on fuzzing rather
/// than on a compiler.
///
/// # Errors
///
/// Fails when the target has no driver, when cargo-fuzz is absent, or when the driver does
/// not build.
pub fn build(name: &str) -> Result<()> {
    let target = runnable(name)?;
    let root = workspace::root()?;
    preflight(&root)?;
    let built = exec::prepare(
        &Invocation::new(
            "cargo",
            [
                String::from("fuzz"),
                String::from("build"),
                String::from("--sanitizer"),
                String::from("none"),
                String::from(target.name),
            ],
        ),
        &root,
    )?;
    if built {
        Ok(())
    } else {
        Err(Error::tool(
            format!("cargo fuzz build --sanitizer none {}", target.name),
            "did not produce a driver",
        ))
    }
}

/// Advances one target by one bounded segment, and records what the segment did.
///
/// # Errors
///
/// Fails when the target has no driver, when cargo-fuzz is absent, when the repository
/// cannot be inspected, or when the record cannot be written. A segment that runs and finds
/// a crash is an outcome, not an error.
pub fn advance(name: &str, runs: &Path) -> Result<Outcome> {
    let target = runnable(name)?;
    let root = workspace::root()?;
    preflight(&root)?;

    let revision = workspace::revision(&root)?;
    let inputs = workspace::input_set(&root)?;
    let environment = workspace::environment()?;

    let corpus = Corpus::open(runs, target.name)?;
    let files_before = corpus.files()?;
    let segment = format!("fuzz-{}", target.name);
    let history = History::read(runs, &segment)?;

    let steps = vec![corpus.invocation(target.name, FUZZ_SEGMENT_SECONDS)];
    let created = Utc::now()?;
    let record = Record::create(runs, TIER, &created.date(), &segment)?;
    let description = format!("One bounded segment of the {} fuzz target.", target.name);
    record.write_fuzz_manifest(&FuzzManifest {
        campaign: record.campaign(),
        tier: TIER,
        topic: &segment,
        created: &created.timestamp(),
        revision: &revision,
        target: target.name,
        covers: target.covers,
        coverage: COVERAGE,
        segment: &segment,
        description: &description,
        steps: &steps.iter().map(Invocation::argv).collect::<Vec<_>>(),
        corpus: &corpus.dir,
        artifacts: &corpus.artifacts,
        corpus_files: files_before,
        prior_segments: history.segments,
        prior_seconds: history.seconds,
        fuzz_seconds: FUZZ_SEGMENT_SECONDS,
        inputs: &inputs,
        environment: &environment,
    })?;

    let evidence = Record::attempt_path(&segment, 1);
    let started = Utc::now()?.timestamp();
    let (result, took) = exec::run_steps(
        &steps,
        &root,
        SEGMENT_BUDGET,
        &Output::Directory(record.resolve(&evidence)),
    )?;
    let attempt_status = match result {
        exec::Outcome::Pass => AttemptStatus::Pass,
        exec::Outcome::Fail { .. } => AttemptStatus::Fail,
        exec::Outcome::Timeout { .. } => AttemptStatus::Timeout,
    };
    let duration_s = took.as_secs_f64();
    let note = result.note();
    record.append(&Entry {
        segment: segment.clone(),
        attempt: 1,
        started,
        duration_s,
        status: attempt_status,
        revision: revision.commit.clone(),
        input_digest: inputs.digest.clone(),
        evidence,
        note: note.clone(),
    })?;

    let outcome = Outcome {
        campaign: String::from(record.campaign()),
        target: target.name,
        revision: revision.label(),
        segment,
        attempt_status,
        duration_s,
        status: if attempt_status.is_satisfied() {
            Status::Sealed
        } else {
            Status::Failed
        },
        record: PathBuf::from(record.dir()),
        corpus: corpus.dir.clone(),
        files_before,
        files_after: corpus.files()?,
        artifacts: corpus.reproducers()?,
        segments_total: history.segments.saturating_add(1),
        seconds_total: history.seconds + duration_s,
        fuzz_seconds: FUZZ_SEGMENT_SECONDS,
        note,
    };
    if outcome.status == Status::Sealed {
        record.write_summary(&report::fuzz_summary(
            &outcome,
            &inputs,
            &environment,
            COVERAGE,
            LIMITS,
        ))?;
    }
    Ok(outcome)
}

/// The run root a routine writes its corpus under.
///
/// The argument wins, then the `RUNS` build input, then the default beside the repository. A
/// segment declared at compile time cannot carry a path the caller chose at run time, so the
/// campaign that runs one passes the root in the environment.
pub const RUNS_VARIABLE: &str = "RUNS";
pub const RUNS_DEFAULT: &str = "../runs";

/// Resolves the run root a routine writes under.
#[must_use]
pub fn runs_root(explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| std::env::var_os(RUNS_VARIABLE).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(RUNS_DEFAULT))
}

/// What one target did inside a routine.
pub struct Advanced {
    pub target: &'static str,
    pub duration_s: f64,
    pub status: AttemptStatus,
    pub files_before: usize,
    pub files_after: usize,
    pub reproducers: Vec<String>,
    pub note: Option<String>,
}

/// What a routine did.
pub struct Routine {
    pub revision: String,
    pub seconds_each: u64,
    pub advanced: Vec<Advanced>,
    pub corpus_root: PathBuf,
}

impl Routine {
    /// Whether every target advanced without a failure and without a reproducer.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.advanced
            .iter()
            .all(|one| one.status.is_satisfied() && one.reproducers.is_empty())
    }
}

/// Advances every runnable codec target by one shortened invocation, inside one segment.
///
/// A gate campaign that names fuzzing as a segment cannot spend a full invocation on each
/// target and stay inside the segment budget, so a routine shortens each one and runs them in
/// turn. The runner's own self-test target is skipped: it reaches no codec code, and a codec
/// campaign's budget should not buy coverage of the runner. Nothing is recorded here: the
/// campaign that ran the routine is the record.
///
/// # Errors
///
/// Fails when cargo-fuzz is absent, the repository cannot be inspected, or a corpus directory
/// cannot be created. A target that runs and finds a crash is an outcome, not an error.
pub fn routine(runs: &Path) -> Result<Routine> {
    let root = workspace::root()?;
    preflight(&root)?;
    let revision = workspace::revision(&root)?;

    let targets: Vec<&Target> = TARGETS
        .iter()
        .filter(|target| target.state == State::Runnable && target.codec)
        .collect();
    let seconds_each = fuzz_routine_seconds(u64::try_from(targets.len()).unwrap_or(u64::MAX));

    let started = Instant::now();
    let mut advanced = Vec::new();
    for target in targets {
        let corpus = Corpus::open(runs, target.name)?;
        let files_before = corpus.files()?;
        let steps = vec![corpus.invocation(target.name, seconds_each)];
        let budget = SEGMENT_BUDGET.saturating_sub(started.elapsed());
        let (result, took) = exec::run_steps(&steps, &root, budget, &Output::Inherit)?;
        advanced.push(Advanced {
            target: target.name,
            duration_s: took.as_secs_f64(),
            status: match result {
                exec::Outcome::Pass => AttemptStatus::Pass,
                exec::Outcome::Fail { .. } => AttemptStatus::Fail,
                exec::Outcome::Timeout { .. } => AttemptStatus::Timeout,
            },
            files_before,
            files_after: corpus.files()?,
            reproducers: corpus.reproducers()?,
            note: result.note(),
        });
    }

    Ok(Routine {
        revision: revision.label(),
        seconds_each,
        advanced,
        corpus_root: runs.join(CORPUS_ROOT),
    })
}

/// The target a `--target` argument names, when a driver exists for it.
fn runnable(name: &str) -> Result<&'static Target> {
    let Some(target) = TARGETS.iter().find(|target| target.name == name) else {
        return Err(Error::Usage(format!(
            "`{name}` is not a fuzz target. Run `entroq-run fuzz list`."
        )));
    };
    match target.state {
        State::Runnable => Ok(target),
        State::Declared => Err(Error::Unavailable(format!(
            "the {} target is declared and has no driver at this revision. It arrives with {}.",
            target.name, target.covers
        ))),
    }
}

/// Confirms that the host carries the fuzz driver tool before the runner depends on it.
fn preflight(root: &Path) -> Result<()> {
    let present = exec::probe(
        &Invocation::new("cargo", [String::from("fuzz"), String::from("--version")]),
        root,
    )?;
    if present {
        Ok(())
    } else {
        Err(Error::tool(
            "cargo fuzz",
            "is not installed on this host. Install it with `cargo install cargo-fuzz`.",
        ))
    }
}

/// One target's persistent corpus, and the directory its reproducers land in.
///
/// Both live under the run root, outside the repository, so the working tree stays clean and
/// the corpus survives every invocation.
struct Corpus {
    dir: PathBuf,
    artifacts: PathBuf,
}

impl Corpus {
    fn open(runs: &Path, target: &str) -> Result<Self> {
        let dir = runs.join(CORPUS_ROOT).join(target);
        let artifacts = dir.join(ARTIFACTS);
        fs::create_dir_all(&artifacts).map_err(|e| Error::at("create", &artifacts, e))?;
        Ok(Self {
            dir: absolute(&dir)?,
            artifacts: absolute(&artifacts)?,
        })
    }

    /// The corpus invocation, bounded by the seconds the caller allows it.
    ///
    /// The corpus directory is the one libFuzzer reads and extends. The artifact prefix is
    /// passed last, so it wins over the one the driver tool supplies for itself, which points
    /// inside the repository.
    fn invocation(&self, target: &str, seconds: u64) -> Invocation {
        Invocation::new(
            "cargo",
            [
                String::from("fuzz"),
                String::from("run"),
                String::from("--sanitizer"),
                String::from("none"),
                String::from(target),
                self.dir.display().to_string(),
                String::from("--"),
                format!("-artifact_prefix={}/", self.artifacts.display()),
                format!("-max_total_time={seconds}"),
                String::from("-print_final_stats=1"),
            ],
        )
    }

    /// The number of corpus inputs. The artifact directory is not one of them.
    fn files(&self) -> Result<usize> {
        Ok(entries(&self.dir)?.len())
    }

    fn reproducers(&self) -> Result<Vec<String>> {
        let mut found = entries(&self.artifacts)?;
        found.sort();
        Ok(found)
    }
}

fn entries(dir: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| Error::at("read", dir, e))? {
        let entry = entry.map_err(|e| Error::at("read", dir, e))?;
        if entry.path().is_file()
            && let Some(name) = entry.file_name().to_str()
        {
            names.push(String::from(name));
        }
    }
    Ok(names)
}

fn absolute(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).map_err(|e| Error::at("resolve", path, e))
}

/// What earlier segments already spent on one target.
///
/// Coverage is the sum of many bounded segments, so this is the figure a record states. It
/// counts every recorded attempt, including one that ended in a crash, because that segment
/// spent its time too.
struct History {
    segments: usize,
    seconds: f64,
}

impl History {
    fn read(runs: &Path, segment: &str) -> Result<Self> {
        let mut history = Self {
            segments: 0,
            seconds: 0.0,
        };
        for record in Record::all(runs, TIER, segment)? {
            for entry in record.entries()? {
                if entry.segment == segment {
                    history.segments = history.segments.saturating_add(1);
                    history.seconds += entry.duration_s;
                }
            }
        }
        Ok(history)
    }
}

#[cfg(test)]
mod tests {
    use super::{State, TARGETS, list, runnable, runs_root};

    /// The parser and decoder entry points that every fuzz target list must carry.
    const REQUIRED: [&str; 11] = [
        "frame-parser",
        "region-parser",
        "block-parser",
        "entropy-table-parser",
        "entropy-decoder",
        "sequence-decoder",
        "full-decoder",
        "streaming-decoder",
        "index-parser",
        "range-decoder",
        "round-trip",
    ];

    #[test]
    fn a_routine_advances_every_runnable_codec_target_and_no_other() {
        let advanced: Vec<&str> = TARGETS
            .iter()
            .filter(|target| target.state == State::Runnable && target.codec)
            .map(|target| target.name)
            .collect();
        assert_eq!(advanced, ["frame-parser", "streaming-decoder"]);
        assert!(!advanced.contains(&"mechanism"), "the runner's own target");
    }

    #[test]
    fn every_required_target_reaches_the_codec() {
        for target in TARGETS.iter().filter(|target| target.codec) {
            assert!(
                REQUIRED.contains(&target.name),
                "{} is a codec target and not a required entry point",
                target.name
            );
        }
    }

    #[test]
    fn a_routine_takes_the_run_root_it_is_given_before_the_one_it_can_find() {
        let named = std::path::PathBuf::from("/somewhere/else");
        assert_eq!(runs_root(Some(named.clone())), named);
        assert!(!runs_root(None).as_os_str().is_empty());
    }

    #[test]
    fn every_required_target_is_declared() {
        for required in REQUIRED {
            let found = TARGETS.iter().find(|target| target.name == required);
            assert!(found.is_some(), "{required} is not declared");
        }
    }

    #[test]
    fn no_target_is_declared_twice() {
        let mut names: Vec<&str> = TARGETS.iter().map(|target| target.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn every_target_beyond_the_required_set_carries_a_driver() {
        for target in TARGETS {
            if !REQUIRED.contains(&target.name) {
                assert_eq!(target.state, State::Runnable, "{}", target.name);
            }
        }
    }

    /// The drivers that exist at this revision, and nothing else.
    ///
    /// A target becomes runnable when its driver is written, so this list grows one entry at
    /// a time. It is asserted exactly, so a driver cannot be marked runnable without the
    /// list that names it being updated in the same change.
    #[test]
    fn the_runnable_set_is_exactly_the_drivers_that_exist() {
        let found: Vec<&str> = TARGETS
            .iter()
            .filter(|target| target.state == State::Runnable)
            .map(|target| target.name)
            .collect();
        assert_eq!(found, ["frame-parser", "streaming-decoder", "mechanism"]);
    }

    #[test]
    fn the_mechanism_driver_says_it_covers_no_entroq_code() {
        let found = TARGETS
            .iter()
            .find(|target| target.name == "mechanism")
            .map(|target| target.covers);
        assert_eq!(
            found.map(|covers| covers.contains("No Entroq code")),
            Some(true)
        );
    }

    #[test]
    fn a_declared_target_names_what_it_waits_for_rather_than_running_empty() {
        let refused = runnable("index-parser");
        let message = refused.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("no driver at this revision"), "{message}");
        assert!(message.contains("the index parser"), "{message}");
    }

    #[test]
    fn an_unknown_target_is_rejected() {
        assert!(runnable("decoder").is_err());
        assert!(runnable("").is_err());
    }

    #[test]
    fn every_driver_that_exists_is_runnable() {
        for name in ["frame-parser", "streaming-decoder", "mechanism"] {
            assert_eq!(runnable(name).map(|target| target.name).ok(), Some(name));
        }
    }

    #[test]
    fn the_listing_states_every_target_and_its_state() {
        let text = list();
        for target in TARGETS {
            assert!(text.contains(target.name), "{} is not listed", target.name);
        }
        assert!(text.contains("declared"));
        assert!(text.contains("runnable"));
    }
}
