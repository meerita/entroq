//! The Entroq benchmark tool.
//!
//! This revision owns the competitor laboratory: it fetches each pinned competitor, builds it
//! as a static library with the release configuration its own project recommends, records how
//! that build was produced, and measures a rebuild from that record against it.
//!
//! It measures no compression, and it links no competitor. A harness that links what this
//! tool builds comes later, so this crate compiles on a host that carries no C library.
//!
//! The laboratory is a build input that lives outside the repository. This tool writes no
//! artifact inside the repository.

mod catalog;
mod cli;
mod compare;
mod digest;
mod error;
mod exec;
mod lab;
mod manifest;

use std::process::ExitCode;

use cli::Invocation;

/// The tool could not produce the build or the rebuild.
const EXIT_FAILURE: u8 = 1;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(failure) => {
            eprintln!("entroq-bench: {failure}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

fn run() -> error::Result<ExitCode> {
    match cli::parse(std::env::args_os().skip(1))? {
        Invocation::Help => {
            print!("{}", cli::HELP);
            Ok(ExitCode::SUCCESS)
        }
        Invocation::Build { codec, lab } => {
            lab::build(codec, &lab::Layout::resolve(lab)?)?;
            Ok(ExitCode::SUCCESS)
        }
        Invocation::Verify { codec, lab } => {
            lab::verify(codec, &lab::Layout::resolve(lab)?)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}
