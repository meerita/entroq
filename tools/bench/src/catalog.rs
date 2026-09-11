//! Owns the pinned competitor set: which compression systems Entroq is measured against,
//! at which revision, built how, and at which operating points.
//!
//! A competitor is pinned to the commit behind its release tag, never to the tag. A tag can
//! move. A result that names a tag cannot say what it measured.
//!
//! The operating points of a competitor are grouped, because the cost of one point spans
//! three orders of magnitude inside one project: Brotli q11 compresses a megabyte for what
//! q0 spends on a hundred. A group is what one segment covers, so a slow point is measured
//! beside points of its own cost rather than beside the cheap ones it would push over the
//! segment budget. The grouping decides what fits a budget. It does not decide what is
//! measured: every pinned point belongs to exactly one group.
//!
//! This module does not own the laboratory layout, the manifest, or the build. It owns only
//! what is true of each competitor before anything is fetched.

/// A set of operating points that one segment covers.
///
/// The name is what a segment identifier carries after the codec name, so it is stable
/// across runs and across a resume.
pub struct PointGroup {
    pub name: &'static str,
    pub points: &'static [&'static str],
}

/// One pinned competitor build.
pub struct Codec {
    /// The identifier a segment, a directory, and a result use.
    pub name: &'static str,
    /// The name the project publishes for itself.
    pub display: &'static str,
    /// The release tag, which is also the version directory name.
    pub version: &'static str,
    pub upstream: &'static str,
    /// The commit behind `version`, resolved from upstream and pinned here.
    pub commit: &'static str,
    /// Where the build description lives, relative to the fetched source root.
    pub source_subdir: &'static str,
    /// The configuration arguments this project recommends for a static release build.
    pub options: &'static [&'static str],
    /// Every library the install step produces, relative to the prefix.
    pub libraries: &'static [&'static str],
    /// The operating points a comparison covers, not only the default level, grouped by
    /// what one segment can afford to measure together.
    pub point_groups: &'static [PointGroup],
    /// The point this project's own interface selects when a caller states no level.
    ///
    /// A tier that measures direction rather than a frontier measures this one, so a cheap
    /// run still compares what a caller who states nothing actually gets.
    pub default_point: &'static str,
    /// A fact about the format a result must state, when the project publishes more than one.
    pub format_note: Option<&'static str>,
}

const LZ4: Codec = Codec {
    name: "lz4",
    display: "LZ4",
    version: "v1.10.0",
    upstream: "https://github.com/lz4/lz4.git",
    commit: "ebb370ca83af193212df4dcbadcc5d87bc0de2f0",
    source_subdir: "build/cmake",
    options: &[
        "-DBUILD_SHARED_LIBS=OFF",
        "-DBUILD_STATIC_LIBS=ON",
        "-DLZ4_BUILD_CLI=OFF",
    ],
    libraries: &["lib/liblz4.a"],
    point_groups: &[
        PointGroup {
            name: "fast",
            points: &["fast-1", "fast-3", "fast-5", "fast-9"],
        },
        PointGroup {
            name: "hc",
            points: &["hc-1", "hc-4", "hc-9", "hc-12"],
        },
    ],
    default_point: "fast-1",
    format_note: None,
};

const ZSTD: Codec = Codec {
    name: "zstd",
    display: "Zstandard",
    version: "v1.5.7",
    upstream: "https://github.com/facebook/zstd.git",
    commit: "f8745da6ff1ad1e7bab384bd1f9d742439278e99",
    source_subdir: "build/cmake",
    options: &[
        "-DZSTD_BUILD_SHARED=OFF",
        "-DZSTD_BUILD_STATIC=ON",
        "-DZSTD_BUILD_PROGRAMS=OFF",
        "-DZSTD_BUILD_TESTS=OFF",
    ],
    libraries: &["lib/libzstd.a"],
    point_groups: &[
        PointGroup {
            name: "low",
            points: &["level-1", "level-3", "level-6", "level-9"],
        },
        PointGroup {
            name: "high",
            points: &["level-12", "level-15", "level-19"],
        },
        PointGroup {
            name: "max",
            points: &["level-22"],
        },
    ],
    default_point: "level-3",
    format_note: None,
};

const BROTLI: Codec = Codec {
    name: "brotli",
    display: "Brotli",
    version: "v1.2.0",
    upstream: "https://github.com/google/brotli.git",
    commit: "028fb5a23661f123017c060daa546b55cf4bde29",
    source_subdir: "",
    options: &[
        "-DBUILD_SHARED_LIBS=OFF",
        "-DBROTLI_BUILD_TOOLS=OFF",
        "-DBROTLI_DISABLE_TESTS=ON",
    ],
    libraries: &[
        "lib/libbrotlicommon.a",
        "lib/libbrotlidec.a",
        "lib/libbrotlienc.a",
    ],
    point_groups: &[
        PointGroup {
            name: "low",
            points: &["q0", "q2", "q5"],
        },
        PointGroup {
            name: "high",
            points: &["q9"],
        },
        PointGroup {
            name: "max",
            points: &["q11"],
        },
    ],
    default_point: "q11",
    format_note: None,
};

const SNAPPY: Codec = Codec {
    name: "snappy",
    display: "Snappy",
    version: "1.2.2",
    upstream: "https://github.com/google/snappy.git",
    commit: "6af9287fbdb913f0794d0148c6aa43b58e63c8e3",
    source_subdir: "",
    options: &[
        "-DBUILD_SHARED_LIBS=OFF",
        "-DSNAPPY_BUILD_TESTS=OFF",
        "-DSNAPPY_BUILD_BENCHMARKS=OFF",
    ],
    libraries: &["lib/libsnappy.a"],
    point_groups: &[PointGroup {
        name: "default",
        points: &["default"],
    }],
    default_point: "default",
    format_note: Some(
        "Snappy defines a block format and a framing format, and the two are not \
         interchangeable. This build measures the block format, which is what the C++ library \
         header publishes through Compress and Uncompress. The framing format carries a stream \
         header and per-chunk checksums that the block format does not, so a result from one \
         says nothing about the other.",
    ),
};

const ZLIB: Codec = Codec {
    name: "zlib",
    display: "zlib",
    version: "v1.3.2",
    upstream: "https://github.com/madler/zlib.git",
    commit: "da607da739fa6047df13e66a2af6b8bec7c2a498",
    source_subdir: "",
    options: &[
        "-DZLIB_BUILD_SHARED=OFF",
        "-DZLIB_BUILD_STATIC=ON",
        "-DZLIB_BUILD_TESTING=OFF",
    ],
    libraries: &["lib/libz.a"],
    point_groups: &[
        PointGroup {
            name: "low",
            points: &["level-1", "level-6"],
        },
        PointGroup {
            name: "high",
            points: &["level-9"],
        },
    ],
    default_point: "level-6",
    format_note: None,
};

/// Every competitor the laboratory holds.
pub const CODECS: &[Codec] = &[LZ4, ZSTD, BROTLI, SNAPPY, ZLIB];

impl Codec {
    /// Every operating point this competitor is measured at, in group order.
    pub fn operating_points(&self) -> Vec<&'static str> {
        self.point_groups
            .iter()
            .flat_map(|group| group.points.iter().copied())
            .collect()
    }

    /// The group a `--points` argument names.
    pub fn group(&self, name: &str) -> Option<&'static PointGroup> {
        self.point_groups.iter().find(|group| group.name == name)
    }

    /// Every group name, in catalog order, for a usage message.
    pub fn group_names(&self) -> String {
        self.point_groups
            .iter()
            .map(|group| group.name)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Finds a competitor by the name a segment or a result uses.
pub fn find(name: &str) -> Option<&'static Codec> {
    CODECS.iter().find(|codec| codec.name == name)
}

/// Every competitor name, in catalog order, for a usage message.
pub fn names() -> String {
    CODECS
        .iter()
        .map(|codec| codec.name)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::{CODECS, find, names};

    #[test]
    fn the_five_required_competitors_are_present() {
        for name in ["lz4", "zstd", "brotli", "snappy", "zlib"] {
            assert!(find(name).is_some(), "`{name}` is not in the catalog");
        }
    }

    #[test]
    fn a_codec_nobody_pinned_is_not_found() {
        assert!(find("lzma").is_none());
        assert!(find("").is_none());
    }

    #[test]
    fn every_name_is_unique() {
        let mut names: Vec<&str> = CODECS.iter().map(|codec| codec.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn every_codec_pins_a_full_commit() {
        for codec in CODECS {
            assert_eq!(codec.commit.len(), 40, "{} pins no commit", codec.name);
            assert!(
                codec.commit.chars().all(|c| c.is_ascii_hexdigit()),
                "{} pins something that is not a commit",
                codec.name
            );
        }
    }

    #[test]
    fn every_codec_names_the_point_its_own_project_defaults_to() {
        for codec in CODECS {
            assert!(
                codec.operating_points().contains(&codec.default_point),
                "{} defaults to {}, which is not one of its points",
                codec.name,
                codec.default_point
            );
        }
    }

    #[test]
    fn every_codec_names_a_library_and_more_than_a_default_level() {
        for codec in CODECS {
            assert!(!codec.libraries.is_empty(), "{} builds nothing", codec.name);
            assert!(
                !codec.operating_points().is_empty(),
                "{} measures nothing",
                codec.name
            );
        }
    }

    #[test]
    fn a_project_that_publishes_two_formats_names_the_one_measured() {
        let snappy = find("snappy");
        assert!(snappy.and_then(|codec| codec.format_note).is_some());
    }

    #[test]
    fn every_configuration_argument_builds_a_static_library_only() {
        for codec in CODECS {
            let shared = codec
                .options
                .iter()
                .any(|option| option.contains("SHARED") && option.ends_with("=OFF"));
            assert!(shared, "{} does not disable its shared library", codec.name);
        }
    }

    #[test]
    fn every_point_belongs_to_exactly_one_group() {
        for codec in CODECS {
            let mut points = codec.operating_points();
            let covered = points.len();
            points.sort_unstable();
            points.dedup();
            assert_eq!(
                points.len(),
                covered,
                "{} measures a point in two groups",
                codec.name
            );
        }
    }

    #[test]
    fn every_group_is_named_once_and_measures_something() {
        for codec in CODECS {
            let mut names: Vec<&str> = codec.point_groups.iter().map(|g| g.name).collect();
            let count = names.len();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), count, "{} names a group twice", codec.name);
            for group in codec.point_groups {
                assert!(
                    !group.points.is_empty(),
                    "{} group {} measures nothing",
                    codec.name,
                    group.name
                );
            }
        }
    }

    #[test]
    fn a_group_nobody_pinned_is_not_found() {
        let Some(zstd) = find("zstd") else {
            unreachable!("zstd is pinned in the catalog")
        };
        assert!(zstd.group("low").is_some());
        assert!(zstd.group("fast").is_none());
        assert!(zstd.group_names().contains("max"));
    }

    #[test]
    fn the_usage_message_lists_every_codec() {
        let listed = names();
        for codec in CODECS {
            assert!(listed.contains(codec.name));
        }
    }
}
