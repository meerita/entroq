//! Owns what a campaign reports: the block it prints when it ends, and the summary a sealed
//! record keeps.
//!
//! Every report names the tier and the record. A result that names neither is not evidence,
//! and this module never produces one.

use crate::campaign::Outcome;
use crate::fuzz::Outcome as FuzzOutcome;
use crate::record::Entry;
use crate::workspace::{Environment, InputSet};

/// Everything a sealed record's summary states.
pub struct Summary<'a> {
    pub outcome: &'a Outcome,
    pub inputs: &'a InputSet,
    pub environment: &'a Environment,
    pub entries: &'a [Entry],
    pub coverage: &'a str,
    pub limits: &'a str,
}

/// The block a campaign prints when it ends.
pub fn console(outcome: &Outcome) -> String {
    let campaign = outcome.campaign.as_deref().map_or_else(
        || String::from("none, this tier is not recorded"),
        String::from,
    );
    let record = outcome.record.as_ref().map_or_else(
        || String::from("none, this tier is not recorded"),
        |p| p.display().to_string(),
    );
    let blockers = if outcome.blockers.is_empty() {
        String::from("none")
    } else {
        outcome.blockers.join(", ")
    };
    format!(
        "Campaign: {campaign}\n\
         Tier: {tier}\n\
         Suite: {suite}\n\
         Revision: {revision}\n\
         Segments: {satisfied}/{total}, {reused} reused\n\
         Duration: {duration}\n\
         Status: {status}\n\
         Record: {record}\n\
         Blockers: {blockers}\n",
        tier = outcome.tier.name(),
        suite = outcome.suite.name(),
        revision = outcome.revision,
        satisfied = outcome.satisfied,
        total = outcome.total,
        reused = outcome.reused,
        duration = seconds(outcome.duration.as_secs_f64()),
        status = outcome.status.as_str(),
    )
}

/// The block one fuzz segment prints when it ends.
///
/// It carries the campaign block every recorded run carries, and three lines a fuzz reader
/// needs on top: where the corpus is and how it grew, whether the segment left a reproducer,
/// and how much time the target has accumulated across every segment so far.
pub fn fuzz_console(outcome: &FuzzOutcome) -> String {
    format!(
        "Campaign: {campaign}\n\
         Tier: dev\n\
         Suite: fuzz\n\
         Target: {target}\n\
         Revision: {revision}\n\
         Segments: {satisfied}/1, 0 reused\n\
         Duration: {duration}\n\
         Status: {status}\n\
         Record: {record}\n\
         Blockers: {blockers}\n\
         Corpus: {corpus}, {after}, {added} added\n\
         Reproducers: {artifacts}\n\
         Accumulated: {accumulated} over {segments}\n",
        campaign = outcome.campaign,
        target = outcome.target,
        revision = outcome.revision,
        satisfied = usize::from(outcome.attempt_status.is_satisfied()),
        duration = seconds(outcome.duration_s),
        status = outcome.status.as_str(),
        record = outcome.record.display(),
        blockers = blocker(outcome),
        corpus = outcome.corpus.display(),
        after = plural(outcome.files_after, "file"),
        added = outcome.files_after.saturating_sub(outcome.files_before),
        artifacts = reproducers(&outcome.artifacts),
        accumulated = seconds(outcome.seconds_total),
        segments = plural(outcome.segments_total, "segment"),
    )
}

/// The summary a fuzz segment writes when it seals.
pub fn fuzz_summary(
    outcome: &FuzzOutcome,
    inputs: &InputSet,
    environment: &Environment,
    coverage: &str,
    limits: &str,
) -> String {
    let lines = vec![
        format!("# {}", outcome.campaign),
        String::new(),
        String::from("Tier: dev"),
        String::from("Suite: fuzz"),
        format!("Status: {}", outcome.status.as_str()),
        format!("Revision: {}", outcome.revision),
        format!("Target: {}", outcome.target),
        format!(
            "Inputs: {} files, {} bytes, {}",
            inputs.file_count, inputs.byte_count, inputs.digest
        ),
        String::from("Segments: 1/1 satisfied, 0 reused"),
        format!("Duration: {}", seconds(outcome.duration_s)),
        format!("libFuzzer budget: {} s", outcome.fuzz_seconds),
        format!(
            "Host: {} {}, {}",
            environment.os, environment.arch, environment.host
        ),
        format!("Toolchain: {}, {}", environment.rustc, environment.cargo),
        String::new(),
        String::from("## Corpus"),
        String::new(),
        format!("Directory: `{}`", outcome.corpus.display()),
        format!(
            "Inputs: {} before, {} after, {} added",
            outcome.files_before,
            outcome.files_after,
            outcome.files_after.saturating_sub(outcome.files_before)
        ),
        format!("Reproducers: {}", reproducers(&outcome.artifacts)),
        String::new(),
        String::from(
            "The corpus persists between invocations, outside the repository. Do not delete \
             it to start clean. It is coverage that many segments paid for.",
        ),
        String::new(),
        String::from("## Accumulated"),
        String::new(),
        format!(
            "{} over {}, including this one.",
            seconds(outcome.seconds_total),
            plural(outcome.segments_total, "segment")
        ),
        String::new(),
        String::from(
            "Each figure is the wall clock of one recorded segment. The driver is built \
             before the segment starts, so the segment measures fuzzing rather than a \
             compiler.",
        ),
        String::new(),
        String::from("## Segments"),
        String::new(),
        String::from("| segment | status | attempt | duration | evidence |"),
        String::from("|---|---|---|---|---|"),
        format!(
            "| {} | {} | 01 | {} | `{}` |",
            outcome.segment,
            outcome.attempt_status,
            seconds(outcome.duration_s),
            crate::record::Record::attempt_path(&outcome.segment, 1)
        ),
        String::new(),
        String::from("## Limits"),
        String::new(),
        String::from(coverage),
        String::new(),
        String::from(limits),
        String::new(),
    ];
    lines.join("\n")
}

/// The exact segment that stopped a fuzz campaign, and what its step reported.
fn blocker(outcome: &FuzzOutcome) -> String {
    if outcome.attempt_status.is_satisfied() {
        return String::from("none");
    }
    outcome.note.as_ref().map_or_else(
        || outcome.segment.clone(),
        |note| format!("{}, {note}", outcome.segment),
    )
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn reproducers(artifacts: &[String]) -> String {
    if artifacts.is_empty() {
        String::from("none")
    } else {
        artifacts.join(", ")
    }
}

/// The one line a campaign prints as each segment ends.
pub fn segment_line(segment: &str, status: &str, seconds_taken: f64, note: Option<&str>) -> String {
    let note = note.map_or_else(String::new, |note| format!("  {note}"));
    format!(
        "  {segment:<20} {status:<8} {:>8}{note}",
        seconds(seconds_taken)
    )
}

/// The summary a campaign writes when it seals.
pub fn summary(summary: &Summary<'_>) -> String {
    let outcome = summary.outcome;
    let mut lines = vec![
        format!("# {}", outcome.campaign.as_deref().unwrap_or("unnamed")),
        String::new(),
        format!("Tier: {}", outcome.tier.name()),
        format!("Suite: {}", outcome.suite.name()),
        format!("Status: {}", outcome.status.as_str()),
        format!("Revision: {}", outcome.revision),
        format!(
            "Inputs: {} files, {} bytes, {}",
            summary.inputs.file_count, summary.inputs.byte_count, summary.inputs.digest
        ),
        format!(
            "Segments: {}/{} satisfied, {} reused",
            outcome.satisfied, outcome.total, outcome.reused
        ),
        format!("Duration: {}", seconds(outcome.duration.as_secs_f64())),
        format!(
            "Host: {} {}, {}",
            summary.environment.os, summary.environment.arch, summary.environment.host
        ),
        format!(
            "Toolchain: {}, {}",
            summary.environment.rustc, summary.environment.cargo
        ),
        String::new(),
        String::from("## Segments"),
        String::new(),
        String::from("| segment | status | attempt | duration | evidence |"),
        String::from("|---|---|---|---|---|"),
    ];
    lines.extend(summary.entries.iter().map(|entry| {
        format!(
            "| {} | {} | {:02} | {} | `{}` |",
            entry.segment,
            entry.status,
            entry.attempt,
            seconds(entry.duration_s),
            entry.evidence
        )
    }));
    lines.extend([
        String::new(),
        String::from("## Limits"),
        String::new(),
        String::from(summary.coverage),
        String::new(),
        String::from(summary.limits),
        String::new(),
    ]);
    lines.join("\n")
}

fn seconds(value: f64) -> String {
    format!("{value:.1} s")
}

#[cfg(test)]
mod tests {
    use super::{console, seconds, segment_line};
    use crate::campaign::{Outcome, Status};
    use crate::tier::{Suite, Tier};
    use std::path::PathBuf;
    use std::time::Duration;

    fn outcome() -> Outcome {
        Outcome {
            tier: Tier::Gate,
            suite: Suite::Workspace,
            campaign: Some(String::from("2026-09-11-01-workspace-gate")),
            revision: String::from("1a195e3"),
            total: 5,
            satisfied: 5,
            reused: 3,
            duration: Duration::from_millis(12_400),
            status: Status::Sealed,
            record: Some(PathBuf::from("../runs/gate/2026-09-11-01-workspace-gate")),
            blockers: Vec::new(),
        }
    }

    #[test]
    fn a_report_states_every_field_a_reader_needs() {
        let text = console(&outcome());
        for field in [
            "Campaign: 2026-09-11-01-workspace-gate",
            "Tier: gate",
            "Suite: workspace",
            "Revision: 1a195e3",
            "Segments: 5/5, 3 reused",
            "Duration: 12.4 s",
            "Status: sealed",
            "Record: ../runs/gate/2026-09-11-01-workspace-gate",
            "Blockers: none",
        ] {
            assert!(text.contains(field), "report is missing `{field}`:\n{text}");
        }
    }

    #[test]
    fn an_unrecorded_report_says_so_rather_than_naming_a_path() {
        let mut outcome = outcome();
        outcome.tier = Tier::Smoke;
        outcome.campaign = None;
        outcome.record = None;
        outcome.status = Status::Passed;
        let text = console(&outcome);
        assert!(text.contains("Record: none, this tier is not recorded"));
        assert!(!text.contains("../runs"));
    }

    #[test]
    fn a_failed_report_names_the_exact_segment() {
        let mut outcome = outcome();
        outcome.status = Status::Failed;
        outcome.satisfied = 4;
        outcome.blockers = vec![String::from("workspace-test")];
        let text = console(&outcome);
        assert!(text.contains("Status: failed"));
        assert!(text.contains("Blockers: workspace-test"));
    }

    #[test]
    fn a_segment_line_names_its_segment_and_status() {
        let line = segment_line("workspace-fmt", "pass", 0.34, None);
        assert!(line.contains("workspace-fmt"));
        assert!(line.contains("pass"));
        assert!(line.contains("0.3 s"));
    }

    #[test]
    fn a_segment_line_carries_its_note_when_it_has_one() {
        let line = segment_line(
            "workspace-test",
            "timeout",
            120.0,
            Some("exceeded the budget"),
        );
        assert!(line.contains("exceeded the budget"));
    }

    #[test]
    fn a_count_of_one_reads_in_the_singular() {
        assert_eq!(super::plural(1, "segment"), "1 segment");
        assert_eq!(super::plural(0, "segment"), "0 segments");
        assert_eq!(super::plural(2, "segment"), "2 segments");
    }

    #[test]
    fn a_duration_reads_in_seconds() {
        assert_eq!(seconds(0.0), "0.0 s");
        assert_eq!(seconds(12.44), "12.4 s");
    }
}
