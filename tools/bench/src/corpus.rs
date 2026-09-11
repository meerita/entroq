//! Owns the corpus cache: where a registered entry lands, how it is obtained, and how it is
//! checked against the registry that describes it.
//!
//! The cache is a build input that lives outside the repository, beside the run records. No
//! corpus byte is ever written inside the repository, and nothing here reads a path the
//! caller did not name or default.
//!
//! Every entry passes the same gate before it may be used: its bytes must be exactly the
//! size the registry states and hash to the digest the registry pins. A fetched entry is
//! checked twice, once on the archive as it arrived and once on the bytes it unpacked to,
//! so a corrupted transfer and a drifted upstream are distinguishable.
//!
//! This module does not own the registry, the shapes, or the segment budget a campaign runs
//! it under.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::digest;
use crate::error::{Error, Result};
use crate::exec::{self, argv};
use crate::registry::{CLASSES, Download, Entry, Origin, Recipe, Selection, Unpack};
use crate::shape;

/// The build input that names the corpus cache, when no argument does.
pub const CORPUS_VARIABLE: &str = "CORPUS";
/// Where the cache sits when neither the argument nor the variable names it.
pub const CORPUS_DEFAULT: &str = "../corpus";

const CACHE_DIR: &str = "cache";
const DOWNLOAD_DIR: &str = "downloads";
const REPRODUCE_DIR: &str = ".reproduce";

/// Where every cache path is derived from.
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// Resolves the cache root, creating it when it does not exist yet.
    ///
    /// The argument wins, then the `CORPUS` build input, then the default beside the
    /// repository.
    ///
    /// # Errors
    ///
    /// Fails when the root cannot be created or cannot be made absolute.
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self> {
        let named = explicit
            .or_else(|| std::env::var_os(CORPUS_VARIABLE).map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from(CORPUS_DEFAULT));
        std::fs::create_dir_all(&named).map_err(|e| Error::at("create", &named, e))?;
        let root = named
            .canonicalize()
            .map_err(|e| Error::at("resolve", &named, e))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The file one entry's bytes live in.
    pub fn entry(&self, entry: &Entry) -> PathBuf {
        self.root.join(CACHE_DIR).join(entry.file())
    }

    /// The archive a fetched entry was obtained from.
    fn archive(&self, entry: &Entry, download: &Download) -> PathBuf {
        self.root
            .join(DOWNLOAD_DIR)
            .join(entry.name)
            .join(download.archive)
    }

    /// Where a reproduction writes, which is removed once the comparison is made.
    fn reproduction(&self) -> PathBuf {
        self.root.join(REPRODUCE_DIR)
    }
}

/// Materializes every selected entry, fetching what is fetched and generating what is
/// generated.
///
/// An entry whose cached bytes already match the registry is left alone, so a warm cache
/// costs one digest and no network.
///
/// # Errors
///
/// Fails when an entry cannot be obtained, or when the bytes obtained are not the ones the
/// registry describes.
pub fn build(selection: &Selection, layout: &Layout) -> Result<()> {
    let entries = require(selection)?;
    for entry in entries {
        let path = layout.entry(entry);
        if present(entry, &path) {
            println!("{:<34} present  {}", entry.name, path.display());
            continue;
        }
        match &entry.origin {
            Origin::Generated(recipe) => generate(entry, recipe, &path)?,
            Origin::Fetched(download) => fetch(entry, download, layout, &path)?,
        }
        confirm(entry, &path)?;
        println!(
            "{:<34} {:<8} {:>12} bytes  {}",
            entry.name,
            entry.class().name(),
            entry.bytes,
            path.display()
        );
    }
    Ok(())
}

/// Generates every selected generated entry twice, into two directories neither run shares,
/// and compares what came out.
///
/// This is what makes a recorded seed a pin rather than a label. Both generations are also
/// compared against the digest the registry carries, so a generator that changed since the
/// registry was written is caught even when it changed consistently.
///
/// # Errors
///
/// Fails when a generation cannot be written, the two generations differ, or either differs
/// from the registry.
pub fn reproduce(selection: &Selection, layout: &Layout) -> Result<()> {
    let entries: Vec<&Entry> = require(selection)?
        .into_iter()
        .filter(|entry| matches!(entry.origin, Origin::Generated(_)))
        .collect();
    if entries.is_empty() {
        return Err(Error::corpus(
            selection.describe(),
            "names no generated entry, and only a generated entry can be reproduced",
        ));
    }

    let scratch = layout.reproduction();
    reset(&scratch)?;
    let first = scratch.join("first");
    let second = scratch.join("second");

    for entry in entries {
        let Origin::Generated(recipe) = &entry.origin else {
            continue;
        };
        let mut digests = Vec::new();
        for directory in [&first, &second] {
            let path = directory.join(entry.file());
            generate(entry, recipe, &path)?;
            digests.push(digest::file(&path)?);
        }
        let (Some(first_digest), Some(second_digest)) = (digests.first(), digests.get(1)) else {
            continue;
        };
        if first_digest != second_digest {
            return Err(Error::corpus(
                format!("the entry {}", entry.name),
                format!(
                    "generated {first_digest} once and {second_digest} the next time, from \
                     the same seed"
                ),
            ));
        }
        if first_digest != entry.digest {
            return Err(Error::corpus(
                format!("the entry {}", entry.name),
                format!(
                    "generates {first_digest}, and the registry pins {}",
                    entry.digest
                ),
            ));
        }
        println!("{:<34} reproduced  {first_digest}", entry.name);
    }

    let _ = std::fs::remove_dir_all(&scratch);
    Ok(())
}

/// Checks every selected entry that is already cached against the registry.
///
/// # Errors
///
/// Fails when a cached entry is missing or is not the one the registry describes.
pub fn verify(selection: &Selection, layout: &Layout) -> Result<()> {
    for entry in require(selection)? {
        let path = layout.entry(entry);
        if !path.is_file() {
            return Err(Error::corpus(
                format!("the entry {}", entry.name),
                format!("is not cached at {}", path.display()),
            ));
        }
        confirm(entry, &path)?;
        println!("{:<34} verified  {}", entry.name, entry.digest);
    }
    Ok(())
}

/// Reads one cached entry and confirms it is the one the registry describes.
///
/// A measurement reads the bytes it measures through here, so a number can never come from
/// a file that is not the registered entry.
///
/// # Errors
///
/// Fails when the entry is not cached, cannot be read, or is not the one the registry
/// describes.
pub fn load(entry: &Entry, layout: &Layout) -> Result<Vec<u8>> {
    let path = layout.entry(entry);
    if !path.is_file() {
        return Err(Error::corpus(
            format!("the entry {}", entry.name),
            format!(
                "is not cached at {}. Materialize the corpus before measuring on it.",
                path.display()
            ),
        ));
    }
    let data = std::fs::read(&path).map_err(|e| Error::at("read", &path, e))?;
    let bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
    if bytes != entry.bytes {
        return Err(Error::corpus(
            format!("the entry {}", entry.name),
            format!("is {bytes} bytes, and the registry records {}", entry.bytes),
        ));
    }
    let found = digest::bytes(&data);
    if found != entry.digest {
        return Err(Error::corpus(
            format!("the entry {}", entry.name),
            format!("hashes to {found}, and the registry pins {}", entry.digest),
        ));
    }
    Ok(data)
}

/// Prints what the registry holds, without touching the cache.
///
/// Every field a corpus entry must state appears here, because a number measured on an
/// entry is only reproducible if a reader can see where the entry came from, what pins it,
/// and what licenses it.
///
/// # Errors
///
/// Fails when the selection names nothing.
pub fn list(selection: &Selection) -> Result<()> {
    let entries = require(selection)?;
    for entry in &entries {
        println!("{}", entry.name);
        field("group", entry.group.name());
        field("content", entry.content);
        field("class", entry.class().name());
        field("bytes", &entry.bytes.to_string());
        field("digest", entry.digest);
        field("license", entry.license.name);
        field("basis", entry.license.basis);
        match &entry.origin {
            Origin::Fetched(download) => {
                field("source", download.url);
                field("revision", download.revision);
                field(
                    "archive",
                    &format!(
                        "{} of {} bytes, {}",
                        download.archive, download.archive_bytes, download.archive_digest
                    ),
                );
            }
            Origin::Generated(recipe) => {
                field("source", "generated by this repository");
                field(
                    "revision",
                    &format!("{} from seed {:#018x}", recipe.shape.name(), recipe.seed),
                );
            }
        }
        println!();
    }

    println!("size classes");
    for class in CLASSES {
        let covered = entries
            .iter()
            .filter(|entry| entry.class() == *class)
            .count();
        field(class.name(), &format!("{covered} entries"));
    }
    Ok(())
}

fn field(name: &str, value: &str) {
    println!("  {name:<10} {value}");
}

/// The entries a selection names, which is never none.
fn require(selection: &Selection) -> Result<Vec<&'static Entry>> {
    let entries = selection.entries();
    if entries.is_empty() {
        return Err(Error::corpus(
            selection.describe(),
            "names no registered entry",
        ));
    }
    Ok(entries)
}

/// Whether the cache already holds this entry's exact bytes.
fn present(entry: &Entry, path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    match size(path) {
        Ok(bytes) if bytes == entry.bytes => {}
        _ => return false,
    }
    digest::file(path).is_ok_and(|found| found == entry.digest)
}

/// Writes one generated entry from its recorded seed.
fn generate(entry: &Entry, recipe: &Recipe, path: &Path) -> Result<()> {
    parent(path)?;
    let file = File::create(path).map_err(|e| Error::at("create", path, e))?;
    let mut sink = BufWriter::new(file);
    shape::write(recipe.shape, recipe.seed, entry.bytes, &mut sink)
        .map_err(|e| Error::at("write", path, e))?;
    // A buffered writer swallows a failed flush when it is dropped, so the last block of
    // an entry is only on disk once this succeeds.
    sink.flush().map_err(|e| Error::at("write", path, e))?;
    Ok(())
}

/// Obtains one fetched entry: the archive first, then the bytes it unpacks to.
fn fetch(entry: &Entry, download: &Download, layout: &Layout, path: &Path) -> Result<()> {
    let archive = layout.archive(entry, download);
    obtain(entry, download, &archive)?;
    parent(path)?;
    match download.unpack {
        Unpack::Plain => {
            let _ = std::fs::copy(&archive, path).map_err(|e| Error::at("copy", &archive, e))?;
        }
        Unpack::ZipMember(member) => exec::run_into(
            &argv(["unzip", "-p", &archive.display().to_string(), member]),
            layout.root(),
            path,
        )?,
        Unpack::Gzip => exec::run_into(
            &argv(["gzip", "-d", "-c", &archive.display().to_string()]),
            layout.root(),
            path,
        )?,
    }
    Ok(())
}

/// Downloads the archive unless one that matches the registry is already here.
///
/// An archive that does not match is discarded and fetched once more, because a transfer
/// that was interrupted leaves a file that looks like a drifted upstream. A second miss is
/// reported, not retried: at that point the bytes upstream serves are not the recorded ones.
fn obtain(entry: &Entry, download: &Download, archive: &Path) -> Result<()> {
    if archive.is_file() && matches(download, archive)? {
        return Ok(());
    }
    if archive.is_file() {
        std::fs::remove_file(archive).map_err(|e| Error::at("clear", archive, e))?;
    }
    parent(archive)?;
    exec::run(
        &argv([
            "curl",
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--output",
            &archive.display().to_string(),
            download.url,
        ]),
        Path::new("."),
    )?;
    if matches(download, archive)? {
        return Ok(());
    }
    Err(Error::corpus(
        format!("the archive of {}", entry.name),
        format!(
            "is {} of {} bytes, and the registry records {} of {} bytes. The upstream \
             artifact at {} is no longer the one this entry pins.",
            digest::file(archive)?,
            size(archive)?,
            download.archive_digest,
            download.archive_bytes,
            download.url
        ),
    ))
}

/// Whether a downloaded archive is the one the registry records.
fn matches(download: &Download, archive: &Path) -> Result<bool> {
    if size(archive)? != download.archive_bytes {
        return Ok(false);
    }
    Ok(digest::file(archive)? == download.archive_digest)
}

/// Confirms that the bytes now in the cache are the bytes the registry describes.
fn confirm(entry: &Entry, path: &Path) -> Result<()> {
    let bytes = size(path)?;
    if bytes != entry.bytes {
        return Err(Error::corpus(
            format!("the entry {}", entry.name),
            format!("is {bytes} bytes, and the registry records {}", entry.bytes),
        ));
    }
    let found = digest::file(path)?;
    if found != entry.digest {
        return Err(Error::corpus(
            format!("the entry {}", entry.name),
            format!("hashes to {found}, and the registry pins {}", entry.digest),
        ));
    }
    Ok(())
}

fn size(path: &Path) -> Result<u64> {
    std::fs::metadata(path)
        .map(|data| data.len())
        .map_err(|e| Error::at("measure", path, e))
}

fn parent(path: &Path) -> Result<()> {
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(dir).map_err(|e| Error::at("create", dir, e))
}

/// Removes a directory and recreates it, so a generation writes into nothing left behind.
fn reset(dir: &Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| Error::at("clear", dir, e))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| Error::at("create", dir, e))
}

#[cfg(test)]
mod tests {
    use super::{CORPUS_DEFAULT, Layout, build, list, reproduce, verify};
    use crate::registry::{Group, Selection, find};

    fn scratch(name: &str) -> Option<Layout> {
        let root = std::env::temp_dir().join(format!("entroq-bench-corpus-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        Layout::resolve(Some(root)).ok()
    }

    #[test]
    fn a_named_root_wins_over_the_default() {
        let Some(layout) = scratch("root") else {
            return;
        };
        assert!(layout.root().is_absolute());
        assert_ne!(layout.root().to_string_lossy(), CORPUS_DEFAULT);
    }

    #[test]
    fn an_entry_is_cached_under_the_cache_directory() {
        let (Some(layout), Some(entry)) = (scratch("layout"), find("project-json-small")) else {
            return;
        };
        let path = layout.entry(entry);
        assert!(path.ends_with("cache/project-json-small.json"));
    }

    #[test]
    fn a_generated_entry_is_produced_and_matches_the_registry() {
        let (Some(layout), Some(entry)) = (scratch("build"), find("project-json-small")) else {
            return;
        };
        let selection = Selection::Entry(entry.name);
        assert!(build(&selection, &layout).is_ok());
        assert!(verify(&selection, &layout).is_ok());
        assert_eq!(
            std::fs::metadata(layout.entry(entry)).map(|d| d.len()).ok(),
            Some(entry.bytes)
        );
    }

    #[test]
    fn a_measurement_reads_an_entry_only_after_it_matches_the_registry() {
        let (Some(layout), Some(entry)) = (scratch("load"), find("project-zeros-tiny")) else {
            return;
        };
        assert!(super::load(entry, &layout).is_err());
        assert!(build(&Selection::Entry(entry.name), &layout).is_ok());
        let data = super::load(entry, &layout);
        assert_eq!(
            data.ok().map(|bytes| bytes.len()),
            usize::try_from(entry.bytes).ok()
        );
        assert!(std::fs::write(layout.entry(entry), b"not the corpus").is_ok());
        assert!(super::load(entry, &layout).is_err());
    }

    #[test]
    fn a_cached_entry_whose_bytes_changed_is_rejected() {
        let (Some(layout), Some(entry)) = (scratch("tamper"), find("project-json-small")) else {
            return;
        };
        let selection = Selection::Entry(entry.name);
        assert!(build(&selection, &layout).is_ok());
        assert!(std::fs::write(layout.entry(entry), b"not the corpus").is_ok());
        assert!(verify(&selection, &layout).is_err());
    }

    #[test]
    fn a_cached_entry_whose_bytes_changed_is_produced_again() {
        let (Some(layout), Some(entry)) = (scratch("repair"), find("project-sparse-small")) else {
            return;
        };
        let selection = Selection::Entry(entry.name);
        assert!(build(&selection, &layout).is_ok());
        assert!(std::fs::write(layout.entry(entry), b"not the corpus").is_ok());
        assert!(build(&selection, &layout).is_ok());
        assert!(verify(&selection, &layout).is_ok());
    }

    #[test]
    fn a_generated_entry_reproduces_from_its_seed() {
        let Some(layout) = scratch("reproduce") else {
            return;
        };
        assert!(reproduce(&Selection::Entry("project-zeros-tiny"), &layout).is_ok());
    }

    #[test]
    fn a_reproduction_leaves_nothing_behind() {
        let Some(layout) = scratch("reproduce-clean") else {
            return;
        };
        assert!(reproduce(&Selection::Entry("project-json-small"), &layout).is_ok());
        assert!(!layout.root().join(".reproduce").exists());
    }

    #[test]
    fn a_fetched_entry_cannot_be_reproduced_from_a_seed() {
        let Some(layout) = scratch("reproduce-fetched") else {
            return;
        };
        assert!(reproduce(&Selection::Entry("enwik8"), &layout).is_err());
    }

    #[test]
    fn a_selection_that_names_nothing_is_rejected() {
        let Some(layout) = scratch("empty") else {
            return;
        };
        assert!(build(&Selection::Entry("silesia"), &layout).is_err());
        assert!(verify(&Selection::Entry("silesia"), &layout).is_err());
        assert!(list(&Selection::Entry("silesia")).is_err());
    }

    #[test]
    fn an_entry_that_was_never_fetched_does_not_verify() {
        let Some(layout) = scratch("missing") else {
            return;
        };
        assert!(verify(&Selection::Entry("enwik8"), &layout).is_err());
    }

    #[test]
    fn the_registry_lists_without_a_cache() {
        assert!(list(&Selection::All).is_ok());
        assert!(list(&Selection::Group(Group::Project)).is_ok());
    }
}
