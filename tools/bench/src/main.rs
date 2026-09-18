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
//! The report: it reads the results a measurement recorded, whole or not at all, marks every
//! operating point another point dominates on each axis that has data, and states whether a
//! second campaign reproduced the numbers of a first one. It links no competitor, so a host
//! that never built the laboratory still reads what one that did recorded.
//!
//! The measurement: it drives each codec in-process and emits one machine-readable result per
//! segment. A competitor goes through the library the laboratory built and the build script
//! linked; Entroq goes through the crate of this workspace. Both cross one call and one
//! process, so neither side of a comparison is charged for a boundary the other does not pay.
//! Every metric is reported as measured, with the call that produced it, or as unavailable,
//! with the reason.
//!
//! A host that has not built the laboratory compiles this tool, builds the laboratory with
//! it, and links on the next build. Until then a measurement fails and says so; it never
//! reports a partial set of competitors as if it were the set.
//!
//! Both build inputs live outside the repository. This tool writes no artifact inside it.

// Without the laboratory the measurement path is not compiled, so the sampling, allocator,
// counter, and environment code that only it reaches has no caller. That is the normal state
// of a host that has not built the laboratory, and it is not a reason to refuse to compile
// the tool that builds it. A host that has linked the laboratory still reports dead code.
#![cfg_attr(not(lab_linked), allow(dead_code))]

mod alloc;
mod catalog;
mod cli;
mod clock;
mod compare;
mod corpus;
mod counters;
mod digest;
// The driving boundary and the measurement that runs on it link the laboratory. A host that
// has not built it compiles everything else, and the tool says what it cannot measure.
#[cfg(lab_linked)]
mod driver;
mod environment;
mod error;
mod exec;
mod lab;
mod laboratory;
mod manifest;
#[cfg(lab_linked)]
mod measure;
mod metric;
mod pareto;
mod parse;
mod plan;
mod registry;
mod report;
mod reproduce;
#[cfg(lab_linked)]
mod result;
mod shape;
mod subject;

use std::path::PathBuf;
use std::process::ExitCode;

use cli::{CorpusAction, Invocation, ReportAction};

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
        Invocation::Report {
            action,
            results,
            baseline,
        } => {
            report(action, &results, &baseline)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Reads every recorded result the invocation named, and emits what it was asked for.
///
/// # Errors
///
/// Fails when a result cannot be read, when one is not a document the parser fully
/// understands, when the results put one operating point on one chart twice, or when either
/// campaign of a comparison holds one measured row twice.
fn report(action: ReportAction, results: &[PathBuf], baseline: &[PathBuf]) -> error::Result<()> {
    let documents = report::read(results)?;
    let produced_at = environment::timestamp()?;
    match action {
        ReportAction::Parse => {
            report::emit(&report::parsed(&documents, &produced_at))?;
            report::log_parse(&documents);
        }
        ReportAction::Pareto => {
            let charts = pareto::charts(&documents)?;
            report::emit(&report::frontier(&documents, &charts, &produced_at))?;
            report::log_frontier(&charts);
        }
        ReportAction::Compare => {
            let first = report::read(baseline)?;
            let found = reproduce::compare(&first, &documents)?;
            report::emit(&report::agreement(&first, &documents, &found, &produced_at))?;
            report::log_agreement(&found);
        }
    }
    Ok(())
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
