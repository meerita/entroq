//! Owns what this binary linked: which competitor library, from which path, with which
//! checksum, and whether that is still what the laboratory holds.
//!
//! A result has to be able to say which artifact produced it. The build records the absolute
//! path and the digest of every archive it pointed the linker at, and this module compares
//! those against the manifest beside the build now. A development host commonly carries its
//! own copies of the same projects, so this is a live question rather than a formality.
//!
//! Nothing here can be satisfied by an assertion in a comment. The archives the linker was
//! given are the ones the manifests name, the search path holds no other copy of them, and
//! a competitor whose library publishes a version reports it at run time, which is checked
//! against the version the catalog pins.
//!
//! This module does not own the laboratory layout or the manifest format.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::lab::Layout;
use crate::manifest::Manifest;

/// What the build recorded for every library it linked, or nothing when the laboratory was
/// not complete at build time.
const LINKED: Option<&str> = option_env!("ENTROQ_LAB_LIBRARIES");

/// One library this binary was linked against.
pub struct Library {
    pub codec: String,
    pub version: String,
    /// The path relative to the build prefix, which is what the manifest names.
    pub relative: String,
    pub absolute: PathBuf,
    /// The digest of the bytes the linker was given.
    pub linked: String,
}

/// What a result states about one linked library now.
pub struct Check {
    pub codec: String,
    pub version: String,
    pub library: String,
    pub linked_from: String,
    pub digest: String,
    /// Whether the manifest in the laboratory still records this digest.
    pub matches_manifest: bool,
    pub note: String,
}

/// The libraries this binary linked, in the order the build emitted them.
pub fn linked() -> Vec<Library> {
    let Some(text) = LINKED else {
        return Vec::new();
    };
    text.split(';')
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let mut fields = record.split('|');
            Some(Library {
                codec: String::from(fields.next()?),
                version: String::from(fields.next()?),
                relative: String::from(fields.next()?),
                absolute: PathBuf::from(fields.next()?),
                linked: String::from(fields.next()?),
            })
        })
        .collect()
}

/// Whether this binary has a laboratory linked at all.
pub fn is_linked() -> bool {
    !linked().is_empty()
}

/// What the tool says when it was built without a laboratory.
pub const NOT_LINKED: &str = "this harness was built without the competitor laboratory \
linked, so it can measure nothing. Build the laboratory first, then build again: the build \
links every pinned competitor or none, and it linked none.";

/// Compares every linked library against the manifest the laboratory holds now.
///
/// # Errors
///
/// Fails when a manifest cannot be read or does not carry a library line.
pub fn confirm(libraries: &[Library], layout: &Layout) -> Result<Vec<Check>> {
    let mut checks = Vec::new();
    for library in libraries {
        let version_dir = layout.root().join(&library.codec).join(&library.version);
        let manifest = Manifest::read(&version_dir)?;
        let recorded = manifest
            .libraries()?
            .into_iter()
            .find(|(relative, _)| *relative == library.relative)
            .map(|(_, digest)| digest);
        let matches = recorded.as_deref() == Some(library.linked.as_str());
        checks.push(Check {
            codec: library.codec.clone(),
            version: library.version.clone(),
            library: library.relative.clone(),
            linked_from: library.absolute.display().to_string(),
            digest: library.linked.clone(),
            matches_manifest: matches,
            note: note(library, recorded.as_deref(), &version_dir),
        });
    }
    Ok(checks)
}

fn note(library: &Library, recorded: Option<&str>, version_dir: &Path) -> String {
    match recorded {
        Some(digest) if digest == library.linked => format!(
            "linked from the laboratory build at {}, and the manifest there still records \
             this digest",
            version_dir.display()
        ),
        Some(digest) => format!(
            "the manifest at {} now records {digest}, and this binary linked {}. The \
             laboratory was rebuilt after this binary was linked, so this result does not \
             describe the build the manifest names.",
            version_dir.display(),
            library.linked
        ),
        None => format!(
            "the manifest at {} names no {}, so the artifact this binary linked cannot be \
             identified from the laboratory",
            version_dir.display(),
            library.relative
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{Library, NOT_LINKED, is_linked, linked, note};
    use std::path::{Path, PathBuf};

    fn library(digest: &str) -> Library {
        Library {
            codec: String::from("zstd"),
            version: String::from("v1.5.7"),
            relative: String::from("lib/libzstd.a"),
            absolute: PathBuf::from("/lab/zstd/v1.5.7/build/lib/libzstd.a"),
            linked: String::from(digest),
        }
    }

    #[test]
    fn a_binary_states_whether_it_linked_a_laboratory() {
        // Either answer is a fact about the build. A binary that guesses is not.
        assert_eq!(is_linked(), !linked().is_empty());
    }

    #[test]
    fn a_matching_manifest_says_where_the_bytes_came_from() {
        let library = library("sha256:aa");
        let text = note(&library, Some("sha256:aa"), Path::new("/lab/zstd/v1.5.7"));
        assert!(text.contains("still records"), "{text}");
    }

    #[test]
    fn a_manifest_that_moved_on_says_the_result_is_not_about_that_build() {
        let library = library("sha256:aa");
        let text = note(&library, Some("sha256:bb"), Path::new("/lab/zstd/v1.5.7"));
        assert!(text.contains("does not describe the build"), "{text}");
        assert!(text.contains("sha256:bb"), "{text}");
    }

    #[test]
    fn a_manifest_with_no_such_library_says_the_artifact_cannot_be_identified() {
        let library = library("sha256:aa");
        let text = note(&library, None, Path::new("/lab/zstd/v1.5.7"));
        assert!(text.contains("cannot be identified"), "{text}");
    }

    #[test]
    fn the_unlinked_message_says_what_to_do_about_it() {
        assert!(NOT_LINKED.contains("Build the laboratory first"));
    }
}
