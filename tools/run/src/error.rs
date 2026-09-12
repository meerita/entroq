//! Owns the runner's failure type and the exit status each failure class maps to.
//!
//! A failed segment is not an error here. A segment that fails or overruns its budget is a
//! campaign outcome, and the campaign reports it. This type covers only the failures that
//! stop the runner from running a campaign at all.

use std::fmt;
use std::io;
use std::path::Path;

pub type Result<T> = std::result::Result<T, Error>;

pub enum Error {
    /// The invocation did not name a campaign the runner can run.
    Usage(String),
    /// A filesystem operation failed.
    Io { context: String, source: io::Error },
    /// A host tool the runner depends on was missing, or reported failure.
    Tool { command: String, message: String },
    /// A record exists but does not hold what a campaign needs to continue.
    Record { context: String, message: String },
    /// The runner recognizes the request but this revision cannot serve it.
    Unavailable(String),
}

impl Error {
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    pub fn at(verb: &str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            context: format!("{verb} {}", path.display()),
            source,
        }
    }

    pub fn tool(command: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Tool {
            command: command.into(),
            message: message.into(),
        }
    }

    pub fn record(context: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Record {
            context: context.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) | Self::Unavailable(message) => write!(f, "{message}"),
            Self::Io { context, source } => write!(f, "could not {context}: {source}"),
            Self::Tool { command, message } => write!(f, "`{command}` {message}"),
            Self::Record { context, message } => write!(f, "{context}: {message}"),
        }
    }
}
