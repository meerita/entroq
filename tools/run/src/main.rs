//! The Entroq validation runner: the program that runs a validation campaign and records
//! what it did.
//!
//! This crate owns tier budgets, segment execution, the run record, resume, and sealing. It
//! does not own what is tested. A segment names a command, and the tool behind that command
//! owns its own behavior.
//!
//! A record lands under the run root the caller names, which is outside the repository. The
//! runner writes no artifact inside the repository.

mod campaign;
mod cli;
mod clock;
mod digest;
mod error;
mod exec;
mod fuzz;
mod record;
mod report;
mod tier;
mod workspace;

use std::process::ExitCode;

use cli::{Fuzz, Invocation};

/// A segment failed or overran its budget.
const EXIT_VALIDATION: u8 = 1;
/// The runner could not run the campaign at all.
const EXIT_RUNNER: u8 = 2;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(failure) => {
            eprintln!("entroq-run: {failure}");
            ExitCode::from(EXIT_RUNNER)
        }
    }
}

fn run() -> error::Result<ExitCode> {
    match cli::parse(std::env::args_os().skip(1))? {
        Invocation::Help => {
            print!("{}", cli::HELP);
            Ok(ExitCode::SUCCESS)
        }
        Invocation::Fuzz(action) => fuzz_action(action),
        Invocation::Campaign { request, mode } => {
            let outcome = campaign::execute(&request, mode)?;
            print!("{}", report::console(&outcome));
            Ok(if outcome.status.is_success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(EXIT_VALIDATION)
            })
        }
    }
}

fn fuzz_action(action: Fuzz) -> error::Result<ExitCode> {
    match action {
        Fuzz::List => {
            print!("{}", fuzz::list());
            Ok(ExitCode::SUCCESS)
        }
        Fuzz::Build { target } => {
            fuzz::build(&target)?;
            Ok(ExitCode::SUCCESS)
        }
        Fuzz::Advance { target, runs } => {
            let outcome = fuzz::advance(&target, &runs)?;
            print!("{}", report::fuzz_console(&outcome));
            Ok(if outcome.status.is_success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(EXIT_VALIDATION)
            })
        }
    }
}
