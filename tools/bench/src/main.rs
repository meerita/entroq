//! The Entroq benchmark tool.
//!
//! It owns two build inputs and the measurement that reads them.
//!
//! The competitor laboratory: it fetches each pinned competitor, builds it as a static
//! library with the release configuration its own project recommends, records how that build
//! was produced, and measures a rebuild from that record against it.
//!
//! The corpus: it holds the registry of every corpus entry, generates the project corpus
//! from recorded seeds, fetches the registered public corpora, and checks both against the
//! checksums the registry pins.
//!
//! The measurement: it drives each competitor in-process, through the library the laboratory
//! built and the build script linked, and emits one machine-readable result per segment.
//! Every metric is reported as measured, with the call that produced it, or as unavailable,
//! with the reason. Entroq itself has no codec path at this revision, so every result states
//! an empty Entroq column and says why.
//!
//! A host that has not built the laboratory compiles this tool, builds the laboratory with
//! it, and links on the next build. Until then a measurement fails and says so; it never
//! reports a partial set of competitors as if it were the set.
//!
//! Both build inputs live outside the repository. This tool writes no artifact inside it.

mod alloc;
mod catalog;
mod cli;
mod clock;
mod compare;
// The competitor boundary and the measurement that drives it link the laboratory. A host
// that has not built it compiles everything else, and the tool says what it cannot measure.
#[cfg(lab_linked)]
mod competitor;
mod corpus;
mod counters;
mod digest;
mod environment;
mod error;
mod exec;
mod lab;
mod laboratory;
mod manifest;
#[cfg(lab_linked)]
mod measure;
mod metric;
mod plan;
mod registry;
#[cfg(lab_linked)]
mod result;
mod shape;

use std::process::ExitCode;

use cli::{CorpusAction, Invocation};

/// The tool could not produce the build, the rebuild, or the measurement.
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
        Invocation::Measure(request) => {
            measure(&request)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Measures one segment and emits its result.
///
/// # Errors
///
/// Fails when the laboratory is not linked, an input is not the one the registry describes,
/// or a competitor library refuses the work.
#[cfg(lab_linked)]
fn measure(request: &plan::Request) -> error::Result<()> {
    let outcome = measure::run(request)?;
    let produced_at = environment::timestamp()?;
    let document = result::document(request, &outcome, &produced_at);
    let written = result::emit(&document)?;
    result::log(request, &outcome, written.as_deref());
    Ok(())
}

/// Reports that this binary has no laboratory to measure with.
///
/// # Errors
///
/// Always. A binary built without the laboratory measures nothing, and saying so is the
/// only honest outcome.
#[cfg(not(lab_linked))]
fn measure(_request: &plan::Request) -> error::Result<()> {
    Err(error::Error::measure(
        "the benchmark harness",
        laboratory::NOT_LINKED,
    ))
}
