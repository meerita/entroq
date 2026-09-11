//! Owns the tool's failure type and the exit status each failure class maps to.
//!
//! A recorded byte difference between a build and its rebuild is not an error here. It is a
//! measurement, and the rebuild record states it. This type covers only the failures that
//! stop the tool from producing a laboratory build or a rebuild result at all.

use std::fmt;
use std::io;
use std::path::Path;

pub type Result<T> = std::result::Result<T, Error>;

pub enum Error {
    /// The invocation did not name work the tool can do.
    Usage(String),
    /// A filesystem operation failed.
    Io { context: String, source: io::Error },
    /// A host tool the build depends on was missing, or reported failure.
    Tool { command: String, message: String },
    /// A laboratory build exists but does not hold what a later step needs.
    Lab { context: String, message: String },
    /// A corpus entry is not the one the registry describes.
    Corpus { context: String, message: String },
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

    pub fn lab(context: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Lab {
            context: context.into(),
            message: message.into(),
        }
    }

    pub fn corpus(context: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Corpus {
            context: context.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => write!(f, "{message}"),
            Self::Io { context, source } => write!(f, "could not {context}: {source}"),
            Self::Tool { command, message } => write!(f, "`{command}` {message}"),
            Self::Lab { context, message } | Self::Corpus { context, message } => {
                write!(f, "{context}: {message}")
            }
        }
    }
}
