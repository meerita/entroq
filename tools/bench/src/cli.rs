//! Owns the tool's command line: what an invocation may ask for, and what it must state.
//!
//! Every input is explicit. A competitor is named, never inferred from a directory listing,
//! so a segment measures the codec its identifier says it does.
//!
//! This module does not own laboratory behavior. It decides only whether an invocation names
//! work the tool can do.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::catalog::{self, Codec};
use crate::error::{Error, Result};
use crate::plan::{Points, Request, Tier};
use crate::registry::{self, Group, Selection, SizeClass};
use crate::subject::{self, Subject};

pub const HELP: &str = "\
entroq-bench: the Entroq benchmark tool.

Usage:
  entroq-bench lab build      --codec <name> [--lab <dir>]
  entroq-bench lab verify     --codec <name> [--lab <dir>]
  entroq-bench corpus build     <what> [--corpus <dir>]
  entroq-bench corpus reproduce <what> [--corpus <dir>]
  entroq-bench corpus verify    <what> [--corpus <dir>]
  entroq-bench corpus list      <what>
  entroq-bench measure        --tier <tier> --codec <name> [--class <name>]
                              [--points <group>] [--segment <id>] [--lab <dir>]
                              [--corpus <dir>]
  entroq-bench report parse   --results <path> [--results <path>]...
  entroq-bench report pareto  --results <path> [--results <path>]...
  entroq-bench report compare --baseline <path> [--baseline <path>]...
                              --results <path> [--results <path>]...
  entroq-bench help

`lab build` fetches one competitor at its pinned commit, builds it as a static library with
the release configuration its own project recommends, and writes the manifest that says how.
A competitor that is already built is left alone, so an old result keeps pointing at the
build that produced it.

`lab verify` rebuilds one competitor from the commands its manifest records, into a prefix
the build has never written to, and compares the libraries byte for byte. A difference is
recorded, not failed: a compiler that embeds a path or a build identifier produces a library
that behaves the same and does not hash the same. The manifest checksum identifies the
artifact a result came from. It is not a claim that the build is bit reproducible.

This revision measures nothing and links nothing it builds.

`corpus build` materializes every selected registry entry into the cache: a generated entry
from its recorded seed, a fetched entry from its upstream archive. Every archive is checked
against its recorded checksum as it arrives, and every entry against the digest the registry
pins. An entry whose cached bytes already match is left alone.

`corpus reproduce` generates every selected generated entry twice, into two directories
neither generation shares, and fails unless both match each other and the registry. It is
what makes a recorded seed a pin rather than a label.

`corpus verify` checks cached bytes against the registry without obtaining anything.

`corpus list` prints the registry, including the license of every entry, and touches no
cache.

No corpus byte is committed. The registry and the method of obtaining it are.

`measure` measures one codec in-process. A competitor goes through the library the laboratory
built and this binary linked; `entroq` goes through the crate of this workspace, so both sides
of a comparison cross one call and one process and neither is charged for a boundary the other
does not pay. It emits one machine-readable result on standard output, and writes the same
document into the directory the runner names for the segment's evidence. Every metric is
reported as measured, with the call that produced it, or as unavailable, with the reason. No
metric is reported as a zero.

Tiers:
  smoke        tiny and small classes, the default operating point, numbers ignored
  dev          adds the medium class, few samples, direction only, never published
  gate         every pinned operating point, adds the large class, spread reported
  publication  full corpora, the only tier a number may leave the repository from

A measurement covers every size class its tier names, unless --class names one, and every
operating point its tier names, unless --points names one group. A segmented campaign names
one class and one group per segment so each segment fits its budget. The cost of one point
spans three orders of magnitude inside one project, so a slow point is grouped with points of
its own cost rather than with the cheap ones it would push over the budget.

Every result states whether it measured Entroq. A segment that did says so; a segment that
measured a competitor says that the Entroq row belongs to the segment that measures it, and no
zero and no placeholder stands in for a number it did not produce.

A measurement needs the laboratory linked, whichever codec it names. A comparison is what the
harness exists for, and a binary that could measure only one side of one would produce a number
nobody could place.

`report parse` reads every recorded result under the paths given and reports what it read.
A result is read whole or rejected: a document carrying a field the parser does not declare,
or omitting one it does, fails rather than being read in part, because a field skipped is a
metric dropped and a dropped metric reads as an absent one.

`report pareto` reads the same results and marks every operating point another point
dominates. A point is dominated when another point on the same chart is
equal or better in both dimensions and strictly better in one. A point equal in both is not
dominated.

Axes:
  ratio against encode throughput
  ratio against decode throughput
  ratio against encode CPU
  ratio against decode CPU
  ratio against memory, as the bytes a codec's own state holds

One chart holds one tier, one host, one corpus entry, and one thread count, and every plotted
point states its tier. An axis no result carries a number for reports no data with the reason
the results gave, and no other metric stands in its place.

`report compare` reads a first campaign and a second one and states, row by row and metric by
metric, whether the second reproduced the first. A number that is a property of the bytes has
no variance, so any difference in it is a finding. A number that is a timing is judged against
the spread the first record states for the block it came from. A number that moves between
runs and that no record states a variance for is reported with its difference and no verdict.
Two rows are compared only when they measured the same bytes, at the same operating point,
through the same competitor version, under the same integrity setting.

A path that names a directory is searched for every file named `result.json`, so a run record
directory yields the results of every segment in it. Name one campaign: two measurements of
one operating point are two measurements, and a frontier cannot hold one of them twice.

Selecting entries, for every corpus action:
  --all             every registered entry
  --group <name>    every entry of one group
  --entry <name>    one entry

Competitors:
  lz4, zstd, brotli, snappy, zlib

Size classes:
  tiny, small, medium, large, huge

Operating point groups:
  lz4      fast, hc
  zstd     low, high, max
  brotli   low, high, max
  snappy   default
  zlib     low, high

Corpus groups:
  project, enwik, sourcecode, gutenberg

Report inputs:
  --results <path>  a result document, or a directory holding them. Repeatable.
  --baseline <path> the first campaign a comparison reads. Repeatable. compare only.

Environment:
  LAB      the competitor codec workspace, when --lab does not name it.
           Not required. Default: ../lab, beside the repository.
  CORPUS   the corpus cache, when --corpus does not name it.
           Not required. Default: ../corpus, beside the repository.
  ENTROQ_SEGMENT_DIR
           the directory a measurement writes its result document into, which the
           validation runner sets to the segment's evidence directory. Not required.
           Without it the document goes to standard output alone.

Host tools:
  Building the laboratory needs git, cmake, a C and C++ compiler, date, and uname.
  Fetching a corpus entry needs curl, and unzip or gzip for an entry that is archived.
  A host missing one fails with its name.

Exit status:
  0  the action succeeded, whatever a rebuild or a measurement found
  1  the tool could not complete the action
";

/// What the tool was asked to do.
pub enum Invocation {
    Help,
    Build {
        codec: &'static Codec,
        lab: Option<PathBuf>,
    },
    Verify {
        codec: &'static Codec,
        lab: Option<PathBuf>,
    },
    Corpus {
        action: CorpusAction,
        selection: Selection,
        corpus: Option<PathBuf>,
    },
    Measure(Box<Request>),
    Report {
        action: ReportAction,
        results: Vec<PathBuf>,
        /// The first campaign a comparison is read against. Empty for every other action.
        baseline: Vec<PathBuf>,
    },
}

/// What a report invocation does with the results it read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportAction {
    /// Report what was read, and how much of each document was checked.
    Parse,
    /// Report the frontier, and every point another point dominates.
    Pareto,
    /// Report whether a second campaign reproduced the numbers of a first one.
    Compare,
}

impl ReportAction {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "parse" => Some(Self::Parse),
            "pareto" => Some(Self::Pareto),
            "compare" => Some(Self::Compare),
            _ => None,
        }
    }
}

/// What a corpus invocation does to the entries it selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorpusAction {
    Build,
    Reproduce,
    Verify,
    List,
}

impl CorpusAction {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "build" => Some(Self::Build),
            "reproduce" => Some(Self::Reproduce),
            "verify" => Some(Self::Verify),
            "list" => Some(Self::List),
            _ => None,
        }
    }
}

/// Parses an invocation.
///
/// # Errors
///
/// Fails when the arguments do not name work the tool can do.
pub fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Invocation> {
    let Some(verb) = args.next() else {
        return Ok(Invocation::Help);
    };
    match text(&verb, "command")?.as_str() {
        "help" | "--help" | "-h" => Ok(Invocation::Help),
        "lab" => lab(args),
        "corpus" => corpus(args),
        "measure" => measure(args),
        "report" => report(args),
        other => Err(Error::Usage(format!(
            "unknown command `{other}`. Run `entroq-bench help`."
        ))),
    }
}

fn lab(mut args: impl Iterator<Item = OsString>) -> Result<Invocation> {
    let action = args
        .next()
        .ok_or_else(|| Error::Usage(String::from("lab: name an action: build or verify")))?;
    let action = text(&action, "action")?;
    if action != "build" && action != "verify" {
        return Err(Error::Usage(format!(
            "lab: unknown action `{action}`. Use build or verify."
        )));
    }

    let mut name: Option<String> = None;
    let mut lab: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match text(&arg, "argument")?.as_str() {
            "--codec" => name = Some(text(&next(&mut args, "--codec")?, "--codec")?),
            "--lab" => lab = Some(PathBuf::from(next(&mut args, "--lab")?)),
            other => {
                return Err(Error::Usage(format!(
                    "unexpected argument `{other}`. Run `entroq-bench help`."
                )));
            }
        }
    }

    let name = name.ok_or_else(|| Error::Usage(String::from("lab: --codec is required")))?;
    let codec = catalog::find(&name).ok_or_else(|| {
        Error::Usage(format!(
            "`{name}` is not a pinned competitor. The laboratory holds {}.",
            catalog::names()
        ))
    })?;

    Ok(if action == "build" {
        Invocation::Build { codec, lab }
    } else {
        Invocation::Verify { codec, lab }
    })
}

fn corpus(mut args: impl Iterator<Item = OsString>) -> Result<Invocation> {
    let action = args.next().ok_or_else(|| {
        Error::Usage(String::from(
            "corpus: name an action: build, reproduce, verify, or list",
        ))
    })?;
    let action = CorpusAction::parse(&text(&action, "action")?).ok_or_else(|| {
        Error::Usage(String::from(
            "corpus: unknown action. Use build, reproduce, verify, or list.",
        ))
    })?;

    let mut selection: Option<Selection> = None;
    let mut corpus: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        let flag = text(&arg, "argument")?;
        let chosen = match flag.as_str() {
            "--all" => Some(Selection::All),
            "--group" => {
                let name = text(&next(&mut args, "--group")?, "--group")?;
                Some(Selection::Group(Group::parse(&name).ok_or_else(|| {
                    Error::Usage(format!(
                        "`{name}` is not a corpus group. The registry holds {}.",
                        registry::group_names()
                    ))
                })?))
            }
            "--entry" => {
                let name = text(&next(&mut args, "--entry")?, "--entry")?;
                let entry = registry::find(&name).ok_or_else(|| {
                    Error::Usage(format!(
                        "`{name}` is not a registered corpus entry. Run \
                         `entroq-bench corpus list --all`."
                    ))
                })?;
                Some(Selection::Entry(entry.name))
            }
            "--corpus" => {
                corpus = Some(PathBuf::from(next(&mut args, "--corpus")?));
                None
            }
            other => {
                return Err(Error::Usage(format!(
                    "unexpected argument `{other}`. Run `entroq-bench help`."
                )));
            }
        };
        if let Some(chosen) = chosen {
            if selection.is_some() {
                return Err(Error::Usage(String::from(
                    "corpus: name one of --all, --group, or --entry, not two",
                )));
            }
            selection = Some(chosen);
        }
    }

    let selection = selection.ok_or_else(|| {
        Error::Usage(String::from(
            "corpus: select entries with --all, --group <name>, or --entry <name>",
        ))
    })?;
    Ok(Invocation::Corpus {
        action,
        selection,
        corpus,
    })
}

fn report(mut args: impl Iterator<Item = OsString>) -> Result<Invocation> {
    let action = args.next().ok_or_else(|| {
        Error::Usage(String::from(
            "report: name an action: parse, pareto, or compare",
        ))
    })?;
    let action = ReportAction::parse(&text(&action, "action")?).ok_or_else(|| {
        Error::Usage(String::from(
            "report: unknown action. Use parse, pareto, or compare.",
        ))
    })?;

    let mut results: Vec<PathBuf> = Vec::new();
    let mut baseline: Vec<PathBuf> = Vec::new();
    while let Some(arg) = args.next() {
        match text(&arg, "argument")?.as_str() {
            "--results" => results.push(PathBuf::from(next(&mut args, "--results")?)),
            "--baseline" => baseline.push(PathBuf::from(next(&mut args, "--baseline")?)),
            other => {
                return Err(Error::Usage(format!(
                    "unexpected argument `{other}`. Run `entroq-bench help`."
                )));
            }
        }
    }
    if results.is_empty() {
        return Err(Error::Usage(String::from(
            "report: name what to read with --results <path>. A report over every result a \
             host happens to hold is a report nobody chose.",
        )));
    }
    if action == ReportAction::Compare && baseline.is_empty() {
        return Err(Error::Usage(String::from(
            "report compare: name the first campaign with --baseline <path>. A comparison \
             against nothing is not a reproduction.",
        )));
    }
    if action != ReportAction::Compare && !baseline.is_empty() {
        return Err(Error::Usage(String::from(
            "report: --baseline belongs to compare. Every other action reads one campaign.",
        )));
    }
    Ok(Invocation::Report {
        action,
        results,
        baseline,
    })
}

fn measure(mut args: impl Iterator<Item = OsString>) -> Result<Invocation> {
    let mut tier_name: Option<String> = None;
    let mut codec_name: Option<String> = None;
    let mut class_name: Option<String> = None;
    let mut points_name: Option<String> = None;
    let mut segment: Option<String> = None;
    let mut lab: Option<PathBuf> = None;
    let mut corpus: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match text(&arg, "argument")?.as_str() {
            "--tier" => tier_name = Some(text(&next(&mut args, "--tier")?, "--tier")?),
            "--codec" => codec_name = Some(text(&next(&mut args, "--codec")?, "--codec")?),
            "--class" => class_name = Some(text(&next(&mut args, "--class")?, "--class")?),
            "--points" => points_name = Some(text(&next(&mut args, "--points")?, "--points")?),
            "--segment" => segment = Some(text(&next(&mut args, "--segment")?, "--segment")?),
            "--lab" => lab = Some(PathBuf::from(next(&mut args, "--lab")?)),
            "--corpus" => corpus = Some(PathBuf::from(next(&mut args, "--corpus")?)),
            other => {
                return Err(Error::Usage(format!(
                    "unexpected argument `{other}`. Run `entroq-bench help`."
                )));
            }
        }
    }

    let tier_name =
        tier_name.ok_or_else(|| Error::Usage(String::from("measure: --tier is required")))?;
    let tier = Tier::parse(&tier_name).ok_or_else(|| {
        Error::Usage(format!(
            "`{tier_name}` is not a tier. Use smoke, dev, gate, or publication."
        ))
    })?;
    let codec_name =
        codec_name.ok_or_else(|| Error::Usage(String::from("measure: --codec is required")))?;
    let subject = Subject::parse(&codec_name).ok_or_else(|| {
        Error::Usage(format!(
            "`{codec_name}` is not a codec this harness measures. It measures {}.",
            subject::names()
        ))
    })?;
    let class = match class_name {
        Some(name) => Some(SizeClass::parse(&name).ok_or_else(|| {
            Error::Usage(format!(
                "`{name}` is not a size class. The registry holds {}.",
                SizeClass::names()
            ))
        })?),
        None => None,
    };
    if let Some(class) = class
        && !tier.classes().contains(&class)
    {
        return Err(Error::Usage(format!(
            "the {} tier does not cover the {} class",
            tier.name(),
            class.name()
        )));
    }

    let points = match points_name {
        Some(name) => {
            if tier.points() == Points::Default {
                return Err(Error::Usage(format!(
                    "the {} tier measures the point {} defaults to, so it covers no \
                     operating point group",
                    tier.name(),
                    subject.display()
                )));
            }
            Some(subject.group(&name).ok_or_else(|| {
                Error::Usage(format!(
                    "`{name}` is not an operating point group of {}. It groups its points as {}.",
                    subject.display(),
                    subject.group_names()
                ))
            })?)
        }
        None => None,
    };

    let segment = segment.unwrap_or_else(|| {
        let mut id = format!("bench-{}", subject.name());
        if let Some(group) = points {
            id.push('-');
            id.push_str(group.name);
        }
        if let Some(class) = class {
            id.push('-');
            id.push_str(class.name());
        }
        id
    });
    Ok(Invocation::Measure(Box::new(Request {
        tier,
        subject,
        class,
        points,
        segment,
        lab,
        corpus,
    })))
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
    use super::{HELP, Invocation, parse};
    use std::ffi::OsString;

    fn invoke(args: &[&str]) -> super::Result<Invocation> {
        parse(args.iter().map(|a| OsString::from(*a)))
    }

    fn built(args: &[&str]) -> Option<(String, Option<String>)> {
        match invoke(args) {
            Ok(Invocation::Build { codec, lab }) => Some((
                String::from(codec.name),
                lab.map(|p| p.display().to_string()),
            )),
            _ => None,
        }
    }

    #[test]
    fn no_argument_asks_for_help() {
        assert!(matches!(invoke(&[]), Ok(Invocation::Help)));
        assert!(matches!(invoke(&["help"]), Ok(Invocation::Help)));
    }

    #[test]
    fn a_build_names_its_codec() {
        assert_eq!(
            built(&["lab", "build", "--codec", "zstd"]),
            Some((String::from("zstd"), None))
        );
    }

    #[test]
    fn a_build_keeps_the_laboratory_it_was_given() {
        assert_eq!(
            built(&["lab", "build", "--codec", "zstd", "--lab", "/tmp/lab"])
                .and_then(|(_, lab)| lab),
            Some(String::from("/tmp/lab"))
        );
    }

    #[test]
    fn a_rebuild_is_a_different_action_on_the_same_codec() {
        let parsed = invoke(&["lab", "verify", "--codec", "brotli"]);
        assert!(matches!(
            parsed,
            Ok(Invocation::Verify { codec, .. }) if codec.name == "brotli"
        ));
    }

    #[test]
    fn a_codec_nobody_pinned_is_rejected_with_the_set_that_exists() {
        let failure = invoke(&["lab", "build", "--codec", "lzma"]);
        assert!(failure.is_err());
        let message = failure.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("lz4"), "{message}");
        assert!(message.contains("zlib"), "{message}");
    }

    #[test]
    fn a_build_without_a_codec_is_rejected() {
        assert!(invoke(&["lab", "build"]).is_err());
    }

    #[test]
    fn an_unknown_command_or_action_is_rejected() {
        assert!(invoke(&["measure"]).is_err());
        assert!(invoke(&["lab"]).is_err());
        assert!(invoke(&["lab", "destroy", "--codec", "lz4"]).is_err());
    }

    #[test]
    fn a_flag_without_a_value_is_rejected() {
        assert!(invoke(&["lab", "build", "--codec"]).is_err());
        assert!(invoke(&["lab", "build", "--codec", "lz4", "--lab"]).is_err());
    }

    #[test]
    fn an_unexpected_argument_is_rejected() {
        assert!(invoke(&["lab", "build", "--codec", "lz4", "--fast"]).is_err());
    }

    #[test]
    fn the_help_names_every_competitor_and_the_build_input() {
        for field in [
            "lz4",
            "zstd",
            "brotli",
            "snappy",
            "zlib",
            "LAB",
            "../lab",
            "CORPUS",
            "../corpus",
        ] {
            assert!(HELP.contains(field), "the help is missing `{field}`");
        }
    }

    fn corpus(args: &[&str]) -> Option<(super::CorpusAction, String, Option<String>)> {
        match invoke(args) {
            Ok(Invocation::Corpus {
                action,
                selection,
                corpus,
            }) => Some((
                action,
                selection.describe(),
                corpus.map(|p| p.display().to_string()),
            )),
            _ => None,
        }
    }

    #[test]
    fn a_corpus_action_carries_its_selection() {
        assert_eq!(
            corpus(&["corpus", "build", "--group", "project"]).map(|c| (c.0, c.1)),
            Some((
                super::CorpusAction::Build,
                String::from("the project group")
            ))
        );
        assert_eq!(
            corpus(&["corpus", "verify", "--entry", "enwik8"]).map(|c| (c.0, c.1)),
            Some((
                super::CorpusAction::Verify,
                String::from("the entry enwik8")
            ))
        );
        assert_eq!(
            corpus(&["corpus", "list", "--all"]).map(|c| (c.0, c.1)),
            Some((super::CorpusAction::List, String::from("every entry")))
        );
    }

    #[test]
    fn a_corpus_action_keeps_the_cache_it_was_given() {
        assert_eq!(
            corpus(&["corpus", "build", "--all", "--corpus", "/tmp/corpus"]).and_then(|c| c.2),
            Some(String::from("/tmp/corpus"))
        );
    }

    #[test]
    fn a_corpus_action_without_a_selection_is_rejected() {
        assert!(invoke(&["corpus", "build"]).is_err());
        assert!(invoke(&["corpus"]).is_err());
    }

    #[test]
    fn two_selections_are_rejected_rather_than_one_silently_winning() {
        assert!(invoke(&["corpus", "build", "--all", "--group", "project"]).is_err());
        assert!(
            invoke(&[
                "corpus",
                "build",
                "--entry",
                "enwik8",
                "--entry",
                "go-source"
            ])
            .is_err()
        );
    }

    #[test]
    fn a_group_or_an_entry_nobody_registered_is_rejected_with_what_exists() {
        let failure = invoke(&["corpus", "build", "--group", "silesia"]);
        let message = failure.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("project"), "{message}");
        assert!(invoke(&["corpus", "build", "--entry", "silesia"]).is_err());
    }

    #[test]
    fn an_unknown_corpus_action_is_rejected() {
        assert!(invoke(&["corpus", "delete", "--all"]).is_err());
    }

    fn measured(args: &[&str]) -> Option<super::Request> {
        match invoke(args) {
            Ok(Invocation::Measure(request)) => Some(*request),
            _ => None,
        }
    }

    #[test]
    fn a_measurement_names_its_tier_and_its_competitor() {
        let request = measured(&["measure", "--tier", "dev", "--codec", "zstd"]);
        assert_eq!(
            request.map(|r| (r.tier.name(), r.subject.name(), r.segment)),
            Some(("dev", "zstd", String::from("bench-zstd")))
        );
    }

    #[test]
    fn a_measurement_of_one_class_names_it_in_its_segment() {
        let request = measured(&[
            "measure", "--tier", "gate", "--codec", "brotli", "--class", "large",
        ]);
        assert_eq!(
            request.map(|r| r.segment),
            Some(String::from("bench-brotli-large"))
        );
    }

    #[test]
    fn a_measurement_of_one_point_group_names_it_in_its_segment() {
        let request = measured(&[
            "measure", "--tier", "gate", "--codec", "brotli", "--points", "max", "--class", "large",
        ]);
        assert_eq!(
            request.map(|r| (r.segment, r.points.map(|g| g.points))),
            Some((String::from("bench-brotli-max-large"), Some(&["q11"][..])))
        );
    }

    #[test]
    fn a_point_group_the_competitor_does_not_have_is_rejected_with_the_ones_it_does() {
        let failure = invoke(&[
            "measure", "--tier", "gate", "--codec", "zstd", "--points", "hc",
        ]);
        assert!(failure.is_err());
        let message = failure.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("low"), "{message}");
        assert!(message.contains("max"), "{message}");
    }

    #[test]
    fn a_tier_that_measures_one_default_point_covers_no_group() {
        for tier in ["smoke", "dev"] {
            let failure = invoke(&[
                "measure", "--tier", tier, "--codec", "zstd", "--points", "low",
            ]);
            assert!(failure.is_err(), "the {tier} tier accepted a point group");
        }
    }

    #[test]
    fn a_named_segment_wins_over_the_derived_one() {
        let request = measured(&[
            "measure",
            "--tier",
            "smoke",
            "--codec",
            "lz4",
            "--segment",
            "bench-probe",
        ]);
        assert_eq!(
            request.map(|r| r.segment),
            Some(String::from("bench-probe"))
        );
    }

    #[test]
    fn a_measurement_keeps_the_build_inputs_it_was_given() {
        let request = measured(&[
            "measure",
            "--tier",
            "smoke",
            "--codec",
            "lz4",
            "--lab",
            "/tmp/lab",
            "--corpus",
            "/tmp/corpus",
        ]);
        let paths = request.map(|r| {
            (
                r.lab.map(|p| p.display().to_string()),
                r.corpus.map(|p| p.display().to_string()),
            )
        });
        assert_eq!(
            paths,
            Some((
                Some(String::from("/tmp/lab")),
                Some(String::from("/tmp/corpus"))
            ))
        );
    }

    #[test]
    fn a_measurement_without_a_tier_or_a_codec_is_rejected() {
        assert!(invoke(&["measure", "--codec", "zstd"]).is_err());
        assert!(invoke(&["measure", "--tier", "dev"]).is_err());
        assert!(invoke(&["measure"]).is_err());
    }

    #[test]
    fn a_tier_or_a_class_nobody_defined_is_rejected_with_what_exists() {
        let failure = invoke(&["measure", "--tier", "quick", "--codec", "zstd"]);
        let message = failure.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("publication"), "{message}");
        assert!(
            invoke(&[
                "measure", "--tier", "dev", "--codec", "zstd", "--class", "enormous"
            ])
            .is_err()
        );
    }

    #[test]
    fn a_class_the_tier_does_not_cover_is_rejected_rather_than_measured_empty() {
        let failure = invoke(&[
            "measure", "--tier", "smoke", "--codec", "zstd", "--class", "large",
        ]);
        assert!(failure.is_err());
        let message = failure.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("does not cover"), "{message}");
    }

    fn report(args: &[&str]) -> Option<(super::ReportAction, Vec<String>)> {
        match invoke(args) {
            Ok(Invocation::Report {
                action, results, ..
            }) => Some((
                action,
                results
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
            )),
            _ => None,
        }
    }

    fn baseline(args: &[&str]) -> Option<Vec<String>> {
        match invoke(args) {
            Ok(Invocation::Report { baseline, .. }) => Some(
                baseline
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
            ),
            _ => None,
        }
    }

    #[test]
    fn a_report_names_its_action_and_every_result_it_reads() {
        assert_eq!(
            report(&["report", "parse", "--results", "/tmp/a"]),
            Some((super::ReportAction::Parse, vec![String::from("/tmp/a")]))
        );
        assert_eq!(
            report(&[
                "report",
                "pareto",
                "--results",
                "/tmp/a",
                "--results",
                "/tmp/b"
            ]),
            Some((
                super::ReportAction::Pareto,
                vec![String::from("/tmp/a"), String::from("/tmp/b")]
            ))
        );
    }

    #[test]
    fn a_comparison_names_the_campaign_it_reads_the_second_one_against() {
        assert_eq!(
            baseline(&[
                "report",
                "compare",
                "--baseline",
                "/tmp/first",
                "--results",
                "/tmp/second"
            ]),
            Some(vec![String::from("/tmp/first")])
        );
    }

    #[test]
    fn a_comparison_against_nothing_is_rejected() {
        let failure = invoke(&["report", "compare", "--results", "/tmp/second"]);
        let message = failure.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("--baseline"), "{message}");
    }

    #[test]
    fn a_baseline_given_to_an_action_that_reads_one_campaign_is_rejected() {
        let failure = invoke(&[
            "report",
            "pareto",
            "--baseline",
            "/tmp/first",
            "--results",
            "/tmp/second",
        ]);
        let message = failure.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("belongs to compare"), "{message}");
    }

    #[test]
    fn a_report_over_nothing_in_particular_is_rejected() {
        let failure = invoke(&["report", "pareto"]);
        let message = failure.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("--results"), "{message}");
        assert!(invoke(&["report"]).is_err());
        assert!(invoke(&["report", "plot", "--results", "/tmp/a"]).is_err());
    }

    #[test]
    fn the_help_states_the_five_axes_and_what_dominated_means() {
        for field in [
            "ratio against encode throughput",
            "ratio against decode throughput",
            "ratio against encode CPU",
            "ratio against decode CPU",
            "ratio against memory",
            "equal or better in both dimensions",
            "--results",
        ] {
            assert!(HELP.contains(field), "the help is missing `{field}`");
        }
    }

    #[test]
    fn the_help_states_the_segment_directory_and_how_each_side_is_driven() {
        assert!(HELP.contains("ENTROQ_SEGMENT_DIR"));
        assert!(HELP.contains("crate of this workspace"));
        assert!(HELP.contains("whether it measured Entroq"));
    }
}
