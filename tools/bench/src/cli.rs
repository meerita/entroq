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
use crate::registry::{self, Group, Selection};

pub const HELP: &str = "\
entroq-bench: the Entroq benchmark tool.

Usage:
  entroq-bench lab build      --codec <name> [--lab <dir>]
  entroq-bench lab verify     --codec <name> [--lab <dir>]
  entroq-bench corpus build     <what> [--corpus <dir>]
  entroq-bench corpus reproduce <what> [--corpus <dir>]
  entroq-bench corpus verify    <what> [--corpus <dir>]
  entroq-bench corpus list      <what>
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

Selecting entries, for every corpus action:
  --all             every registered entry
  --group <name>    every entry of one group
  --entry <name>    one entry

Competitors:
  lz4, zstd, brotli, snappy, zlib

Corpus groups:
  project, enwik, sourcecode, gutenberg

Environment:
  LAB      the competitor codec workspace, when --lab does not name it.
           Not required. Default: ../lab, beside the repository.
  CORPUS   the corpus cache, when --corpus does not name it.
           Not required. Default: ../corpus, beside the repository.

Host tools:
  Building the laboratory needs git, cmake, a C and C++ compiler, date, and uname.
  Fetching a corpus entry needs curl, and unzip or gzip for an entry that is archived.
  A host missing one fails with its name.

Exit status:
  0  the action succeeded, whatever a rebuild measured
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
}
