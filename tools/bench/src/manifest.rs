//! Owns the manifest that says how one competitor build was produced.
//!
//! A competitor build with no manifest cannot appear in a result, because a number produced
//! by a library nobody can rebuild is not evidence. The manifest therefore carries every
//! fact a rebuild needs and every fact a result must state: the pinned commit, the exact
//! commands, the effective flags, the toolchain, the architecture, and the checksum of each
//! library the build installed.
//!
//! The recorded commands are the rebuild input, not a description of one. They name three
//! variables, so the same commands rebuild into a prefix they have never seen:
//!
//! ```text
//! $SOURCE   the pinned source tree
//! $BUILD    the build tree, which holds no installed artifact
//! $PREFIX   the prefix the install step writes the libraries to
//! ```
//!
//! A checksum here identifies the artifact a result came from. It is not a claim that the
//! build is bit reproducible. `Rebuild` measures that separately and records what it found.

use std::fs;
use std::path::Path;

use crate::compare::Difference;
use crate::error::{Error, Result};

pub const FILE: &str = "MANIFEST";

/// The variable a recorded command uses for the pinned source tree.
pub const SOURCE: &str = "$SOURCE";
/// The variable a recorded command uses for the build tree.
pub const BUILD: &str = "$BUILD";
/// The variable a recorded command uses for the install prefix.
pub const PREFIX: &str = "$PREFIX";

const HEADER: &str = "\
# How this competitor build was produced.
#
# Written by the benchmark tool. Do not edit by hand: a result cites this file, so an
# edited field is a false record.
#
# A `build_command` line is the rebuild input. Its three variables are the pinned source
# tree, the build tree, and the install prefix, so the same commands rebuild into a prefix
# they have never seen.
";

/// Every field that must be populated before a build may appear in a result.
const REQUIRED: &[&str] = &[
    "codec",
    "display_name",
    "version",
    "upstream_source",
    "upstream_commit",
    "obtained",
    "obtain_command",
    "build_command",
    "build_flags_c",
    "build_type",
    "linkage",
    "compiler",
    "build_tool",
    "build_date",
    "architecture",
    "host_os",
    "library",
    "operating_point",
];

/// One competitor build's manifest, in the order it was written.
pub struct Manifest {
    entries: Vec<(String, String)>,
}

impl Manifest {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Records one field. A key may repeat, which is how a list field is written.
    ///
    /// # Errors
    ///
    /// Fails when the value would not survive a round trip through the file.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        if value.trim().is_empty() {
            return Err(Error::lab(format!("the field `{key}`"), "has no value"));
        }
        if value.contains('\n') {
            return Err(Error::lab(
                format!("the field `{key}`"),
                "spans more than one line",
            ));
        }
        self.entries
            .push((String::from(key), String::from(value.trim())));
        Ok(())
    }

    /// Records one command, in the form a rebuild can split back into an argument vector.
    ///
    /// # Errors
    ///
    /// Fails when an argument carries a space, because the rebuild splits on spaces and
    /// would pass that argument as two.
    pub fn command(&mut self, key: &str, argv: &[String]) -> Result<()> {
        if argv.is_empty() {
            return Err(Error::lab(format!("the field `{key}`"), "names no program"));
        }
        for argument in argv {
            if argument.contains(char::is_whitespace) {
                return Err(Error::lab(
                    format!("the field `{key}`"),
                    format!("carries the argument `{argument}`, which contains whitespace"),
                ));
            }
        }
        self.set(key, &argv.join(" "))
    }

    /// The first value recorded for a key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// Every value recorded for a key, in the order they were written.
    pub fn all(&self, key: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    /// Every command recorded for a key, split back into argument vectors, with the three
    /// path variables replaced.
    pub fn commands(&self, key: &str, paths: &Paths<'_>) -> Vec<Vec<String>> {
        self.all(key)
            .iter()
            .map(|line| {
                line.split(' ')
                    .filter(|argument| !argument.is_empty())
                    .map(|argument| paths.expand(argument))
                    .collect()
            })
            .collect()
    }

    /// Every library the build installed, as a prefix-relative path and its digest.
    pub fn libraries(&self) -> Result<Vec<(String, String)>> {
        self.all("library")
            .iter()
            .map(|line| {
                line.rsplit_once(' ')
                    .map(|(path, digest)| (String::from(path.trim()), String::from(digest.trim())))
                    .ok_or_else(|| {
                        Error::lab(
                            "the field `library`",
                            format!("does not carry a path and a digest: `{line}`"),
                        )
                    })
            })
            .collect()
    }

    /// The required fields this manifest does not populate.
    pub fn missing(&self) -> Vec<&'static str> {
        REQUIRED
            .iter()
            .filter(|field| self.get(field).is_none())
            .copied()
            .collect()
    }

    pub fn render(&self) -> String {
        let mut text = String::from(HEADER);
        for (key, value) in &self.entries {
            text.push('\n');
            text.push_str(key);
            text.push_str(": ");
            text.push_str(value);
        }
        text.push('\n');
        text
    }

    /// Writes the manifest beside the build it describes.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be written.
    pub fn write(&self, dir: &Path) -> Result<()> {
        let path = dir.join(FILE);
        fs::write(&path, self.render()).map_err(|e| Error::at("write", &path, e))
    }

    /// Reads the manifest of an existing build.
    ///
    /// # Errors
    ///
    /// Fails when the file is missing, unreadable, or holds a line that is not a field.
    pub fn read(dir: &Path) -> Result<Self> {
        let path = dir.join(FILE);
        let text = fs::read_to_string(&path).map_err(|e| Error::at("read", &path, e))?;
        Self::parse(&text)
    }

    /// Parses a manifest.
    ///
    /// # Errors
    ///
    /// Fails when a line is neither blank, a comment, nor a `key: value` field.
    pub fn parse(text: &str) -> Result<Self> {
        let mut manifest = Self::new();
        for (index, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let number = index.saturating_add(1);
            let (key, value) = trimmed.split_once(':').ok_or_else(|| {
                Error::lab(
                    format!("manifest line {number}"),
                    "is not a `key: value` field",
                )
            })?;
            manifest.set(key.trim(), value)?;
        }
        Ok(manifest)
    }
}

/// The three paths a recorded command names.
pub struct Paths<'a> {
    pub source: &'a Path,
    pub build: &'a Path,
    pub prefix: &'a Path,
}

impl Paths<'_> {
    /// Replaces every path variable in one argument.
    pub fn expand(&self, argument: &str) -> String {
        let mut expanded = String::from(argument);
        for (variable, path) in [
            (SOURCE, self.source),
            (BUILD, self.build),
            (PREFIX, self.prefix),
        ] {
            if expanded.contains(variable) {
                expanded = expanded.replace(variable, &path.display().to_string());
            }
        }
        expanded
    }
}

/// One library, as the pinned build recorded it and as the rebuild produced it.
pub struct Comparison {
    /// The library path, relative to the prefix.
    pub path: String,
    pub recorded: String,
    pub rebuilt: String,
    /// What the two files differ in, when they are not identical.
    pub difference: Option<Difference>,
}

impl Comparison {
    pub const fn identical(&self) -> bool {
        self.difference.is_none()
    }
}

/// What a rebuild found when it compared its libraries against the recorded ones.
pub struct Rebuild {
    pub prefix: String,
    pub commands: Vec<String>,
    pub libraries: Vec<Comparison>,
}

impl Rebuild {
    pub const FILE: &'static str = "REBUILD";

    /// Whether every rebuilt library matched the recorded byte for byte.
    pub fn reproduced(&self) -> bool {
        self.libraries.iter().all(Comparison::identical)
    }

    /// The libraries whose bytes differ from the recorded build.
    pub fn differences(&self) -> Vec<&str> {
        self.libraries
            .iter()
            .filter(|library| !library.identical())
            .map(|library| library.path.as_str())
            .collect()
    }

    pub fn render(&self, when: &str) -> String {
        let mut lines = vec![
            String::from(
                "# What a rebuild from the recorded commands produced, measured against the\n\
                 # build the manifest describes.\n\
                 #\n\
                 # A difference here is a fact about the build, not a failed check. A compiler\n\
                 # and an archiver embed a path, a timestamp, or a build identifier, so two\n\
                 # builds of one source tree can behave the same and not hash the same. The\n\
                 # measured size and differing-byte count say how far apart they are.",
            ),
            String::new(),
            format!("rebuilt_at: {when}"),
            format!("rebuilt_into: {}", self.prefix),
            format!(
                "byte_reproducible: {}",
                if self.reproduced() { "yes" } else { "no" }
            ),
        ];
        lines.extend(
            self.commands
                .iter()
                .map(|command| format!("rebuild_command: {command}")),
        );
        lines.extend(self.libraries.iter().map(|library| {
            let verdict = library.difference.as_ref().map_or_else(
                || String::from("identical"),
                |difference| format!("differs in {}", difference.describe()),
            );
            format!(
                "library: {} {verdict} recorded={} rebuilt={}",
                library.path, library.recorded, library.rebuilt
            )
        }));
        lines.push(String::new());
        lines.join("\n")
    }

    /// Writes the rebuild result beside the manifest it measured.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be written.
    pub fn write(&self, dir: &Path, when: &str) -> Result<()> {
        let path = dir.join(Self::FILE);
        fs::write(&path, self.render(when)).map_err(|e| Error::at("write", &path, e))
    }
}

#[cfg(test)]
mod tests {
    use super::{Comparison, Manifest, Paths, Rebuild};
    use crate::compare::Difference;
    use crate::exec::argv;
    use std::path::Path;

    fn populated() -> Manifest {
        let mut manifest = Manifest::new();
        for (key, value) in [
            ("codec", "lz4"),
            ("display_name", "LZ4"),
            ("version", "v1.10.0"),
            ("upstream_source", "https://github.com/lz4/lz4.git"),
            ("upstream_commit", "ebb370ca"),
            ("obtained", "a shallow fetch of the pinned commit"),
            ("obtain_command", "git init"),
            ("build_flags_c", "-O3 -DNDEBUG"),
            ("build_type", "Release"),
            ("linkage", "static"),
            ("compiler", "Apple clang 21"),
            ("build_tool", "cmake 4.3.4"),
            ("build_date", "2026-09-11T00:00:00Z"),
            ("architecture", "aarch64"),
            ("host_os", "macos"),
            ("library", "lib/liblz4.a sha256:abc"),
            ("operating_point", "fast-1"),
        ] {
            assert!(manifest.set(key, value).is_ok());
        }
        assert!(
            manifest
                .command("build_command", &argv(["cmake", "--build", "$BUILD"]))
                .is_ok()
        );
        manifest
    }

    #[test]
    fn a_populated_manifest_misses_no_required_field() {
        assert!(populated().missing().is_empty());
    }

    #[test]
    fn a_manifest_names_every_field_it_does_not_populate() {
        let mut manifest = Manifest::new();
        assert!(manifest.set("codec", "lz4").is_ok());
        let missing = manifest.missing();
        assert!(!missing.contains(&"codec"));
        assert!(missing.contains(&"upstream_commit"));
        assert!(missing.contains(&"library"));
    }

    #[test]
    fn a_manifest_round_trips_through_the_file_form() {
        let written = populated();
        let read = Manifest::parse(&written.render());
        assert!(read.is_ok());
        let read = read.unwrap_or_else(|_| Manifest::new());
        assert_eq!(read.get("codec"), Some("lz4"));
        assert_eq!(read.get("upstream_commit"), Some("ebb370ca"));
        assert!(read.missing().is_empty());
    }

    #[test]
    fn a_repeated_key_keeps_every_value_in_order() {
        let mut manifest = Manifest::new();
        assert!(manifest.set("operating_point", "q0").is_ok());
        assert!(manifest.set("operating_point", "q11").is_ok());
        assert_eq!(manifest.all("operating_point"), vec!["q0", "q11"]);
        assert_eq!(manifest.get("operating_point"), Some("q0"));
    }

    #[test]
    fn an_empty_or_multiline_value_is_rejected() {
        let mut manifest = Manifest::new();
        assert!(manifest.set("codec", "   ").is_err());
        assert!(manifest.set("codec", "lz4\nzstd").is_err());
    }

    #[test]
    fn a_command_argument_that_carries_whitespace_is_rejected() {
        let mut manifest = Manifest::new();
        let spaced = argv(["cmake", "-DCMAKE_C_FLAGS=-O3 -DNDEBUG"]);
        assert!(manifest.command("build_command", &spaced).is_err());
        assert!(manifest.command("build_command", &[]).is_err());
    }

    #[test]
    fn a_recorded_command_expands_its_three_paths() {
        let mut manifest = Manifest::new();
        let configure = argv([
            "cmake",
            "-S",
            "$SOURCE/build/cmake",
            "-B",
            "$BUILD",
            "-DCMAKE_INSTALL_PREFIX=$PREFIX",
        ]);
        assert!(manifest.command("build_command", &configure).is_ok());
        let commands = manifest.commands(
            "build_command",
            &Paths {
                source: Path::new("/lab/lz4/v1.10.0/source"),
                build: Path::new("/tmp/tree"),
                prefix: Path::new("/tmp/prefix"),
            },
        );
        assert_eq!(
            commands.first().map(Vec::as_slice),
            Some(
                argv([
                    "cmake",
                    "-S",
                    "/lab/lz4/v1.10.0/source/build/cmake",
                    "-B",
                    "/tmp/tree",
                    "-DCMAKE_INSTALL_PREFIX=/tmp/prefix",
                ])
                .as_slice()
            )
        );
    }

    #[test]
    fn a_library_line_splits_into_a_path_and_a_digest() {
        let manifest = populated();
        let libraries = manifest.libraries();
        assert_eq!(
            libraries.ok().as_deref(),
            Some([(String::from("lib/liblz4.a"), String::from("sha256:abc"))].as_slice())
        );
    }

    #[test]
    fn a_library_line_with_no_digest_is_reported() {
        let mut manifest = Manifest::new();
        assert!(manifest.set("library", "lib/liblz4.a").is_ok());
        assert!(manifest.libraries().is_err());
    }

    #[test]
    fn a_comment_and_a_blank_line_are_not_fields() {
        let parsed = Manifest::parse("# a comment\n\ncodec: zlib\n");
        assert!(parsed.is_ok());
        let parsed = parsed.unwrap_or_else(|_| Manifest::new());
        assert_eq!(parsed.get("codec"), Some("zlib"));
        assert_eq!(parsed.all("codec").len(), 1);
    }

    #[test]
    fn a_line_that_is_not_a_field_is_reported_not_skipped() {
        assert!(Manifest::parse("codec zlib\n").is_err());
    }

    #[test]
    fn a_rebuild_that_matches_every_library_reports_byte_reproducible() {
        let rebuild = Rebuild {
            prefix: String::from("/lab/.rebuild/lz4/v1.10.0"),
            commands: vec![String::from("cmake --build /tmp/tree")],
            libraries: vec![Comparison {
                path: String::from("lib/liblz4.a"),
                recorded: String::from("sha256:abc"),
                rebuilt: String::from("sha256:abc"),
                difference: None,
            }],
        };
        assert!(rebuild.reproduced());
        assert!(rebuild.differences().is_empty());
        assert!(
            rebuild
                .render("2026-09-11")
                .contains("byte_reproducible: yes")
        );
    }

    #[test]
    fn a_rebuild_that_differs_names_the_library_both_digests_and_the_measurement() {
        let rebuild = Rebuild {
            prefix: String::from("/lab/.rebuild/lz4/v1.10.0"),
            commands: Vec::new(),
            libraries: vec![Comparison {
                path: String::from("lib/liblz4.a"),
                recorded: String::from("sha256:abc"),
                rebuilt: String::from("sha256:def"),
                difference: Some(Difference {
                    recorded_bytes: 1024,
                    rebuilt_bytes: 1024,
                    differing_bytes: 32,
                    first_offsets: vec![32, 33],
                }),
            }],
        };
        assert!(!rebuild.reproduced());
        assert_eq!(rebuild.differences(), vec!["lib/liblz4.a"]);
        let text = rebuild.render("2026-09-11");
        assert!(text.contains("byte_reproducible: no"));
        assert!(text.contains("recorded=sha256:abc"));
        assert!(text.contains("rebuilt=sha256:def"));
        assert!(text.contains("32 differing"), "{text}");
    }
}
