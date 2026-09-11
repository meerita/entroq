//! Owns the competitor laboratory: where a pinned build lives, how it is produced, and how a
//! rebuild from its own record is measured against it.
//!
//! The laboratory is a build input, not a repository artifact. It holds every compression
//! system Entroq is compared against, pinned to a commit and built with the release
//! configuration each project recommends for itself. A competitor built with weaker
//! optimization than Entroq is not a baseline.
//!
//! A pinned build is never rebuilt in place. Once a version directory carries a manifest,
//! building it again is a no-op, so an old result keeps pointing at the build that produced
//! it. A new build of the same project takes a new version directory.
//!
//! This module does not choose what is measured, and it links nothing it builds.

use std::path::{Path, PathBuf};

use crate::catalog::Codec;
use crate::compare;
use crate::digest;
use crate::error::{Error, Result};
use crate::exec::{self, argv};
use crate::manifest::{BUILD, Comparison, Manifest, PREFIX, Paths, Rebuild, SOURCE};

/// The build input that names the laboratory, when no argument does.
pub const LAB_VARIABLE: &str = "LAB";
/// Where the laboratory sits when neither the argument nor the variable names it.
pub const LAB_DEFAULT: &str = "../lab";

const SOURCE_DIR: &str = "source";
const PREFIX_DIR: &str = "build";
const TREE_ROOT: &str = ".build-tree";
const REBUILD_ROOT: &str = ".rebuild";

/// Where every laboratory path is derived from.
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// Resolves the laboratory root, creating it when it does not exist yet.
    ///
    /// The argument wins, then the `LAB` build input, then the default beside the
    /// repository. The root is made absolute, because a recorded command names it and a
    /// rebuild runs that command from a different working directory.
    ///
    /// # Errors
    ///
    /// Fails when the root cannot be created or cannot be made absolute.
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self> {
        let named = explicit
            .or_else(|| std::env::var_os(LAB_VARIABLE).map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from(LAB_DEFAULT));
        std::fs::create_dir_all(&named).map_err(|e| Error::at("create", &named, e))?;
        let root = named
            .canonicalize()
            .map_err(|e| Error::at("resolve", &named, e))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory one pinned build owns.
    pub fn version_dir(&self, codec: &Codec) -> PathBuf {
        self.root.join(codec.name).join(codec.version)
    }

    fn source(&self, codec: &Codec) -> PathBuf {
        self.version_dir(codec).join(SOURCE_DIR)
    }

    fn prefix(&self, codec: &Codec) -> PathBuf {
        self.version_dir(codec).join(PREFIX_DIR)
    }

    fn tree(&self, codec: &Codec) -> PathBuf {
        self.root
            .join(TREE_ROOT)
            .join(codec.name)
            .join(codec.version)
    }

    fn rebuild_prefix(&self, codec: &Codec) -> PathBuf {
        self.root
            .join(REBUILD_ROOT)
            .join(codec.name)
            .join(codec.version)
            .join(PREFIX_DIR)
    }

    fn rebuild_tree(&self, codec: &Codec) -> PathBuf {
        self.root
            .join(REBUILD_ROOT)
            .join(codec.name)
            .join(codec.version)
            .join(TREE_ROOT)
    }
}

/// Builds one competitor into the laboratory, unless it is already built.
///
/// A version directory that already carries a manifest is left exactly as it is. Its result
/// history points at those bytes.
///
/// # Errors
///
/// Fails when the source cannot be fetched at its pinned commit, a build step fails, the
/// install produces a library the catalog does not name, or the manifest cannot be written.
pub fn build(codec: &Codec, layout: &Layout) -> Result<()> {
    let version_dir = layout.version_dir(codec);
    if version_dir.join(crate::manifest::FILE).is_file() {
        println!(
            "{} {} is already built at {}",
            codec.display,
            codec.version,
            version_dir.display()
        );
        return Ok(());
    }

    std::fs::create_dir_all(&version_dir).map_err(|e| Error::at("create", &version_dir, e))?;
    let source = layout.source(codec);
    let obtain = fetch(codec, &source)?;

    let prefix = layout.prefix(codec);
    let tree = layout.tree(codec);
    reset(&prefix)?;
    reset(&tree)?;

    let recorded = recipe(codec);
    let paths = Paths {
        source: &source,
        build: &tree,
        prefix: &prefix,
    };
    for command in &recorded {
        let expanded: Vec<String> = command
            .iter()
            .map(|argument| paths.expand(argument))
            .collect();
        exec::run(&expanded, &version_dir)?;
    }

    let manifest = describe(codec, layout, &obtain, &recorded, &tree, &prefix)?;
    manifest.write(&version_dir)?;

    // The build tree holds object files no result cites. Only the prefix and the record stay.
    let _ = std::fs::remove_dir_all(&tree);

    println!(
        "{} {} built into {}",
        codec.display,
        codec.version,
        version_dir.display()
    );
    Ok(())
}

/// Rebuilds one competitor from its own manifest and measures the result against it.
///
/// The rebuild runs the recorded commands, not a fresh derivation of them, into a prefix the
/// build has never written to. A byte difference is recorded, not failed: a compiler that
/// embeds a path or a build identifier produces a library that behaves the same and does not
/// hash the same.
///
/// # Errors
///
/// Fails when the manifest is missing a required field, the pinned source has drifted, a
/// recorded command fails, or the rebuild installs no library where the manifest names one.
pub fn verify(codec: &Codec, layout: &Layout) -> Result<()> {
    let version_dir = layout.version_dir(codec);
    let manifest = Manifest::read(&version_dir)?;

    let missing = manifest.missing();
    if !missing.is_empty() {
        return Err(Error::lab(
            format!("the manifest of {} {}", codec.display, codec.version),
            format!("does not populate {}", missing.join(", ")),
        ));
    }

    let source = layout.source(codec);
    confirm_pin(codec, &source, &manifest)?;
    let recorded = confirm_libraries(codec, &layout.prefix(codec), &manifest)?;

    let prefix = layout.rebuild_prefix(codec);
    let tree = layout.rebuild_tree(codec);
    reset(&prefix)?;
    reset(&tree)?;

    let paths = Paths {
        source: &source,
        build: &tree,
        prefix: &prefix,
    };
    let commands = manifest.commands("build_command", &paths);
    for command in &commands {
        exec::run(command, &version_dir)?;
    }

    let mut libraries = Vec::new();
    for (relative, digest) in recorded {
        let rebuilt = prefix.join(&relative);
        if !rebuilt.is_file() {
            return Err(Error::lab(
                format!("the rebuild of {} {}", codec.display, codec.version),
                format!("installed no {relative}, which the manifest names"),
            ));
        }
        libraries.push(Comparison {
            rebuilt: digest::file(&rebuilt)?,
            difference: compare::files(&layout.prefix(codec).join(&relative), &rebuilt)?,
            path: relative,
            recorded: digest,
        });
    }

    let rebuild = Rebuild {
        prefix: prefix.display().to_string(),
        commands: commands.iter().map(|c| exec::describe(c)).collect(),
        libraries,
    };
    rebuild.write(&version_dir, &now()?)?;
    let _ = std::fs::remove_dir_all(&tree);

    report(codec, &rebuild);
    Ok(())
}

/// Fetches the pinned commit, and returns the commands that obtained it.
///
/// A source tree that is already here is checked, not refetched. A tree at another commit is
/// a defect in the laboratory, not something to overwrite.
fn fetch(codec: &Codec, source: &Path) -> Result<Vec<Vec<String>>> {
    let commands = vec![
        argv(["git", "init", "--quiet"]),
        argv(["git", "remote", "add", "origin", codec.upstream]),
        argv([
            "git",
            "fetch",
            "--quiet",
            "--depth",
            "1",
            "origin",
            codec.commit,
        ]),
        argv(["git", "checkout", "--quiet", "--detach", "FETCH_HEAD"]),
    ];
    if source.join(".git").is_dir() {
        confirm_commit(codec, source)?;
        return Ok(commands);
    }
    std::fs::create_dir_all(source).map_err(|e| Error::at("create", source, e))?;
    for command in &commands {
        exec::run(command, source)?;
    }
    confirm_commit(codec, source)?;
    Ok(commands)
}

fn confirm_commit(codec: &Codec, source: &Path) -> Result<()> {
    let head = exec::capture(&argv(["git", "rev-parse", "HEAD"]), source)?;
    if head == codec.commit {
        return Ok(());
    }
    Err(Error::lab(
        format!("the source of {} {}", codec.display, codec.version),
        format!("is at {head}, and this build pins {}", codec.commit),
    ))
}

/// Confirms that the tree a rebuild is about to use is still the pinned one.
fn confirm_pin(codec: &Codec, source: &Path, manifest: &Manifest) -> Result<()> {
    let pinned = manifest.get("upstream_commit").unwrap_or_default();
    let head = exec::capture(&argv(["git", "rev-parse", "HEAD"]), source)?;
    if head != pinned {
        return Err(Error::lab(
            format!("the source of {} {}", codec.display, codec.version),
            format!("is at {head}, and its manifest records {pinned}"),
        ));
    }
    let dirty = exec::capture(&argv(["git", "status", "--porcelain"]), source)?;
    if dirty.is_empty() {
        return Ok(());
    }
    Err(Error::lab(
        format!("the source of {} {}", codec.display, codec.version),
        "carries uncommitted changes, so it is no longer the pinned tree",
    ))
}

/// The commands that configure, build, and install one competitor.
///
/// Every path is a variable, so the same commands rebuild into a prefix they have never
/// seen. `Release` is the configuration each of these projects recommends for itself, and
/// none of them is configured below it.
fn recipe(codec: &Codec) -> Vec<Vec<String>> {
    let source = if codec.source_subdir.is_empty() {
        String::from(SOURCE)
    } else {
        format!("{SOURCE}/{}", codec.source_subdir)
    };
    let mut configure = argv([
        "cmake",
        "-S",
        &source,
        "-B",
        BUILD,
        "-DCMAKE_BUILD_TYPE=Release",
        &format!("-DCMAKE_INSTALL_PREFIX={PREFIX}"),
    ]);
    configure.extend(codec.options.iter().map(|option| String::from(*option)));
    vec![
        configure,
        argv(["cmake", "--build", BUILD, "--config", "Release"]),
        argv(["cmake", "--install", BUILD, "--config", "Release"]),
    ]
}

/// Writes down every fact a result must be able to state about this build.
fn describe(
    codec: &Codec,
    layout: &Layout,
    obtain: &[Vec<String>],
    recorded: &[Vec<String>],
    tree: &Path,
    prefix: &Path,
) -> Result<Manifest> {
    let cache = Cache::read(tree)?;
    let mut manifest = Manifest::new();
    manifest.set("codec", codec.name)?;
    manifest.set("display_name", codec.display)?;
    manifest.set("version", codec.version)?;
    manifest.set("upstream_source", codec.upstream)?;
    manifest.set("upstream_tag", codec.version)?;
    manifest.set("upstream_commit", codec.commit)?;
    manifest.set(
        "obtained",
        "a shallow git fetch of the pinned commit from the upstream repository, checked out \
         detached",
    )?;
    for command in obtain {
        manifest.command("obtain_command", command)?;
    }
    for command in recorded {
        manifest.command("build_command", command)?;
    }
    manifest.set(
        "build_working_dir",
        "the version directory this manifest sits in",
    )?;
    manifest.set("build_type", &cache.require("CMAKE_BUILD_TYPE")?)?;
    manifest.set("build_flags_c", &cache.require("CMAKE_C_FLAGS_RELEASE")?)?;
    if let Some(flags) = cache.get("CMAKE_CXX_FLAGS_RELEASE") {
        manifest.set("build_flags_cxx", &flags)?;
    }
    manifest.set("linkage", "static")?;
    manifest.set("compiler", &compiler(&cache)?)?;
    let cmake = exec::capture(&argv(["cmake", "--version"]), layout.root())?;
    manifest.set("build_tool", cmake.lines().next().unwrap_or("unknown"))?;
    manifest.set("build_date", &now()?)?;
    manifest.set("architecture", std::env::consts::ARCH)?;
    manifest.set("host_os", std::env::consts::OS)?;
    manifest.set(
        "host_kernel",
        &exec::capture(&argv(["uname", "-sr"]), layout.root())?,
    )?;

    for relative in codec.libraries {
        let path = prefix.join(relative);
        if !path.is_file() {
            return Err(Error::lab(
                format!("the build of {} {}", codec.display, codec.version),
                format!("installed no {relative}, which the catalog names"),
            ));
        }
        manifest.set("library", &format!("{relative} {}", digest::file(&path)?))?;
    }
    manifest.set("library_root", PREFIX_DIR)?;

    for point in codec.operating_points {
        manifest.set("operating_point", point)?;
    }
    if let Some(note) = codec.format_note {
        manifest.set("format_note", note)?;
    }
    Ok(manifest)
}

/// The compiler the build actually used, as it describes itself.
fn compiler(cache: &Cache) -> Result<String> {
    let path = cache.require("CMAKE_C_COMPILER")?;
    let version = exec::capture(&argv([path.as_str(), "--version"]), Path::new("."))?;
    let first = version.lines().next().unwrap_or("unknown");
    Ok(format!("{first} at {path}"))
}

/// The configured values `CMake` wrote down, which are the flags the build really used.
struct Cache {
    entries: Vec<(String, String)>,
}

impl Cache {
    fn read(tree: &Path) -> Result<Self> {
        let path = tree.join("CMakeCache.txt");
        let text = std::fs::read_to_string(&path).map_err(|e| Error::at("read", &path, e))?;
        let mut entries = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
                continue;
            }
            if let Some((name, value)) = trimmed.split_once('=')
                && let Some((key, _)) = name.split_once(':')
            {
                entries.push((String::from(key), String::from(value.trim())));
            }
        }
        Ok(Self { entries })
    }

    fn get(&self, key: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
            .filter(|value| !value.is_empty())
    }

    fn require(&self, key: &str) -> Result<String> {
        self.get(key).ok_or_else(|| {
            Error::lab(
                "the build configuration",
                format!("records no {key}, so the manifest cannot state it"),
            )
        })
    }
}

/// Confirms that the pinned build is still the one its manifest describes.
///
/// A rebuild compares against these bytes, so a library that no longer matches its recorded
/// digest makes the comparison meaningless.
fn confirm_libraries(
    codec: &Codec,
    prefix: &Path,
    manifest: &Manifest,
) -> Result<Vec<(String, String)>> {
    let libraries = manifest.libraries()?;
    for (relative, recorded) in &libraries {
        let path = prefix.join(relative);
        if !path.is_file() {
            return Err(Error::lab(
                format!("the build of {} {}", codec.display, codec.version),
                format!("no longer holds {relative}, which its manifest names"),
            ));
        }
        let found = digest::file(&path)?;
        if &found != recorded {
            return Err(Error::lab(
                format!("the build of {} {}", codec.display, codec.version),
                format!("holds a {relative} that is not the one its manifest records"),
            ));
        }
    }
    Ok(libraries)
}

fn report(codec: &Codec, rebuild: &Rebuild) {
    println!(
        "{} {} rebuilt into {}",
        codec.display, codec.version, rebuild.prefix
    );
    for library in &rebuild.libraries {
        let verdict = library.difference.as_ref().map_or_else(
            || String::from("identical to the recorded build"),
            |difference| format!("differs in {}", difference.describe()),
        );
        println!("  {}  {verdict}", library.path);
    }
    if rebuild.reproduced() {
        println!("  byte reproducible: yes");
    } else {
        println!(
            "  byte reproducible: no, for {}. The rebuild succeeded from the recorded \
             commands alone; the manifest checksum identifies the artifact a result came \
             from, and does not claim the build is bit reproducible.",
            rebuild.differences().join(", ")
        );
    }
}

/// Removes a directory and recreates it, so a step writes into nothing left behind.
fn reset(dir: &Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| Error::at("clear", dir, e))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| Error::at("create", dir, e))
}

fn now() -> Result<String> {
    crate::environment::timestamp()
}

#[cfg(test)]
mod tests {
    use super::{LAB_DEFAULT, Layout, recipe};
    use crate::catalog::find;
    use crate::manifest::{BUILD, PREFIX, SOURCE};

    #[test]
    fn a_recipe_configures_builds_and_installs() {
        let codec = find("lz4");
        assert!(codec.is_some());
        let Some(codec) = codec else { return };
        let commands = recipe(codec);
        assert_eq!(commands.len(), 3);
        assert!(
            commands
                .iter()
                .all(|c| c.first().map(String::as_str) == Some("cmake"))
        );
    }

    #[test]
    fn a_recipe_names_every_path_as_a_variable() {
        for codec in crate::catalog::CODECS {
            for command in recipe(codec) {
                for argument in command {
                    assert!(
                        !argument.starts_with('/'),
                        "{} records an absolute path: {argument}",
                        codec.name
                    );
                }
            }
        }
    }

    #[test]
    fn a_recipe_builds_at_the_release_configuration_and_installs_to_the_prefix() {
        for codec in crate::catalog::CODECS {
            let commands = recipe(codec);
            let configure = commands.first().map(Vec::as_slice).unwrap_or_default();
            assert!(
                configure.iter().any(|a| a == "-DCMAKE_BUILD_TYPE=Release"),
                "{} is not configured for release",
                codec.name
            );
            assert!(
                configure
                    .iter()
                    .any(|a| a == &format!("-DCMAKE_INSTALL_PREFIX={PREFIX}")),
                "{} does not install to the prefix",
                codec.name
            );
            assert!(
                configure.iter().any(|a| a.contains(SOURCE)),
                "{} does not build the pinned source",
                codec.name
            );
            assert!(
                commands
                    .iter()
                    .skip(1)
                    .all(|c| c.iter().any(|a| a == BUILD)),
                "{} does not build the recorded tree",
                codec.name
            );
        }
    }

    #[test]
    fn a_recipe_carries_every_option_the_catalog_pins() {
        for codec in crate::catalog::CODECS {
            let commands = recipe(codec);
            let configure = commands.first().map(Vec::as_slice).unwrap_or_default();
            for option in codec.options {
                assert!(
                    configure.iter().any(|a| a == option),
                    "{} drops {option}",
                    codec.name
                );
            }
        }
    }

    #[test]
    fn a_named_root_wins_over_the_default() {
        let named = std::env::temp_dir().join("entroq-bench-lab-root");
        let layout = Layout::resolve(Some(named));
        assert!(layout.is_ok());
        let Ok(layout) = layout else { return };
        assert!(layout.root().is_absolute());
        assert_ne!(layout.root().to_string_lossy(), LAB_DEFAULT);
    }

    #[test]
    fn a_version_directory_is_named_by_the_codec_and_its_version() {
        let named = std::env::temp_dir().join("entroq-bench-lab-layout");
        let layout = Layout::resolve(Some(named));
        let codec = find("zstd");
        let (Ok(layout), Some(codec)) = (layout, codec) else {
            return;
        };
        let dir = layout.version_dir(codec);
        assert!(dir.ends_with("zstd/v1.5.7"));
    }
}
