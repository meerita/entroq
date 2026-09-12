//! Owns the tool's failure type.
//!
//! A proof that runs and does not hold is not an error here. It is an outcome, and the
//! command that ran it reports it and exits non-zero. This type covers only the failures that
//! stop a proof from running at all.

use std::fmt;
use std::io;
use std::path::Path;

pub type Result<T> = std::result::Result<T, Error>;

pub enum Error {
    /// The invocation did not name work the tool can do.
    Usage(String),
    /// A filesystem operation failed.
    Io { context: String, source: io::Error },
    /// The codec refused a request this tool made of it.
    Codec(codec::format::Error),
    /// A child process could not be run, or did not report what it was asked for.
    Child(String),
}

impl Error {
    pub fn at(verb: &str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            context: format!("{verb} {}", path.display()),
            source,
        }
    }

    pub fn child(message: impl Into<String>) -> Self {
        Self::Child(message.into())
    }
}

impl From<codec::format::Error> for Error {
    fn from(source: codec::format::Error) -> Self {
        Self::Codec(source)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) | Self::Child(message) => write!(f, "{message}"),
            Self::Io { context, source } => write!(f, "could not {context}: {source}"),
            Self::Codec(source) => write!(f, "the codec refused the request: {source}"),
        }
    }
}
