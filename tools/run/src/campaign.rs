//! Owns campaign execution: which segments run, which are reused, what each attempt records,
//! and whether the campaign seals.
//!
//! A segment is the unit of work, of failure, and of resume. A segment whose last attempt
//! passed at the current revision against the current input set is reused, and its journal
//! line says it was reused rather than presenting the result as fresh. Anything else runs
//! again.
//!
//! Sealing is decided here and nowhere else. A campaign seals only when every segment of its
//! tier is satisfied, at one revision, against one input set. An unrecorded tier never seals,
//! because it leaves nothing to seal.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cli::{Mode, Request};
use crate::clock::Utc;
use crate::error::{Error, Result};
use crate::exec::{self, Output};
use crate::record::{Entry, Manifest, Record, Status as AttemptStatus};
use crate::report::{self, Summary};
use crate::tier::{COVERAGE, SEGMENT_BUDGET, Segment, Tier};
use crate::workspace::{self, Environment, InputSet, Revision};

/// How a campaign ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Every segment satisfied, at one revision, and the record holds the proof.
    Sealed,
    /// Every segment satisfied at an unrecorded tier, which leaves nothing to seal.
    Passed,
    /// At least one segment failed or overran its budget.
    Failed,
}

impl Status {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sealed => "sealed",
            Self::Passed => "passed",
            Self::Failed => "failed",
        }
    }

    pub const fn is_success(self) -> bool {
        !matches!(self, Self::Failed)
    }
}

/// What a campaign did, in the shape a report states it.
pub struct Outcome {
    pub tier: Tier,
    pub campaign: Option<String>,
    pub revision: String,
    pub total: usize,
    pub satisfied: usize,
    pub reused: usize,
    pub duration: Duration,
    pub status: Status,
    pub record: Option<PathBuf>,
    pub blockers: Vec<String>,
}

struct Context<'a> {
    root: &'a Path,
    revision: &'a Revision,
    inputs: &'a InputSet,
    record: Option<&'a Record>,
    tier: Tier,
    started: Instant,
}

/// Runs a campaign, or continues the most recent one.
///
/// Prints one line per segment as it ends, and appends its journal line at the same moment,
/// so an interrupted campaign leaves a usable record of everything that already finished.
///
/// # Errors
///
/// Fails when the repository cannot be inspected, the record cannot be written, or a segment
/// cannot be started. A segment that runs and fails is an outcome, not an error.
pub fn execute(request: &Request, mode: Mode) -> Result<Outcome> {
    let started = Instant::now();
    let root = workspace::root()?;
    let revision = workspace::revision(&root)?;
    let inputs = workspace::input_set(&root)?;
    let environment = workspace::environment()?;
    let tier = request.tier;
    let segments = tier.segments();

    let record = open_record(request, mode, &revision, &inputs, &environment)?;
    let prior = match record.as_ref() {
        Some(record) => record.entries()?,
        None => Vec::new(),
    };
    let context = Context {
        root: &root,
        revision: &revision,
        inputs: &inputs,
        record: record.as_ref(),
        tier,
        started,
    };

    let mut written: Vec<Entry> = Vec::new();
    let mut satisfied: usize = 0;
    let mut reused: usize = 0;
    let mut blockers: Vec<String> = Vec::new();

    for segment in segments {
        let entry = attempt(&context, segment, &prior, mode)?;
        println!(
            "{}",
            report::segment_line(
                &entry.segment,
                entry.status.as_str(),
                entry.duration_s,
                entry.note.as_deref(),
            )
        );
        if let Some(record) = context.record {
            record.append(&entry)?;
        }
        if entry.status.is_satisfied() {
            satisfied = satisfied.saturating_add(1);
        } else {
            blockers.push(entry.segment.clone());
        }
        if entry.status == AttemptStatus::Cached {
            reused = reused.saturating_add(1);
        }
        written.push(entry);
    }

    let status = if blockers.is_empty() {
        if record.is_some() {
            Status::Sealed
        } else {
            Status::Passed
        }
    } else {
        Status::Failed
    };
    let outcome = Outcome {
        tier,
        campaign: record.as_ref().map(|r| String::from(r.campaign())),
        revision: revision.label(),
        total: segments.len(),
        satisfied,
        reused,
        duration: started.elapsed(),
        status,
        record: record.as_ref().map(|r| PathBuf::from(r.dir())),
        blockers,
    };

    if let (Status::Sealed, Some(record)) = (status, record.as_ref()) {
        let summary = Summary {
            outcome: &outcome,
            inputs: &inputs,
            environment: &environment,
            entries: &written,
            coverage: COVERAGE,
        };
        record.write_summary(&report::summary(&summary))?;
    }
    Ok(outcome)
}

fn open_record(
    request: &Request,
    mode: Mode,
    revision: &Revision,
    inputs: &InputSet,
    environment: &Environment,
) -> Result<Option<Record>> {
    if !request.tier.is_recorded() {
        return Ok(None);
    }
    let runs = request
        .runs
        .as_deref()
        .ok_or_else(|| Error::Usage(String::from("a recorded tier needs --runs")))?;
    match mode {
        Mode::Fresh => {
            let created = Utc::now()?;
            let record = Record::create(runs, request.tier, &created.date(), &request.topic)?;
            record.write_manifest(&Manifest {
                campaign: record.campaign(),
                tier: request.tier,
                topic: &request.topic,
                created: &created.timestamp(),
                revision,
                coverage: COVERAGE,
                segments: request.tier.segments(),
                inputs,
                environment,
            })?;
            Ok(Some(record))
        }
        Mode::Resume => Record::latest(runs, request.tier)?
            .map(Some)
            .ok_or_else(|| {
                Error::record(
                    format!("{} tier", request.tier.name()),
                    "has no campaign to resume. Start one first.",
                )
            }),
    }
}

fn attempt(context: &Context<'_>, segment: &Segment, prior: &[Entry], mode: Mode) -> Result<Entry> {
    let started = Utc::now()?.timestamp();
    let number = next_attempt(prior, segment.id);

    if mode == Mode::Resume
        && let Some(reusable) = reusable(prior, segment.id, context.revision, context.inputs)
    {
        return Ok(Entry {
            segment: String::from(segment.id),
            attempt: number,
            started,
            duration_s: 0.0,
            status: AttemptStatus::Cached,
            revision: context.revision.commit.clone(),
            input_digest: context.inputs.digest.clone(),
            evidence: reusable.evidence.clone(),
            note: Some(format!(
                "reuses attempt {:02}, which passed at this revision against these inputs",
                reusable.attempt
            )),
        });
    }

    let evidence = Record::attempt_path(segment.id, number);
    let output = context.record.map_or(Output::Inherit, |record| {
        Output::Directory(record.resolve(&evidence))
    });
    let budget = effective_budget(context.tier, context.started);
    let (outcome, took) = exec::run_segment(segment, context.root, budget, &output)?;
    let status = match outcome {
        exec::Outcome::Pass => AttemptStatus::Pass,
        exec::Outcome::Fail { .. } => AttemptStatus::Fail,
        exec::Outcome::Timeout { .. } => AttemptStatus::Timeout,
    };
    Ok(Entry {
        segment: String::from(segment.id),
        attempt: number,
        started,
        duration_s: took.as_secs_f64(),
        status,
        revision: context.revision.commit.clone(),
        input_digest: context.inputs.digest.clone(),
        evidence: if context.record.is_some() {
            evidence
        } else {
            String::from("none, this tier is not recorded")
        },
        note: outcome.note(),
    })
}

/// The budget one segment gets.
///
/// A tier with a campaign budget spends it across its segments, so the last segment of a
/// campaign that has already used its time gets none and reports a timeout. No segment ever
/// gets more than the segment budget.
fn effective_budget(tier: Tier, started: Instant) -> Duration {
    tier.campaign_budget().map_or(SEGMENT_BUDGET, |campaign| {
        SEGMENT_BUDGET.min(campaign.saturating_sub(started.elapsed()))
    })
}

/// The last attempt at a segment, when it can be reused.
///
/// Only the last attempt counts. A segment that passed and then failed at the same revision
/// runs again, because the failure is the newer fact.
fn reusable<'a>(
    prior: &'a [Entry],
    segment: &str,
    revision: &Revision,
    inputs: &InputSet,
) -> Option<&'a Entry> {
    prior
        .iter()
        .rev()
        .find(|entry| entry.segment == segment)
        .filter(|entry| entry.status.is_satisfied())
        .filter(|entry| entry.revision == revision.commit)
        .filter(|entry| entry.input_digest == inputs.digest)
}

fn next_attempt(prior: &[Entry], segment: &str) -> u32 {
    prior
        .iter()
        .filter(|entry| entry.segment == segment)
        .map(|entry| entry.attempt)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::{Status, effective_budget, next_attempt, reusable};
    use crate::record::{Entry, Record, Status as AttemptStatus};
    use crate::tier::{SEGMENT_BUDGET, Tier};
    use crate::workspace::{InputSet, Revision};
    use std::time::{Duration, Instant};

    const COMMIT: &str = "1a195e3c0ffee00ddeadbeef";
    const DIGEST: &str = "sha256:abc";

    fn revision(commit: &str) -> Revision {
        Revision {
            commit: String::from(commit),
            dirty: false,
        }
    }

    fn inputs(digest: &str) -> InputSet {
        InputSet {
            description: "test",
            file_count: 1,
            byte_count: 1,
            digest: String::from(digest),
        }
    }

    fn entry(
        segment: &str,
        attempt: u32,
        status: AttemptStatus,
        commit: &str,
        digest: &str,
    ) -> Entry {
        Entry {
            segment: String::from(segment),
            attempt,
            started: String::from("2026-09-11T20:00:00Z"),
            duration_s: 1.0,
            status,
            revision: String::from(commit),
            input_digest: String::from(digest),
            evidence: Record::attempt_path(segment, attempt),
            note: None,
        }
    }

    fn reuse(prior: &[Entry]) -> Option<u32> {
        reusable(prior, "workspace-fmt", &revision(COMMIT), &inputs(DIGEST)).map(|e| e.attempt)
    }

    #[test]
    fn a_pass_at_the_current_revision_and_inputs_is_reused() {
        let prior = vec![entry(
            "workspace-fmt",
            1,
            AttemptStatus::Pass,
            COMMIT,
            DIGEST,
        )];
        assert_eq!(reuse(&prior), Some(1));
    }

    #[test]
    fn a_reuse_can_itself_be_reused() {
        let prior = vec![entry(
            "workspace-fmt",
            1,
            AttemptStatus::Cached,
            COMMIT,
            DIGEST,
        )];
        assert_eq!(reuse(&prior), Some(1));
    }

    #[test]
    fn a_pass_at_another_revision_runs_again() {
        let prior = vec![entry(
            "workspace-fmt",
            1,
            AttemptStatus::Pass,
            "other",
            DIGEST,
        )];
        assert_eq!(reuse(&prior), None);
    }

    #[test]
    fn a_pass_against_other_inputs_runs_again() {
        let prior = vec![entry(
            "workspace-fmt",
            1,
            AttemptStatus::Pass,
            COMMIT,
            "sha256:zzz",
        )];
        assert_eq!(reuse(&prior), None);
    }

    #[test]
    fn a_failure_or_a_timeout_runs_again() {
        for status in [AttemptStatus::Fail, AttemptStatus::Timeout] {
            let prior = vec![entry("workspace-fmt", 1, status, COMMIT, DIGEST)];
            assert_eq!(reuse(&prior), None);
        }
    }

    #[test]
    fn a_missing_segment_runs() {
        let prior = vec![entry(
            "workspace-test",
            1,
            AttemptStatus::Pass,
            COMMIT,
            DIGEST,
        )];
        assert_eq!(reuse(&prior), None);
    }

    #[test]
    fn only_the_last_attempt_decides_reuse() {
        let prior = vec![
            entry("workspace-fmt", 1, AttemptStatus::Pass, COMMIT, DIGEST),
            entry("workspace-fmt", 2, AttemptStatus::Fail, COMMIT, DIGEST),
        ];
        assert_eq!(reuse(&prior), None);
    }

    #[test]
    fn a_retry_after_a_failure_is_reused_once_it_passes() {
        let prior = vec![
            entry("workspace-fmt", 1, AttemptStatus::Fail, COMMIT, DIGEST),
            entry("workspace-fmt", 2, AttemptStatus::Pass, COMMIT, DIGEST),
        ];
        assert_eq!(reuse(&prior), Some(2));
    }

    #[test]
    fn the_first_attempt_at_a_segment_is_one() {
        assert_eq!(next_attempt(&[], "workspace-fmt"), 1);
    }

    #[test]
    fn an_attempt_number_follows_the_highest_already_recorded() {
        let prior = vec![
            entry("workspace-fmt", 1, AttemptStatus::Fail, COMMIT, DIGEST),
            entry("workspace-test", 1, AttemptStatus::Pass, COMMIT, DIGEST),
            entry("workspace-fmt", 2, AttemptStatus::Fail, COMMIT, DIGEST),
        ];
        assert_eq!(next_attempt(&prior, "workspace-fmt"), 3);
        assert_eq!(next_attempt(&prior, "workspace-test"), 2);
    }

    #[test]
    fn a_segmented_tier_gives_every_segment_the_full_budget() {
        assert_eq!(effective_budget(Tier::Gate, Instant::now()), SEGMENT_BUDGET);
    }

    #[test]
    fn a_campaign_budget_never_buys_more_than_the_segment_budget() {
        for tier in [Tier::Smoke, Tier::Dev] {
            assert!(effective_budget(tier, Instant::now()) <= SEGMENT_BUDGET);
        }
    }

    #[test]
    fn an_exhausted_campaign_budget_leaves_a_segment_no_time() {
        let long_past = Instant::now().checked_sub(Duration::from_secs(600));
        if let Some(started) = long_past {
            assert!(effective_budget(Tier::Smoke, started).is_zero());
        }
    }

    #[test]
    fn only_a_failure_is_not_a_success() {
        assert!(Status::Sealed.is_success());
        assert!(Status::Passed.is_success());
        assert!(!Status::Failed.is_success());
    }
}
