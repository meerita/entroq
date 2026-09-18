//! The Entroq skeleton proofs: the two gates that no unit test can hold.
//!
//! The memory growth curve measures what the streaming pair costs as the logical input grows
//! by three orders of magnitude. A declared bound is a claim until something reads the
//! process's own resident set and allocator while the bound is under load, and a test that
//! runs in the same process as every other test cannot.
//!
//! The cross-architecture vectors settle whether two architectures write the same bytes and
//! read each other's. One lane writes them, another lane writes them, and each side compares.
//!
//! Both write their report to the directory a recorded run names for the segment's evidence,
//! which is outside the repository. This tool writes no artifact inside it.

mod alloc;
mod cli;
mod content;
mod curve;
mod error;
mod host;
mod vectors;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cli::Invocation;

/// The variable a recorded run names the segment's evidence directory with.
const SEGMENT_DIR: &str = "ENTROQ_SEGMENT_DIR";

/// A proof ran and did not hold.
const EXIT_PROOF: u8 = 1;
/// The tool could not run the proof at all.
const EXIT_TOOL: u8 = 2;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(failure) => {
            eprintln!("entroq-proof: {failure}");
            ExitCode::from(EXIT_TOOL)
        }
    }
}

fn run() -> error::Result<ExitCode> {
    match cli::parse(std::env::args_os().skip(1))? {
        Invocation::Help => {
            print!("{}", cli::HELP);
            Ok(ExitCode::SUCCESS)
        }
        Invocation::CurvePoint { bytes } => {
            let point = curve::point(bytes)?;
            print!("{}", curve::line(&point));
            Ok(ExitCode::SUCCESS)
        }
        Invocation::Curve { sizes } => {
            let program = std::env::current_exe()
                .map_err(|e| error::Error::at("find", Path::new("this program"), e))?;
            let (points, refusal, verdict) = curve::run(&program, &sizes)?;
            let table = curve::table(&points, &refusal, &verdict);
            print!("{table}");
            emit("curve.md", &table)?;
            Ok(outcome(verdict.flat))
        }
        Invocation::Vectors { out } => {
            let report = vectors::write(&out)?;
            print!("{report}");
            Ok(ExitCode::SUCCESS)
        }
        Invocation::Cross { mine, theirs } => {
            let comparison = vectors::cross(&mine, &theirs)?;
            let report = vectors::report(&comparison, &mine, &theirs);
            print!("{report}");
            emit("byteorder.md", &report)?;
            Ok(outcome(comparison.identical))
        }
    }
}

fn outcome(held: bool) -> ExitCode {
    if held {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_PROOF)
    }
}

/// Writes a report into the evidence directory a recorded run named, when one named it.
fn emit(name: &str, body: &str) -> error::Result<()> {
    let Some(dir) = std::env::var_os(SEGMENT_DIR) else {
        return Ok(());
    };
    let path = PathBuf::from(dir).join(name);
    let mut file =
        std::fs::File::create(&path).map_err(|e| error::Error::at("create", &path, e))?;
    file.write_all(body.as_bytes())
        .map_err(|e| error::Error::at("write", &path, e))
}
