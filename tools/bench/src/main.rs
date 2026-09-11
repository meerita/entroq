//! The Entroq benchmark tool.
//!
//! This revision owns two build inputs a measurement will need, and measures nothing itself.
//!
//! The competitor laboratory: it fetches each pinned competitor, builds it as a static
//! library with the release configuration its own project recommends, records how that build
//! was produced, and measures a rebuild from that record against it. It links no competitor,
//! so this crate compiles on a host that carries no C library.
//!
//! The corpus: it holds the registry of every corpus entry, generates the project corpus
//! from recorded seeds, fetches the registered public corpora, and checks both against the
//! checksums the registry pins.
//!
//! Both live outside the repository. This tool writes no artifact inside it.

mod catalog;
mod cli;
mod compare;
mod corpus;
mod digest;
mod error;
mod exec;
mod lab;
mod manifest;
mod registry;
mod shape;

use std::process::ExitCode;

use cli::{CorpusAction, Invocation};

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
        Invocation::Corpus {
            action,
            selection,
            corpus,
        } => {
            match action {
                // Listing reads the registry alone, so it never creates a cache root.
                CorpusAction::List => corpus::list(&selection)?,
                CorpusAction::Build => {
                    corpus::build(&selection, &corpus::Layout::resolve(corpus)?)?;
                }
                CorpusAction::Reproduce => {
                    corpus::reproduce(&selection, &corpus::Layout::resolve(corpus)?)?;
                }
                CorpusAction::Verify => {
                    corpus::verify(&selection, &corpus::Layout::resolve(corpus)?)?;
                }
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}
