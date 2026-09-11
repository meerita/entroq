//! Links the competitor laboratory into the benchmark tool, and records what it linked.
//!
//! The laboratory is a build input produced outside this repository. This script reads the
//! manifest of every pinned competitor, points the linker at the exact static archive that
//! manifest names, and hands the binary the checksum of each archive it linked, so a result
//! can state the artifact it was produced with instead of asserting it.
//!
//! Linking is all or nothing. A laboratory that does not hold every competitor, or that
//! holds one whose bytes no longer match its manifest, is not linked at all. The tool then
//! measures nothing and says why. It never links part of a set, because a result missing a
//! competitor for a reason nobody recorded is worse than no result.
//!
//! An unbuilt laboratory is the normal state of a clean host: this same tool is what builds
//! it. So a missing laboratory is not an error here. It leaves `lab_linked` unset.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

// The pinned competitor set is read from the module that owns it, so the laboratory build
// and the linker cannot disagree about which competitors exist.
// The link step reads the pinned set and the library list. The lookup helpers the tool
// itself uses are unreachable from here, and removing them from the owning module to
// satisfy this script would be the wrong direction of ownership.
#[allow(dead_code)]
#[path = "src/catalog.rs"]
mod catalog;

use catalog::{CODECS, Codec};

/// The build input that names the laboratory, when the environment does not.
const LAB_DEFAULT: &str = "../lab";
/// The manifest file the laboratory build writes beside each competitor.
const MANIFEST: &str = "MANIFEST";
/// The manifest field that names the prefix directory, relative to the version directory.
const LIBRARY_ROOT: &str = "library_root";
/// The manifest field that names one installed library and its digest.
const LIBRARY: &str = "library";

/// One library the linker was pointed at.
struct Linked {
    codec: &'static str,
    version: &'static str,
    /// The path relative to the prefix, which is what the manifest names.
    relative: String,
    absolute: PathBuf,
    /// The digest of the file this build read, which the manifest also records.
    digest: String,
}

fn main() {
    // The cfg is declared whether or not it is set, so the unexpected-cfg lint stays quiet
    // on a host with no laboratory.
    println!("cargo::rustc-check-cfg=cfg(lab_linked)");
    println!("cargo::rerun-if-changed=src/catalog.rs");
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=LAB");

    let Some(root) = laboratory() else { return };

    let mut linked: Vec<Linked> = Vec::new();
    for codec in CODECS {
        let version_dir = root.join(codec.name).join(codec.version);
        println!(
            "cargo::rerun-if-changed={}",
            version_dir.join(MANIFEST).display()
        );
        match resolve(codec, &version_dir) {
            Some(libraries) => linked.extend(libraries),
            None => return,
        }
    }

    emit(&linked);
}

/// Where the laboratory sits, as an absolute path.
///
/// `LAB` names it, and the default sits beside the repository. Either may be relative, and a
/// build script runs in its own package directory rather than the repository root, so a
/// relative path is resolved against the repository root and not against the current
/// directory. That is the same root the tool itself is invoked from.
fn laboratory() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
    // tools/bench -> tools -> the repository root.
    let repository = manifest_dir.ancestors().nth(2)?;
    let named = std::env::var_os("LAB").map_or_else(|| PathBuf::from(LAB_DEFAULT), PathBuf::from);
    let resolved = if named.is_absolute() {
        named
    } else {
        repository.join(named)
    };
    resolved.canonicalize().ok()
}

/// The libraries one competitor's manifest names, when every one of them is present and is
/// the artifact the manifest recorded.
fn resolve(codec: &Codec, version_dir: &Path) -> Option<Vec<Linked>> {
    let text = std::fs::read_to_string(version_dir.join(MANIFEST)).ok()?;
    let prefix = version_dir.join(field(&text, LIBRARY_ROOT)?);

    let mut libraries = Vec::new();
    for line in values(&text, LIBRARY) {
        let (relative, recorded) = line.rsplit_once(' ')?;
        let absolute = prefix.join(relative.trim());
        let found = digest(&absolute)?;
        if found != recorded.trim() {
            return None;
        }
        libraries.push(Linked {
            codec: codec.name,
            version: codec.version,
            relative: String::from(relative.trim()),
            absolute,
            digest: found,
        });
    }
    // A manifest that names no library describes a build that installed nothing.
    if libraries.len() == codec.libraries.len() && !libraries.is_empty() {
        Some(libraries)
    } else {
        None
    }
}

/// Points the linker at each archive and hands the binary what it linked.
fn emit(linked: &[Linked]) {
    let mut directories: Vec<&Path> = Vec::new();
    for library in linked {
        if let Some(dir) = library.absolute.parent()
            && !directories.contains(&dir)
        {
            directories.push(dir);
            println!("cargo::rustc-link-search=native={}", dir.display());
        }
    }

    // A static archive resolves symbols for whatever precedes it, so a library that depends
    // on another is named first. The catalog lists a set from its base upward.
    for library in linked.iter().rev() {
        if let Some(name) = archive_name(&library.relative) {
            println!("cargo::rustc-link-lib=static={name}");
        }
    }

    // Snappy is C++. Its C entry points are `extern "C"`, and the code behind them is not,
    // so the C++ runtime has to come from the host.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "macos" || target_os == "ios" {
        println!("cargo::rustc-link-lib=dylib=c++");
    } else {
        println!("cargo::rustc-link-lib=dylib=stdc++");
    }

    let mut manifest = String::new();
    for library in linked {
        let _ = write!(
            manifest,
            "{}|{}|{}|{}|{};",
            library.codec,
            library.version,
            library.relative,
            library.absolute.display(),
            library.digest,
        );
    }
    println!("cargo::rustc-env=ENTROQ_LAB_LIBRARIES={manifest}");
    println!("cargo::rustc-cfg=lab_linked");
}

/// The name `-l` takes: `lib/libzstd.a` links as `zstd`.
fn archive_name(relative: &str) -> Option<&str> {
    relative
        .rsplit('/')
        .next()?
        .strip_prefix("lib")?
        .strip_suffix(".a")
}

/// The first value a manifest records for a field.
fn field(text: &str, key: &str) -> Option<String> {
    values(text, key).into_iter().next().map(String::from)
}

/// Every value a manifest records for a field, in the order they were written.
fn values<'a>(text: &'a str, key: &str) -> Vec<&'a str> {
    text.lines()
        .filter_map(|line| line.trim().split_once(':'))
        .filter(|(name, _)| name.trim() == key)
        .map(|(_, value)| value.trim())
        .collect()
}

fn digest(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(format!("sha256:{:x}", hasher.finalize()))
}
