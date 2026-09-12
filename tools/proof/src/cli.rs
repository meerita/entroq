//! Owns the tool's command line: what an invocation may ask for, and what it must state.
//!
//! Every input is explicit. A directory is named, never inferred, so a lane compares the
//! vectors it was pointed at rather than whatever a working directory happened to hold.
//!
//! This module does not own what a proof measures. It decides only whether an invocation
//! names work the tool can do.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::curve;
use crate::error::{Error, Result};

pub const HELP: &str = "\
entroq-proof: the Entroq skeleton proofs.

Usage:
  entroq-proof curve       [--sizes <bytes>[,<bytes>]...]
  entroq-proof curve-point --bytes <count>
  entroq-proof vectors     --out <dir>
  entroq-proof cross       --mine <dir> --theirs <dir>
  entroq-proof help

`curve` measures what the streaming pair holds as the logical input grows from one mebibyte
to one gibibyte. It runs one child process per size, because the resident-set high-water mark
is process wide and monotonic, and a point measured beside a larger one would report the
larger one's figure. It prints the curve as a table, states the criterion it judged the curve
against, and exits non-zero when the curve does not hold it.

`curve-point` is one point of that curve, in its own process. It prints one line of fields and
nothing else. `curve` runs it; a person reads `curve`.

`vectors` writes the format vector catalog into a directory. The catalog is code, so two lanes
running the same revision write the same set without exchanging a description of it. Only the
streams travel between lanes.

`cross` reads the vectors two lanes wrote, compares every stream byte for byte, and decodes
each lane's output against the content the catalog describes. It exits non-zero when a stream
differs or does not decode.

Byte order is architectural, not microarchitectural, so an emulated lane settles what `cross`
asks. No timing is read here, and no performance claim is made from a lane.

A recorded run sets ENTROQ_SEGMENT_DIR, and every command writes its report there as well as
to standard output. A run that finds nothing set writes only to standard output.
";

/// What the invocation asked for.
pub enum Invocation {
    Help,
    Curve { sizes: Vec<u64> },
    CurvePoint { bytes: u64 },
    Vectors { out: PathBuf },
    Cross { mine: PathBuf, theirs: PathBuf },
}

/// Reads the invocation.
///
/// # Errors
///
/// Fails when the arguments do not name work the tool can do.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Invocation> {
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let (command, rest) = match args.split_first() {
        Some((command, rest)) => (command.as_str(), rest),
        None => return Ok(Invocation::Help),
    };
    let given = options(rest)?;

    match command {
        "help" | "--help" | "-h" => Ok(Invocation::Help),
        "curve" => {
            only(&given, &["--sizes"])?;
            Ok(Invocation::Curve {
                sizes: match value(&given, "--sizes") {
                    Some(list) => sizes(list)?,
                    None => Vec::from(curve::SIZES),
                },
            })
        }
        "curve-point" => {
            only(&given, &["--bytes"])?;
            Ok(Invocation::CurvePoint {
                bytes: count(required(&given, "--bytes")?)?,
            })
        }
        "vectors" => {
            only(&given, &["--out"])?;
            Ok(Invocation::Vectors {
                out: PathBuf::from(required(&given, "--out")?),
            })
        }
        "cross" => {
            only(&given, &["--mine", "--theirs"])?;
            Ok(Invocation::Cross {
                mine: PathBuf::from(required(&given, "--mine")?),
                theirs: PathBuf::from(required(&given, "--theirs")?),
            })
        }
        other => Err(Error::Usage(format!(
            "`{other}` is not a command. Run `entroq-proof help`."
        ))),
    }
}

/// The `--name value` pairs an invocation carried, in the order it gave them.
fn options(args: &[String]) -> Result<Vec<(String, String)>> {
    let mut given: Vec<(String, String)> = Vec::new();
    let mut index = 0_usize;
    while let Some(arg) = args.get(index) {
        if !arg.starts_with("--") {
            return Err(Error::Usage(format!("`{arg}` is not an option")));
        }
        let next = args
            .get(index.saturating_add(1))
            .ok_or_else(|| Error::Usage(format!("{arg} needs a value")))?;
        if given.iter().any(|(name, _)| name == arg) {
            return Err(Error::Usage(format!("{arg} was given twice")));
        }
        given.push((arg.clone(), next.clone()));
        index = index.saturating_add(2);
    }
    Ok(given)
}

/// Rejects an option the command does not take.
fn only(given: &[(String, String)], allowed: &[&str]) -> Result<()> {
    for (name, _) in given {
        if !allowed.contains(&name.as_str()) {
            return Err(Error::Usage(format!(
                "`{name}` is not an option this command takes"
            )));
        }
    }
    Ok(())
}

fn value<'a>(given: &'a [(String, String)], name: &str) -> Option<&'a str> {
    given
        .iter()
        .find(|(option, _)| option == name)
        .map(|(_, value)| value.as_str())
}

fn required<'a>(given: &'a [(String, String)], name: &str) -> Result<&'a str> {
    value(given, name).ok_or_else(|| Error::Usage(format!("{name} is required")))
}

fn count(text: &str) -> Result<u64> {
    text.parse()
        .map_err(|_| Error::Usage(format!("`{text}` is not a byte count")))
}

fn sizes(list: &str) -> Result<Vec<u64>> {
    let mut parsed = Vec::new();
    for field in list.split(',') {
        let size = count(field)?;
        if size == 0 {
            return Err(Error::Usage(String::from("a curve point needs bytes")));
        }
        parsed.push(size);
    }
    if parsed.is_empty() {
        return Err(Error::Usage(String::from("--sizes names no size")));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::{HELP, Invocation, parse};
    use std::ffi::OsString;

    fn invoke(args: &[&str]) -> super::Result<Invocation> {
        parse(args.iter().map(|arg| OsString::from(*arg)))
    }

    #[test]
    fn no_argument_asks_for_help() {
        assert!(matches!(invoke(&[]), Ok(Invocation::Help)));
        assert!(matches!(invoke(&["help"]), Ok(Invocation::Help)));
    }

    #[test]
    fn the_help_names_every_command_it_documents() {
        for command in ["curve", "curve-point", "vectors", "cross"] {
            assert!(HELP.contains(command), "{command}");
        }
    }

    /// The sizes an invocation resolved to, or none when it did not name a curve.
    fn curve_sizes(args: &[&str]) -> Option<Vec<u64>> {
        match invoke(args) {
            Ok(Invocation::Curve { sizes }) => Some(sizes),
            _ => None,
        }
    }

    #[test]
    fn a_curve_defaults_to_the_sizes_the_curve_declares() {
        assert_eq!(
            curve_sizes(&["curve"]),
            Some(Vec::from(super::curve::SIZES))
        );
    }

    #[test]
    fn a_curve_takes_the_sizes_it_is_given() {
        assert_eq!(
            curve_sizes(&["curve", "--sizes", "1024,2048"]),
            Some(vec![1_024, 2_048])
        );
    }

    #[test]
    fn a_size_that_is_not_a_count_is_refused() {
        assert!(invoke(&["curve", "--sizes", "many"]).is_err());
        assert!(invoke(&["curve", "--sizes", "0"]).is_err());
        assert!(invoke(&["curve", "--sizes"]).is_err());
    }

    #[test]
    fn a_point_needs_its_byte_count() {
        assert!(invoke(&["curve-point"]).is_err());
        assert!(matches!(
            invoke(&["curve-point", "--bytes", "1024"]),
            Ok(Invocation::CurvePoint { bytes: 1_024 })
        ));
    }

    #[test]
    fn a_comparison_needs_both_lanes() {
        assert!(invoke(&["cross", "--mine", "a"]).is_err());
        assert!(invoke(&["cross", "--theirs", "b"]).is_err());
        assert!(invoke(&["cross", "--mine", "a", "--theirs", "b"]).is_ok());
    }

    #[test]
    fn an_option_the_command_does_not_take_is_refused() {
        assert!(invoke(&["vectors", "--into", "a"]).is_err());
        assert!(invoke(&["vectors", "--out", "a", "--out", "b"]).is_err());
        assert!(invoke(&["measure"]).is_err());
    }
}
