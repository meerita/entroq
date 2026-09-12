//! Runs a host command and reports what it did.
//!
//! A build step inherits the terminal, so whatever runs the tool captures the build log as it
//! is produced. A query captures its output instead, because the tool reads the answer.
//!
//! A command is executed directly, never through a shell. No argument is expanded, quoted, or
//! word split by anything but this module.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{Error, Result};

/// Runs a command and waits for it, letting its output reach the caller's terminal.
///
/// # Errors
///
/// Fails when the command cannot be started, or exits with anything but success.
pub fn run(argv: &[String], working_dir: &Path) -> Result<()> {
    let (program, rest) = split(argv)?;
    let description = describe(argv);
    let status = Command::new(program)
        .args(rest)
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| Error::io(format!("start `{description}`"), e))?;
    if status.success() {
        return Ok(());
    }
    Err(Error::tool(
        description,
        status.code().map_or_else(
            || String::from("was terminated by a signal"),
            |code| format!("exited {code}"),
        ),
    ))
}

/// Runs a command and writes its standard output to `path`, replacing whatever is there.
///
/// The output is streamed to the file by the operating system, so an archive member of any
/// size passes through without the tool holding it.
///
/// # Errors
///
/// Fails when the file cannot be created, the command cannot be started, or the command
/// exits with anything but success.
pub fn run_into(argv: &[String], working_dir: &Path, path: &Path) -> Result<()> {
    let (program, rest) = split(argv)?;
    let description = describe(argv);
    let sink = std::fs::File::create(path).map_err(|e| Error::at("create", path, e))?;
    let status = Command::new(program)
        .args(rest)
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(sink))
        .status()
        .map_err(|e| Error::io(format!("start `{description}`"), e))?;
    if status.success() {
        return Ok(());
    }
    Err(Error::tool(
        description,
        status.code().map_or_else(
            || String::from("was terminated by a signal"),
            |code| format!("exited {code}"),
        ),
    ))
}

/// Runs a command and returns its trimmed standard output.
///
/// # Errors
///
/// Fails when the command cannot be started, exits with anything but success, or writes
/// output that is not UTF-8.
pub fn capture(argv: &[String], working_dir: &Path) -> Result<String> {
    let (program, rest) = split(argv)?;
    let description = describe(argv);
    let output = Command::new(program)
        .args(rest)
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::io(format!("start `{description}`"), e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().next().unwrap_or("failed with no message");
        return Err(Error::tool(description, format!("failed: {reason}")));
    }
    String::from_utf8(output.stdout)
        .map(|text| String::from(text.trim()))
        .map_err(|_| Error::tool(description, "produced output that is not UTF-8"))
}

/// Builds an argument vector from string-like parts.
pub fn argv<I, S>(parts: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    parts
        .into_iter()
        .map(|part| String::from(part.as_ref()))
        .collect()
}

/// The one line a manifest records for a command.
pub fn describe(argv: &[String]) -> String {
    argv.join(" ")
}

fn split(argv: &[String]) -> Result<(&String, &[String])> {
    match argv.split_first() {
        Some((program, rest)) => Ok((program, rest)),
        None => Err(Error::lab("a command", "names no program")),
    }
}

#[cfg(test)]
mod tests {
    use super::{argv, capture, describe, run, run_into};
    use std::path::Path;

    #[test]
    fn a_redirected_command_writes_its_output_to_the_file() {
        let path = std::env::temp_dir().join("entroq-bench-exec-redirect");
        assert!(run_into(&argv(["echo", "member"]), Path::new("."), &path).is_ok());
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some("member\n")
        );
    }

    #[test]
    fn a_redirected_command_that_fails_is_reported() {
        let path = std::env::temp_dir().join("entroq-bench-exec-redirect-fail");
        assert!(run_into(&argv(["false"]), Path::new("."), &path).is_err());
    }

    #[test]
    fn an_argument_vector_describes_itself_as_one_line() {
        assert_eq!(
            describe(&argv(["cmake", "--build", "."])),
            "cmake --build ."
        );
    }

    #[test]
    fn a_successful_command_passes() {
        assert!(run(&argv(["true"]), Path::new(".")).is_ok());
    }

    #[test]
    fn a_failing_command_names_its_exit_status() {
        let failure = run(&argv(["false"]), Path::new("."));
        assert!(failure.is_err());
        let message = failure.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("exited 1"), "{message}");
    }

    #[test]
    fn a_command_that_does_not_exist_is_reported_not_ignored() {
        assert!(run(&argv(["entroq-no-such-tool"]), Path::new(".")).is_err());
    }

    #[test]
    fn a_query_returns_its_trimmed_output() {
        let text = capture(&argv(["echo", "pinned"]), Path::new("."));
        assert_eq!(text.ok().as_deref(), Some("pinned"));
    }

    #[test]
    fn an_empty_command_is_rejected() {
        assert!(run(&[], Path::new(".")).is_err());
    }
}
