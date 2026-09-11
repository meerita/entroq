//! Owns what the runner learns about the repository it validates: its root, its revision,
//! its input set, and the host it runs on.
//!
//! Every fact here comes from `git` or from the toolchain in use. The runner reads no
//! undeclared host state, and it writes nothing inside the repository.
//!
//! `git` is a required host tool. A host without it cannot produce a record, because a
//! result that cannot name its revision is not evidence.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::digest::SetDigest;
use crate::error::{Error, Result};
use crate::exec;

/// The revision a result was measured at.
///
/// A dirty working tree is recorded, not hidden. The input set digest is what actually
/// distinguishes two dirty trees at the same commit.
pub struct Revision {
    pub commit: String,
    pub dirty: bool,
}

impl Revision {
    pub fn short(&self) -> &str {
        self.commit.get(..7).unwrap_or(&self.commit)
    }

    pub fn label(&self) -> String {
        if self.dirty {
            format!("{} (dirty)", self.short())
        } else {
            String::from(self.short())
        }
    }
}

/// The inputs a campaign ran against.
pub struct InputSet {
    pub description: &'static str,
    pub file_count: usize,
    pub byte_count: u64,
    pub digest: String,
}

/// The host and toolchain a result was produced on.
pub struct Environment {
    pub os: &'static str,
    pub arch: &'static str,
    pub host: String,
    pub rustc: String,
    pub cargo: String,
    pub runner: &'static str,
}

const INPUT_SET_DESCRIPTION: &str =
    "Every file git tracks in the repository, read from the working tree.";

/// Finds the repository the runner was invoked inside.
///
/// # Errors
///
/// Fails when `git` is missing or the working directory is not inside a repository.
pub fn root() -> Result<PathBuf> {
    Ok(PathBuf::from(git(
        &["rev-parse", "--show-toplevel"],
        Path::new("."),
    )?))
}

/// Reads the revision and whether the working tree carries uncommitted changes.
///
/// # Errors
///
/// Fails when `git` cannot report the revision, which includes a repository with no commit.
pub fn revision(root: &Path) -> Result<Revision> {
    let commit = git(&["rev-parse", "HEAD"], root)?;
    let status = git(&["status", "--porcelain"], root)?;
    Ok(Revision {
        commit,
        dirty: !status.is_empty(),
    })
}

/// Digests every tracked file, in the order `git` lists them.
///
/// # Errors
///
/// Fails when `git` cannot list the tracked files, or a listed file cannot be read.
pub fn input_set(root: &Path) -> Result<InputSet> {
    let listing = git_bytes(&["ls-files", "-z"], root)?;
    // `-z` terminates each path, so the split leaves one empty tail entry.
    let mut paths = Vec::new();
    for record in listing.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        let path = std::str::from_utf8(record)
            .map_err(|_| Error::tool("git ls-files", "listed a path that is not UTF-8"))?;
        paths.push(String::from(path));
    }
    paths.sort();

    let mut set = SetDigest::new();
    for path in &paths {
        set.add(path, &root.join(path))?;
    }
    let (digest, file_count, byte_count) = set.finish();
    Ok(InputSet {
        description: INPUT_SET_DESCRIPTION,
        file_count,
        byte_count,
        digest,
    })
}

/// Records the host and the toolchain that produced a result.
///
/// # Errors
///
/// Fails when `rustc` or `cargo` cannot report its version.
pub fn environment() -> Result<Environment> {
    let verbose = tool("rustc", &["--version", "--verbose"], Path::new("."))?;
    let host = verbose
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map_or_else(|| String::from("unknown"), String::from);
    let rustc = verbose
        .lines()
        .next()
        .map_or_else(|| String::from("unknown"), String::from);
    let cargo = tool("cargo", &["--version"], Path::new("."))?;
    Ok(Environment {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        host,
        rustc,
        cargo,
        runner: env!("CARGO_PKG_VERSION"),
    })
}

fn git(args: &[&str], working_dir: &Path) -> Result<String> {
    tool("git", args, working_dir)
}

fn tool(program: &str, args: &[&str], working_dir: &Path) -> Result<String> {
    let bytes = raw(program, args, working_dir)?;
    String::from_utf8(bytes)
        .map(|text| String::from(text.trim()))
        .map_err(|_| Error::tool(describe(program, args), "produced output that is not UTF-8"))
}

fn git_bytes(args: &[&str], working_dir: &Path) -> Result<Vec<u8>> {
    raw("git", args, working_dir)
}

fn raw(program: &str, args: &[&str], working_dir: &Path) -> Result<Vec<u8>> {
    let command = describe(program, args);
    let output = Command::new(exec::resolve(program))
        .args(args)
        .current_dir(working_dir)
        .output()
        .map_err(|e| Error::io(format!("run `{command}`"), e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().next().unwrap_or("failed with no message");
        return Err(Error::tool(command, format!("failed: {reason}")));
    }
    Ok(output.stdout)
}

fn describe(program: &str, args: &[&str]) -> String {
    let mut line = String::from(program);
    for arg in args {
        line.push(' ');
        line.push_str(arg);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::Revision;

    #[test]
    fn a_clean_revision_is_labelled_by_its_short_commit() {
        let revision = Revision {
            commit: String::from("1a195e3c0ffee00ddeadbeef"),
            dirty: false,
        };
        assert_eq!(revision.short(), "1a195e3");
        assert_eq!(revision.label(), "1a195e3");
    }

    #[test]
    fn a_dirty_revision_says_so() {
        let revision = Revision {
            commit: String::from("1a195e3c0ffee00ddeadbeef"),
            dirty: true,
        };
        assert_eq!(revision.label(), "1a195e3 (dirty)");
    }

    #[test]
    fn a_short_commit_string_is_not_truncated() {
        let revision = Revision {
            commit: String::from("1a19"),
            dirty: false,
        };
        assert_eq!(revision.short(), "1a19");
    }
}
