//! Runs one segment under a hard wall-clock budget and captures its raw output.
//!
//! A recorded tier streams output to files as it is produced, never collecting it in memory,
//! so a noisy step cannot grow the runner's footprint. An unrecorded tier writes nothing and
//! lets the step inherit the terminal. A step that overruns its budget is killed. The kill
//! reaches that child; it does not reach a process the child had already spawned.
//!
//! A step is executed directly, never through a shell. The program name `cargo` resolves
//! through the `CARGO` environment variable when it is set, so a segment invokes the same
//! toolchain that invoked the runner.
//!
//! This module does not own the budget. It enforces the one it is given.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::tier::{Segment, Step};

/// How often a running step is checked against its budget.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Where a segment's raw output goes.
///
/// An unrecorded tier creates nothing, so it inherits the terminal rather than naming a
/// directory it would have to make.
pub enum Output {
    Inherit,
    Directory(PathBuf),
}

/// What a segment did.
pub enum Outcome {
    Pass,
    Fail { step: String, code: Option<i32> },
    Timeout { step: String },
}

impl Outcome {
    pub const fn is_pass(&self) -> bool {
        matches!(self, Self::Pass)
    }

    /// The one line a journal entry carries when the status needs it.
    pub fn note(&self) -> Option<String> {
        match self {
            Self::Pass => None,
            Self::Fail { step, code } => Some(code.as_ref().map_or_else(
                || format!("`{step}` was terminated by a signal"),
                |code| format!("`{step}` exited {code}"),
            )),
            Self::Timeout { step } => Some(format!("`{step}` exceeded the segment budget")),
        }
    }
}

/// Runs every step of `segment` in order, stopping at the first step that does not pass.
///
/// Returns the outcome and the wall-clock time the segment took. The budget covers the whole
/// segment, not each step, so a segment cannot buy more time by adding steps.
///
/// # Errors
///
/// Fails when the output directory cannot be written or a step cannot be started.
pub fn run_segment(
    segment: &Segment,
    working_dir: &Path,
    budget: Duration,
    output: &Output,
) -> Result<(Outcome, Duration)> {
    let logs = open_output(segment, output)?;

    let started = Instant::now();
    for step in segment.steps {
        let remaining = budget.saturating_sub(started.elapsed());
        let outcome = run_step(step, working_dir, remaining, logs.as_ref())?;
        if !outcome.is_pass() {
            return Ok((outcome, started.elapsed()));
        }
    }
    Ok((Outcome::Pass, started.elapsed()))
}

/// Prepares the raw-output destination, and records the exact argv a directory captures.
fn open_output(segment: &Segment, output: &Output) -> Result<Option<(File, File)>> {
    let Output::Directory(dir) = output else {
        return Ok(None);
    };
    std::fs::create_dir_all(dir).map_err(|e| Error::at("create", dir, e))?;
    let commands = segment
        .steps
        .iter()
        .map(describe)
        .collect::<Vec<_>>()
        .join("\n");
    let command_path = dir.join("command.txt");
    std::fs::write(&command_path, format!("{commands}\n"))
        .map_err(|e| Error::at("write", &command_path, e))?;
    Ok(Some((
        open_log(&dir.join("stdout.txt"))?,
        open_log(&dir.join("stderr.txt"))?,
    )))
}

fn run_step(
    step: &Step,
    working_dir: &Path,
    budget: Duration,
    logs: Option<&(File, File)>,
) -> Result<Outcome> {
    let description = describe(step);
    if budget.is_zero() {
        return Ok(Outcome::Timeout { step: description });
    }

    let (out, err) = match logs {
        Some((out, err)) => (clone_log(out, &description)?, clone_log(err, &description)?),
        None => (Stdio::inherit(), Stdio::inherit()),
    };
    let mut child = Command::new(resolve(step.program))
        .args(step.args)
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .map_err(|e| Error::io(format!("start `{description}`"), e))?;

    let started = Instant::now();
    loop {
        match child
            .try_wait()
            .map_err(|e| Error::io(format!("wait for `{description}`"), e))?
        {
            Some(status) if status.success() => return Ok(Outcome::Pass),
            Some(status) => {
                return Ok(Outcome::Fail {
                    step: description,
                    code: status.code(),
                });
            }
            None => {
                if started.elapsed() >= budget {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(Outcome::Timeout { step: description });
                }
                sleep(POLL_INTERVAL);
            }
        }
    }
}

/// Resolves the program a step names.
///
/// `cargo` follows the `CARGO` variable, which Cargo sets for a program it runs, so a nested
/// invocation stays on the toolchain that started the campaign.
pub fn resolve(program: &str) -> OsString {
    if program == "cargo"
        && let Some(cargo) = std::env::var_os("CARGO")
    {
        return cargo;
    }
    OsString::from(program)
}

fn describe(step: &Step) -> String {
    let mut line = String::from(step.program);
    for arg in step.args {
        line.push(' ');
        line.push_str(arg);
    }
    line
}

fn open_log(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| Error::at("open", path, e))
}

fn clone_log(file: &File, description: &str) -> Result<Stdio> {
    file.try_clone()
        .map(Stdio::from)
        .map_err(|e| Error::io(format!("capture the output of `{description}`"), e))
}

#[cfg(test)]
mod tests {
    use super::{Outcome, describe};
    use crate::tier::Step;

    #[test]
    fn a_step_describes_its_exact_argv() {
        let step = Step {
            program: "cargo",
            args: &["fmt", "--all", "--", "--check"],
        };
        assert_eq!(describe(&step), "cargo fmt --all -- --check");
    }

    #[test]
    fn a_passing_outcome_carries_no_note() {
        assert!(Outcome::Pass.note().is_none());
    }

    #[test]
    fn a_failing_outcome_names_the_step_and_its_status() {
        let outcome = Outcome::Fail {
            step: String::from("cargo test"),
            code: Some(101),
        };
        assert_eq!(outcome.note().as_deref(), Some("`cargo test` exited 101"));
    }

    #[test]
    fn a_timeout_outcome_names_the_step() {
        let outcome = Outcome::Timeout {
            step: String::from("cargo test"),
        };
        assert_eq!(
            outcome.note().as_deref(),
            Some("`cargo test` exceeded the segment budget")
        );
    }
}
