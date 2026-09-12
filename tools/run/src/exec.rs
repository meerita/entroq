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
//! A recorded segment is told where its evidence goes, through `ENTROQ_SEGMENT_DIR`. A tool
//! that produces an artifact of its own, such as a benchmark result, writes it there. The
//! directory is inside the run record and never inside the repository. An unrecorded tier
//! sets nothing, and a tool that finds nothing set writes no artifact.
//!
//! A preparation command and a host-tool probe are the two things here that no budget covers
//! and no record captures. A preparation command builds what a segment then measures, so a
//! compile does not spend the budget the segment needs for the work it records.
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

/// The variable that names the directory a segment's evidence goes to.
pub const SEGMENT_DIR: &str = "ENTROQ_SEGMENT_DIR";

/// One step, resolved to the exact argv it runs.
///
/// A static segment states its argv at compile time. A segment the caller composes states
/// paths it learns at run time, such as the corpus directory one fuzz invocation reads.
pub struct Invocation {
    program: String,
    args: Vec<String>,
}

impl Invocation {
    pub fn new(program: &str, args: impl IntoIterator<Item = String>) -> Self {
        Self {
            program: String::from(program),
            args: args.into_iter().collect(),
        }
    }

    fn of(step: &Step) -> Self {
        Self::new(step.program, step.args.iter().map(|arg| String::from(*arg)))
    }

    /// The exact argv, program first, in the shape a manifest records it.
    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec![self.program.clone()];
        argv.extend(self.args.iter().cloned());
        argv
    }

    fn describe(&self) -> String {
        let mut line = self.program.clone();
        for arg in &self.args {
            line.push(' ');
            line.push_str(arg);
        }
        line
    }
}

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
    let steps: Vec<Invocation> = segment.steps.iter().map(Invocation::of).collect();
    run_steps(&steps, working_dir, budget, output)
}

/// Runs every step in order, stopping at the first step that does not pass.
///
/// This is `run_segment` for a segment the caller composed rather than declared.
///
/// # Errors
///
/// Fails when the output directory cannot be written or a step cannot be started.
pub fn run_steps(
    steps: &[Invocation],
    working_dir: &Path,
    budget: Duration,
    output: &Output,
) -> Result<(Outcome, Duration)> {
    let logs = open_output(steps, output)?;

    let evidence = match output {
        Output::Directory(dir) => Some(dir.as_path()),
        Output::Inherit => None,
    };

    let started = Instant::now();
    for step in steps {
        let remaining = budget.saturating_sub(started.elapsed());
        let outcome = run_step(step, working_dir, remaining, logs.as_ref(), evidence)?;
        if !outcome.is_pass() {
            return Ok((outcome, started.elapsed()));
        }
    }
    Ok((Outcome::Pass, started.elapsed()))
}

/// Runs one preparation command to completion, and reports whether it succeeded.
///
/// A preparation command is not a validation step, so no budget covers it and nothing
/// records it. Its output goes to the terminal.
///
/// # Errors
///
/// Fails when the command cannot be started or waited for.
pub fn prepare(invocation: &Invocation, working_dir: &Path) -> Result<bool> {
    let description = invocation.describe();
    Command::new(resolve(&invocation.program))
        .args(&invocation.args)
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .status()
        .map(|status| status.success())
        .map_err(|e| Error::io(format!("run `{description}`"), e))
}

/// Runs one command with its output discarded, and reports whether it succeeded.
///
/// This is how the runner asks whether a host tool is present before it depends on one.
///
/// # Errors
///
/// Fails when the command cannot be started or waited for. A command that is absent is not
/// an error here: it is a `false`.
pub fn probe(invocation: &Invocation, working_dir: &Path) -> Result<bool> {
    match Command::new(resolve(&invocation.program))
        .args(&invocation.args)
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => Ok(status.success()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::io(format!("run `{}`", invocation.describe()), e)),
    }
}

/// Prepares the raw-output destination, and records the exact argv a directory captures.
fn open_output(steps: &[Invocation], output: &Output) -> Result<Option<(File, File)>> {
    let Output::Directory(dir) = output else {
        return Ok(None);
    };
    // A directory already here belongs to an attempt that was interrupted before it wrote a
    // journal line. No line cites it, and keeping it would blend two attempts into one.
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| Error::at("clear", dir, e))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| Error::at("create", dir, e))?;
    let commands = steps
        .iter()
        .map(Invocation::describe)
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
    step: &Invocation,
    working_dir: &Path,
    budget: Duration,
    logs: Option<&(File, File)>,
    evidence: Option<&Path>,
) -> Result<Outcome> {
    let description = step.describe();
    if budget.is_zero() {
        return Ok(Outcome::Timeout { step: description });
    }

    let (out, err) = match logs {
        Some((out, err)) => (clone_log(out, &description)?, clone_log(err, &description)?),
        None => (Stdio::inherit(), Stdio::inherit()),
    };
    let mut command = Command::new(resolve(&step.program));
    let _ = command
        .args(&step.args)
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    match evidence {
        Some(dir) => {
            let _ = command.env(SEGMENT_DIR, dir);
        }
        // An unrecorded tier has no evidence directory, and a variable left over from an
        // outer invocation would send an artifact somewhere this campaign did not choose.
        None => {
            let _ = command.env_remove(SEGMENT_DIR);
        }
    }
    let mut child = command
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
    use super::{Invocation, Outcome, Output, run_segment};
    use crate::tier::{Segment, Step};
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn a_step_describes_its_exact_argv() {
        let step = Step {
            program: "cargo",
            args: &["fmt", "--all", "--", "--check"],
        };
        assert_eq!(
            Invocation::of(&step).describe(),
            "cargo fmt --all -- --check"
        );
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
    fn an_interrupted_attempt_does_not_leak_output_into_its_retry() {
        let dir = std::env::temp_dir().join("entroq-run-exec-retry");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(std::fs::create_dir_all(&dir).is_ok());
        assert!(std::fs::write(dir.join("stdout.txt"), b"interrupted attempt").is_ok());

        let segment = Segment {
            id: "echo",
            description: "A segment that writes one known line.",
            steps: &[Step {
                program: "echo",
                args: &["second"],
            }],
        };
        let run = run_segment(
            &segment,
            Path::new("."),
            Duration::from_secs(10),
            &Output::Directory(dir.clone()),
        );
        assert!(matches!(run, Ok((Outcome::Pass, _))));
        let stdout = std::fs::read_to_string(dir.join("stdout.txt")).unwrap_or_default();
        assert_eq!(stdout, "second\n");
    }

    #[test]
    fn a_recorded_segment_is_told_where_its_evidence_goes() {
        let dir = std::env::temp_dir().join("entroq-run-exec-evidence");
        let _ = std::fs::remove_dir_all(&dir);
        let segment = Segment {
            id: "evidence",
            description: "A segment that reports the directory it was given.",
            steps: &[Step {
                program: "sh",
                args: &["-c", "printf %s \"$ENTROQ_SEGMENT_DIR\""],
            }],
        };
        let run = run_segment(
            &segment,
            Path::new("."),
            Duration::from_secs(10),
            &Output::Directory(dir.clone()),
        );
        assert!(matches!(run, Ok((Outcome::Pass, _))));
        let reported = std::fs::read_to_string(dir.join("stdout.txt")).unwrap_or_default();
        assert_eq!(reported, dir.display().to_string());
    }

    #[test]
    fn an_unrecorded_segment_is_told_nothing_so_it_writes_no_artifact() {
        let segment = Segment {
            id: "no-evidence",
            description: "A segment that fails when it is given an evidence directory.",
            steps: &[Step {
                program: "sh",
                args: &["-c", "test -z \"$ENTROQ_SEGMENT_DIR\""],
            }],
        };
        let run = run_segment(
            &segment,
            Path::new("."),
            Duration::from_secs(10),
            &Output::Inherit,
        );
        assert!(matches!(run, Ok((Outcome::Pass, _))));
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
