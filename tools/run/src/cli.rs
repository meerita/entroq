//! Owns the runner's command line: what an invocation may ask for, and what it must state.
//!
//! Every input is explicit. A recorded tier must name its run root, because a record that
//! lands somewhere the caller did not choose is evidence nobody can find.
//!
//! This module does not own tier behavior. It decides only whether an invocation names a
//! campaign the runner can run.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::tier::{Suite, Tier};

pub const HELP: &str = "\
entroq-run: the Entroq validation runner.

Usage:
  entroq-run run    --tier <tier> [--suite <suite>] [--runs <dir>] [--topic <name>]
  entroq-run resume --tier <tier> [--suite <suite>]  --runs <dir>
  entroq-run fuzz   --target <name> --runs <dir>
  entroq-run help

Tiers:
  smoke        30 s, 1 MiB inputs, not recorded
  dev          120 s, 10 MiB inputs, one segment, recorded
  gate         100 MiB inputs, segmented and resumable, recorded
  publication  full corpora, segmented and authorized, recorded

Suites:
  workspace    the gates every revision must pass. The default.
  lab          the competitor laboratory: one pinned build per competitor. Segmented, so
               it runs at a segmented tier only.
  corpus       the corpus registry: the project corpus, generated from recorded seeds, and
               every registered public corpus, fetched and checksummed. Segmented, so it
               runs at a segmented tier only.
  bench        the benchmark: one segment per competitor, measured in-process through the
               library the laboratory built. A segmented tier splits by size class too.

A tier says how a campaign is bounded, recorded, and resumed. A suite says which segments
it runs.

`run` starts a campaign. `resume` continues the most recent campaign of the same suite at a
segmented tier, re-running only the segments that did not pass at the current revision
against the current inputs.

No segment runs longer than 120 seconds. A segment that overruns is reported as a timeout,
and the budget does not move to accommodate it.

--runs is required for every recorded tier. Its record lands in
<runs>/<tier>/<date>-<NN>-<topic>/ and never inside the repository.

Every segment of a recorded campaign is given ENTROQ_SEGMENT_DIR, the directory its raw
output and any artifact it produces belong in. A benchmark segment writes its result document
there. No segment writes inside the repository.

Environment:
  CARGO    the cargo binary a segment step invokes. Not required. Default: cargo.
  LAB      the competitor codec workspace a lab segment builds into, and a benchmark
           segment measures from. Not required. Default: ../lab.
  CORPUS   the corpus cache a corpus segment materializes into, and a benchmark segment
           reads. Not required. Default: ../corpus.

Exit status:
  0  the campaign passed, and a recorded campaign sealed
  1  a segment failed or overran its budget
  2  the runner could not run the campaign
";

/// Whether a campaign starts fresh or continues the most recent one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Fresh,
    Resume,
}

/// A campaign the runner was asked to run.
pub struct Request {
    pub tier: Tier,
    pub suite: Suite,
    pub runs: Option<PathBuf>,
    pub topic: String,
}

pub enum Invocation {
    Help,
    Campaign {
        request: Request,
        mode: Mode,
    },
    /// The runner recognizes the request, and this revision cannot serve it.
    Unavailable(String),
}

/// Parses an invocation.
///
/// # Errors
///
/// Fails when the arguments do not name a campaign the runner can run.
pub fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Invocation> {
    let Some(verb) = args.next() else {
        return Ok(Invocation::Help);
    };
    match text(&verb, "command")?.as_str() {
        "help" | "--help" | "-h" => Ok(Invocation::Help),
        "run" => campaign(args, Mode::Fresh),
        "resume" => campaign(args, Mode::Resume),
        "fuzz" => Ok(Invocation::Unavailable(String::from(
            "fuzz: no fuzz target exists at this revision, so there is nothing to advance",
        ))),
        other => Err(Error::Usage(format!(
            "unknown command `{other}`. Run `entroq-run help`."
        ))),
    }
}

fn campaign(mut args: impl Iterator<Item = OsString>, mode: Mode) -> Result<Invocation> {
    let mut tier_name: Option<String> = None;
    let mut suite_name: Option<String> = None;
    let mut runs: Option<PathBuf> = None;
    let mut topic: Option<String> = None;

    while let Some(arg) = args.next() {
        match text(&arg, "argument")?.as_str() {
            "--tier" => tier_name = Some(text(&next(&mut args, "--tier")?, "--tier")?),
            "--suite" => suite_name = Some(text(&next(&mut args, "--suite")?, "--suite")?),
            "--runs" => runs = Some(PathBuf::from(next(&mut args, "--runs")?)),
            "--topic" => topic = Some(text(&next(&mut args, "--topic")?, "--topic")?),
            other => {
                return Err(Error::Usage(format!(
                    "unexpected argument `{other}`. Run `entroq-run help`."
                )));
            }
        }
    }

    let tier_name = tier_name.ok_or_else(|| Error::Usage(String::from("--tier is required")))?;
    let tier = Tier::parse(&tier_name).ok_or_else(|| {
        Error::Usage(format!(
            "unknown tier `{tier_name}`. Run `entroq-run help`."
        ))
    })?;
    let suite = match suite_name {
        Some(name) => Suite::parse(&name).ok_or_else(|| {
            Error::Usage(format!("unknown suite `{name}`. Run `entroq-run help`."))
        })?,
        None => Suite::Workspace,
    };

    if suite.segments(tier).is_empty() {
        return Err(Error::Usage(format!(
            "the {} suite has no segments at the {} tier, so there is nothing to run",
            suite.name(),
            tier.name()
        )));
    }
    if tier.is_recorded() && runs.is_none() {
        return Err(Error::Usage(format!(
            "the {} tier is recorded, so --runs is required",
            tier.name()
        )));
    }
    if !tier.is_recorded() && runs.is_some() {
        return Err(Error::Usage(format!(
            "the {} tier is not recorded, so --runs has nothing to write",
            tier.name()
        )));
    }
    if mode == Mode::Resume && !tier.is_resumable() {
        return Err(Error::Usage(format!(
            "the {} tier runs as one campaign and is not resumable",
            tier.name()
        )));
    }

    let topic = topic.unwrap_or_else(|| String::from(suite.default_topic()));
    validate_topic(&topic)?;
    Ok(Invocation::Campaign {
        request: Request {
            tier,
            suite,
            runs,
            topic,
        },
        mode,
    })
}

/// A topic names a directory, so it stays a plain lowercase identifier.
fn validate_topic(topic: &str) -> Result<()> {
    let shaped = !topic.is_empty()
        && topic
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !topic.starts_with('-')
        && !topic.ends_with('-');
    if shaped {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "`{topic}` is not a topic. Use lowercase letters, digits, and inner hyphens."
        )))
    }
}

fn next(args: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<OsString> {
    args.next()
        .ok_or_else(|| Error::Usage(format!("{flag} needs a value")))
}

fn text(value: &OsString, what: &str) -> Result<String> {
    value
        .clone()
        .into_string()
        .map_err(|_| Error::Usage(format!("{what} is not valid UTF-8")))
}

#[cfg(test)]
mod tests {
    use super::{Invocation, Mode, parse};
    use crate::tier::{Suite, Tier};
    use std::ffi::OsString;

    fn invoke(args: &[&str]) -> super::Result<Invocation> {
        parse(args.iter().map(|a| OsString::from(*a)))
    }

    fn campaign(args: &[&str]) -> Option<(Tier, Mode, Option<String>, String)> {
        match invoke(args) {
            Ok(Invocation::Campaign { request, mode }) => Some((
                request.tier,
                mode,
                request.runs.map(|p| p.display().to_string()),
                request.topic,
            )),
            _ => None,
        }
    }

    fn suite(args: &[&str]) -> Option<(Suite, String)> {
        match invoke(args) {
            Ok(Invocation::Campaign { request, .. }) => Some((request.suite, request.topic)),
            _ => None,
        }
    }

    #[test]
    fn no_argument_asks_for_help() {
        assert!(matches!(invoke(&[]), Ok(Invocation::Help)));
        assert!(matches!(invoke(&["help"]), Ok(Invocation::Help)));
    }

    #[test]
    fn smoke_runs_without_a_run_root() {
        let parsed = campaign(&["run", "--tier", "smoke"]);
        assert_eq!(
            parsed.map(|(t, m, r, _)| (t, m, r)),
            Some((Tier::Smoke, Mode::Fresh, None))
        );
    }

    #[test]
    fn smoke_refuses_a_run_root_it_would_not_write() {
        assert!(invoke(&["run", "--tier", "smoke", "--runs", "../runs"]).is_err());
    }

    #[test]
    fn a_recorded_tier_requires_a_run_root() {
        assert!(invoke(&["run", "--tier", "dev"]).is_err());
        assert!(invoke(&["run", "--tier", "gate"]).is_err());
    }

    #[test]
    fn a_recorded_tier_keeps_the_run_root_it_was_given() {
        let parsed = campaign(&["run", "--tier", "dev", "--runs", "../runs"]);
        assert_eq!(
            parsed.map(|(_, _, r, _)| r),
            Some(Some(String::from("../runs")))
        );
    }

    #[test]
    fn the_topic_defaults_and_can_be_named() {
        let default = campaign(&["run", "--tier", "smoke"]).map(|(_, _, _, t)| t);
        assert_eq!(default.as_deref(), Some("workspace-gate"));
        let named = campaign(&["run", "--tier", "smoke", "--topic", "timeout-proof"]);
        assert_eq!(
            named.map(|(_, _, _, t)| t).as_deref(),
            Some("timeout-proof")
        );
    }

    #[test]
    fn a_topic_that_is_not_a_directory_name_is_rejected() {
        assert!(invoke(&["run", "--tier", "smoke", "--topic", "a/b"]).is_err());
        assert!(invoke(&["run", "--tier", "smoke", "--topic", ""]).is_err());
        assert!(invoke(&["run", "--tier", "smoke", "--topic", "-lead"]).is_err());
        assert!(invoke(&["run", "--tier", "smoke", "--topic", "Upper"]).is_err());
    }

    #[test]
    fn only_a_segmented_tier_resumes() {
        let gate = campaign(&["resume", "--tier", "gate", "--runs", "../runs"]);
        assert_eq!(
            gate.map(|(t, m, _, _)| (t, m)),
            Some((Tier::Gate, Mode::Resume))
        );
        assert!(invoke(&["resume", "--tier", "dev", "--runs", "../runs"]).is_err());
        assert!(invoke(&["resume", "--tier", "smoke"]).is_err());
    }

    #[test]
    fn a_campaign_runs_the_workspace_suite_when_none_is_named() {
        assert_eq!(
            suite(&["run", "--tier", "smoke"]),
            Some((Suite::Workspace, String::from("workspace-gate")))
        );
    }

    #[test]
    fn a_named_suite_brings_its_own_topic() {
        assert_eq!(
            suite(&[
                "run", "--tier", "gate", "--suite", "lab", "--runs", "../runs"
            ]),
            Some((Suite::Lab, String::from("lab-build")))
        );
    }

    #[test]
    fn a_named_topic_still_wins_over_the_suite_default() {
        let named = suite(&[
            "run", "--tier", "gate", "--suite", "lab", "--runs", "../runs", "--topic", "relab",
        ]);
        assert_eq!(named.map(|(_, topic)| topic).as_deref(), Some("relab"));
    }

    #[test]
    fn a_suite_with_no_segments_at_a_tier_is_rejected_rather_than_run_empty() {
        let failure = invoke(&[
            "run", "--tier", "dev", "--suite", "lab", "--runs", "../runs",
        ]);
        assert!(failure.is_err());
        let message = failure.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("no segments"), "{message}");
    }

    #[test]
    fn an_unknown_suite_is_rejected() {
        assert!(
            invoke(&[
                "run", "--tier", "gate", "--suite", "codecs", "--runs", "../runs"
            ])
            .is_err()
        );
    }

    #[test]
    fn an_unknown_tier_or_command_is_rejected() {
        assert!(invoke(&["run", "--tier", "quick", "--runs", "../runs"]).is_err());
        assert!(invoke(&["sprint", "--tier", "dev"]).is_err());
    }

    #[test]
    fn a_flag_without_a_value_is_rejected() {
        assert!(invoke(&["run", "--tier"]).is_err());
        assert!(invoke(&["run", "--tier", "dev", "--runs"]).is_err());
    }

    #[test]
    fn an_unexpected_argument_is_rejected() {
        assert!(invoke(&["run", "--tier", "dev", "--runs", "../runs", "--fast"]).is_err());
    }

    #[test]
    fn a_capability_this_revision_lacks_is_named_not_guessed() {
        assert!(matches!(invoke(&["fuzz"]), Ok(Invocation::Unavailable(_))));
    }

    #[test]
    fn the_benchmark_runs_as_a_suite_at_every_tier() {
        assert_eq!(
            suite(&["run", "--tier", "smoke", "--suite", "bench"]),
            Some((Suite::Bench, String::from("bench-baseline")))
        );
        assert!(
            invoke(&[
                "run", "--tier", "gate", "--suite", "bench", "--runs", "../runs"
            ])
            .is_ok()
        );
    }
}
