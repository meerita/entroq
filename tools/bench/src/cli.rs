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

pub const HELP: &str = "\
entroq-bench: the Entroq benchmark tool.

Usage:
  entroq-bench lab build  --codec <name> [--lab <dir>]
  entroq-bench lab verify --codec <name> [--lab <dir>]
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

Competitors:
  lz4, zstd, brotli, snappy, zlib

Environment:
  LAB   the competitor codec workspace, when --lab does not name it.
        Not required. Default: ../lab, beside the repository.

Host tools:
  git, cmake, a C and C++ compiler, date, and uname. A host missing one fails with its name.

Exit status:
  0  the build or the rebuild succeeded, whatever the rebuild measured
  1  the tool could not produce the build or the rebuild
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
        for field in ["lz4", "zstd", "brotli", "snappy", "zlib", "LAB", "../lab"] {
            assert!(HELP.contains(field), "the help is missing `{field}`");
        }
    }
}
