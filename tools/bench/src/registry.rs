//! Owns the corpus registry: which corpus entries exist, where each one comes from, how
//! large it is, what licenses it, and which size class it falls in.
//!
//! The registry is code. The corpus bytes are not: nothing here is committed except the
//! description of how an entry is obtained, and the checksum that says the obtained bytes
//! are the ones this registry means. A corpus only its author can obtain produces a result
//! nobody can reproduce.
//!
//! An entry is registered only when its license is established, either because the
//! distribution states one or because the distributor documents a provenance chain to a
//! licensed source. Two widely used compression corpora are absent for that reason: neither
//! the Silesia corpus nor the Canterbury corpus states a license anywhere in its
//! distribution, and their contents mix material whose terms cannot be traced, including
//! unattributed medical images, vendor binaries, and a spreadsheet of unstated origin. An
//! entry registered with a guessed license would put a redistribution claim nobody checked
//! behind every number measured on it.
//!
//! This module does not own the cache layout, the fetch, or the generator.

use crate::shape::Shape;

/// The size classes a benchmark tier selects by.
///
/// The intervals are half open, so every byte count falls in exactly one class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SizeClass {
    /// Under 1 KiB.
    Tiny,
    /// 1 KiB up to 64 KiB.
    Small,
    /// 64 KiB up to 4 MiB.
    Medium,
    /// 4 MiB up to 100 MiB.
    Large,
    /// 100 MiB and above, which is streamed and measured at the publication tier only.
    Huge,
}

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

impl SizeClass {
    /// The class a byte count falls in.
    pub const fn of(bytes: u64) -> Self {
        if bytes < KIB {
            Self::Tiny
        } else if bytes < 64 * KIB {
            Self::Small
        } else if bytes < 4 * MIB {
            Self::Medium
        } else if bytes < 100 * MIB {
            Self::Large
        } else {
            Self::Huge
        }
    }

    /// The class a `--class` argument names.
    pub fn parse(name: &str) -> Option<Self> {
        CLASSES.iter().copied().find(|class| class.name() == name)
    }

    /// Every class name, in increasing order, for a usage message.
    pub fn names() -> String {
        CLASSES
            .iter()
            .map(|class| class.name())
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Tiny => "tiny",
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
            Self::Huge => "huge",
        }
    }
}

/// Every class, in increasing order.
pub const CLASSES: &[SizeClass] = &[
    SizeClass::Tiny,
    SizeClass::Small,
    SizeClass::Medium,
    SizeClass::Large,
    SizeClass::Huge,
];

/// A set of entries that one segment materializes.
///
/// A group is the unit a segment covers, so a group whose fetch would not fit the segment
/// budget is split until it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// The deterministic project corpus, generated from recorded seeds.
    Project,
    /// The English Wikipedia snapshot used across the compression literature.
    Enwik,
    /// A pinned source tree, as the single archive a project ships it in.
    SourceCode,
    /// Public-domain English literature, as distributed by Project Gutenberg.
    Gutenberg,
}

impl Group {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Enwik => "enwik",
            Self::SourceCode => "sourcecode",
            Self::Gutenberg => "gutenberg",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        GROUPS.iter().copied().find(|group| group.name() == name)
    }
}

pub const GROUPS: &[Group] = &[
    Group::Project,
    Group::Enwik,
    Group::SourceCode,
    Group::Gutenberg,
];

/// What licenses an entry, and how that was established.
pub struct License {
    /// The license identifier, as the licensor publishes it.
    pub name: &'static str,
    /// What established it. A registered entry always has one.
    pub basis: &'static str,
}

/// How a fetched archive becomes the entry's bytes.
#[derive(Clone, Copy)]
pub enum Unpack {
    /// The downloaded file is the entry.
    Plain,
    /// One named member of a zip archive is the entry.
    ZipMember(&'static str),
    /// The gzip stream decodes to the entry.
    Gzip,
}

/// An entry obtained from an upstream distributor.
pub struct Download {
    pub url: &'static str,
    /// What pins the upstream artifact: a release, a tag, or a dated snapshot.
    pub revision: &'static str,
    /// The file name the archive is cached under.
    pub archive: &'static str,
    pub archive_bytes: u64,
    pub archive_digest: &'static str,
    pub unpack: Unpack,
}

/// An entry this repository produces from a recorded seed.
pub struct Recipe {
    pub shape: Shape,
    pub seed: u64,
}

pub enum Origin {
    Fetched(Download),
    Generated(Recipe),
}

/// One corpus entry: one file, obtained one way, with one digest.
pub struct Entry {
    /// The identifier a segment, a cache file, and a result use.
    pub name: &'static str,
    pub group: Group,
    /// What the data is, in the terms a corpus requirement states.
    pub content: &'static str,
    /// The size of the entry, which decides its class.
    pub bytes: u64,
    /// The digest of the entry's bytes, in the `sha256:<hex>` form.
    pub digest: &'static str,
    pub license: License,
    pub origin: Origin,
}

impl Entry {
    pub const fn class(&self) -> SizeClass {
        SizeClass::of(self.bytes)
    }

    /// The file name the entry is cached under.
    pub fn file(&self) -> String {
        match &self.origin {
            Origin::Generated(recipe) => format!("{}.{}", self.name, recipe.shape.extension()),
            Origin::Fetched(_) => String::from(self.name),
        }
    }
}

/// Builds one generated entry, so the table below states only what differs between them.
macro_rules! generated {
    ($name:literal, $shape:expr, $seed:literal, $bytes:expr, $digest:literal) => {
        Entry {
            name: $name,
            group: Group::Project,
            content: $shape.name(),
            bytes: $bytes,
            digest: $digest,
            license: License {
                name: "MIT OR Apache-2.0",
                basis: "produced by this repository, and licensed as this repository is",
            },
            origin: Origin::Generated(Recipe {
                shape: $shape,
                seed: $seed,
            }),
        }
    };
}

/// The English Wikipedia snapshot the compression literature measures on.
///
/// The distributor states no license, and documents the provenance that establishes one:
/// the file is the first 100,000,000 bytes of the English Wikipedia article dump taken on
/// 3 March 2006, and Wikimedia licensed English Wikipedia article text under the GNU Free
/// Documentation License 1.2 on that date.
const ENWIK8: Entry = Entry {
    name: "enwik8",
    group: Group::Enwik,
    content: "English Wikipedia article text, as XML",
    bytes: 100_000_000,
    digest: "sha256:2b49720ec4d78c3c9fabaee6e4179a5e997302b3a70029f30f2d582218c024a8",
    license: License {
        name: "GFDL-1.2-only",
        basis: "the first 100,000,000 bytes of the English Wikipedia article dump of \
                2006-03-03, whose article text Wikimedia licensed under the GNU Free \
                Documentation License 1.2 at that date",
    },
    origin: Origin::Fetched(Download {
        url: "http://mattmahoney.net/dc/enwik8.zip",
        revision: "English Wikipedia dump of 2006-03-03, first 10^8 bytes",
        archive: "enwik8.zip",
        archive_bytes: 36_445_475,
        archive_digest: "sha256:547994d9980ebed1288380d652999f38a14fe291a6247c157c3d33d4932534bc",
        unpack: Unpack::ZipMember("enwik8"),
    }),
};

/// A pinned source tree, as the single archive its project ships.
///
/// The corpus entry is the uncompressed tar, not the gzip stream: a compression corpus made
/// of already-compressed bytes measures the container, not the data.
const GO_SOURCE: Entry = Entry {
    name: "go-source",
    group: Group::SourceCode,
    content: "a source tree of Go, C, and assembly, concatenated as one tar",
    bytes: 162_634_752,
    digest: "sha256:1dc79ddfa5e461d9a1b1ec89899b2a87b288c4f0e8f5088b8bfe082e5b4bb5a9",
    license: License {
        name: "BSD-3-Clause",
        basis: "stated by the LICENSE file at the root of the archived source tree",
    },
    origin: Origin::Fetched(Download {
        url: "https://go.dev/dl/go1.27.1.src.tar.gz",
        revision: "go1.27.1",
        archive: "go1.27.1.src.tar.gz",
        archive_bytes: 35_109_201,
        archive_digest: "sha256:4e408abae126d916b6164627193f2c54f0e3ca1312d693b86db45f862ab238b1",
        unpack: Unpack::Gzip,
    }),
};

/// Builds one Project Gutenberg entry, which differ only in the work they carry.
macro_rules! gutenberg {
    ($name:literal, $content:literal, $id:literal, $url:literal, $bytes:expr, $digest:literal) => {
        Entry {
            name: $name,
            group: Group::Gutenberg,
            content: $content,
            bytes: $bytes,
            digest: $digest,
            license: License {
                name: "Project Gutenberg License",
                basis: "stated in the header of the file itself, which carries the \
                        Project Gutenberg License for a work that is public domain in \
                        the United States",
            },
            origin: Origin::Fetched(Download {
                url: $url,
                revision: $id,
                archive: $name,
                archive_bytes: $bytes,
                archive_digest: $digest,
                unpack: Unpack::Plain,
            }),
        }
    };
}

/// Every registered entry.
///
/// The project entries come first, because they are the only ones a host with no network
/// can produce.
pub const ENTRIES: &[Entry] = &[
    generated!(
        "project-high-entropy-tiny",
        Shape::HighEntropy,
        0x0001_0000_0000_0001,
        256,
        "sha256:82d8661376cfeecf22c9b619866b155c71aca1d16b6da60b1d887acc735bdfb1"
    ),
    generated!(
        "project-zeros-tiny",
        Shape::Zeros,
        0x0001_0000_0000_0002,
        512,
        "sha256:076a27c79e5ace2a3d47f9dd2e83e4ff6ea8872b3c2218f66c92b89b55f36560"
    ),
    generated!(
        "project-json-small",
        Shape::Json,
        0x0001_0000_0000_0003,
        1_024,
        "sha256:3bcdb5b802b491afaf190be45d33729c6e547b38e802ec673d9d84fb0cd7e73c"
    ),
    generated!(
        "project-sparse-small",
        Shape::SparseStructures,
        0x0001_0000_0000_0004,
        4_096,
        "sha256:0f28fd3df61031f7369a008e526bbc241de31a75207238fdcef42600270a2232"
    ),
    generated!(
        "project-short-tokens-small",
        Shape::ShortRepeatedTokens,
        0x0001_0000_0000_0005,
        16_384,
        "sha256:5ec2b370410e7942c5a11af7c0087a6c9f7cd963a99ee62748b59ed313ed17e6"
    ),
    generated!(
        "project-source-small",
        Shape::SourceCode,
        0x0001_0000_0000_0006,
        32_768,
        "sha256:593f26ce7d6bea1009c913f7044829d89440540d4f9028ac6f576856a54efe6a"
    ),
    generated!(
        "project-logs-medium",
        Shape::Logs,
        0x0001_0000_0000_0007,
        65_536,
        "sha256:b6a1cc7ac6792c218569d4eb68b664441c452e64d771fc65a7b326a7f0119008"
    ),
    generated!(
        "project-json-medium",
        Shape::Json,
        0x0001_0000_0000_0008,
        262_144,
        "sha256:b05ca2849e21905666823e4b2062a93155ea0ca21727dbd528c3cb6c0d1fc88d"
    ),
    generated!(
        "project-database-rows-medium",
        Shape::DatabaseRows,
        0x0001_0000_0000_0009,
        262_144,
        "sha256:64f3caefc08f09e67a1fcc9c4f6f1a9a3580eeb424d662df346d88979261d339"
    ),
    generated!(
        "project-serialized-binary-medium",
        Shape::SerializedBinary,
        0x0001_0000_0000_000A,
        262_144,
        "sha256:a528d27645bc7daf46bbf81f40e541b7dab615d3d0f545dd4860bb37ace1879f"
    ),
    generated!(
        "project-mixed-medium",
        Shape::MixedBinaryText,
        0x0001_0000_0000_000B,
        262_144,
        "sha256:35186ea3ccbdf0acbdd4df484c2c4f4607ed0d284a6c4390740e9da44a6047ac"
    ),
    generated!(
        "project-source-medium",
        Shape::SourceCode,
        0x0001_0000_0000_000C,
        1_048_576,
        "sha256:6629173b07bca85f130c8951d26f3aa1268b52a742899d6dc8a6b1d3b4d8a5cd"
    ),
    generated!(
        "project-zeros-medium",
        Shape::Zeros,
        0x0001_0000_0000_000D,
        1_048_576,
        "sha256:30e14955ebf1352266dc2ff8067e68104607e750abb9d3b36582b8af909fcb58"
    ),
    generated!(
        "project-long-repetitions-medium",
        Shape::LongRepetitions,
        0x0001_0000_0000_000E,
        1_048_576,
        "sha256:2163d150f872a69c25ade8cd5e33dc03d05c855ce246fdb21ad9fd9fbb3828e0"
    ),
    generated!(
        "project-high-entropy-medium",
        Shape::HighEntropy,
        0x0001_0000_0000_000F,
        1_048_576,
        "sha256:3e03eb87b41ec05b76e80735fecf25c0c2e298b6de037ca35c6e460a267f0f5e"
    ),
    generated!(
        "project-already-compressed-medium",
        Shape::AlreadyCompressed,
        0x0001_0000_0000_0010,
        1_048_576,
        "sha256:a184a89fafd2023572fa1cce02f2b6280f331073378645c336fd4da17f1742ec"
    ),
    generated!(
        "project-sparse-medium",
        Shape::SparseStructures,
        0x0001_0000_0000_0011,
        1_048_576,
        "sha256:7d78bf53063112273acdcc9954bb004da889e1a842ec3f2980ecc0296fb98f85"
    ),
    generated!(
        "project-logs-large",
        Shape::Logs,
        0x0001_0000_0000_0012,
        8_388_608,
        "sha256:5db649bff45a0bf42f6c37a997a43a6a44c2e67a7ce9ac14b952a14b95307d0c"
    ),
    generated!(
        "project-large-blob",
        Shape::LargeBlob,
        0x0001_0000_0000_0013,
        8_388_608,
        "sha256:18da36bf987b36387ab5f635c4f31a6ea8a63c9563d6aabd9f74ad764543e469"
    ),
    ENWIK8,
    GO_SOURCE,
    gutenberg!(
        "gutenberg-pride-and-prejudice",
        "English prose, as plain text",
        "Project Gutenberg eBook 1342",
        "https://www.gutenberg.org/cache/epub/1342/pg1342.txt",
        772_386,
        "sha256:3f6bb9d6f78e0293b56acd4714dd68cb7d6d1d293402031ce9d5a216bcaf9d75"
    ),
    gutenberg!(
        "gutenberg-war-and-peace",
        "English prose translated from Russian, as plain text",
        "Project Gutenberg eBook 2600",
        "https://www.gutenberg.org/cache/epub/2600/pg2600.txt",
        3_359_610,
        "sha256:2d5bb2ad5f422765e714617e21fa31bbaf8958aa79682c86fca6660fcc5d1b2b"
    ),
    gutenberg!(
        "gutenberg-shakespeare",
        "English verse and drama, as plain text",
        "Project Gutenberg eBook 100",
        "https://www.gutenberg.org/cache/epub/100/pg100.txt",
        5_638_480,
        "sha256:3cf4b3d44ee14cff4e14e78e2ad3318eff76f3f7f2afc3cee6bb925879110a37"
    ),
];

/// Which entries an invocation asked for.
pub enum Selection {
    All,
    Group(Group),
    Entry(&'static str),
}

impl Selection {
    /// The entries this selection names, in registry order.
    pub fn entries(&self) -> Vec<&'static Entry> {
        ENTRIES
            .iter()
            .filter(|entry| match self {
                Self::All => true,
                Self::Group(group) => entry.group == *group,
                Self::Entry(name) => entry.name == *name,
            })
            .collect()
    }

    /// What this selection names, for a message.
    pub fn describe(&self) -> String {
        match self {
            Self::All => String::from("every entry"),
            Self::Group(group) => format!("the {} group", group.name()),
            Self::Entry(name) => format!("the entry {name}"),
        }
    }
}

/// Finds an entry by the name a segment or a result uses.
pub fn find(name: &str) -> Option<&'static Entry> {
    ENTRIES.iter().find(|entry| entry.name == name)
}

/// Every group name, in registry order, for a usage message.
pub fn group_names() -> String {
    GROUPS
        .iter()
        .map(|group| group.name())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::{CLASSES, ENTRIES, Entry, GROUPS, Group, Origin, Selection, SizeClass, find};
    use crate::shape::SHAPES;

    fn generated() -> Vec<&'static Entry> {
        ENTRIES
            .iter()
            .filter(|entry| matches!(entry.origin, Origin::Generated(_)))
            .collect()
    }

    #[test]
    fn every_entry_states_a_license_and_the_basis_for_it() {
        for entry in ENTRIES {
            assert!(
                !entry.license.name.trim().is_empty(),
                "{} states no license",
                entry.name
            );
            assert!(
                !entry.license.basis.trim().is_empty(),
                "{} states no basis for its license",
                entry.name
            );
        }
    }

    #[test]
    fn every_entry_maps_to_exactly_one_class() {
        for entry in ENTRIES {
            let matched: Vec<SizeClass> = CLASSES
                .iter()
                .copied()
                .filter(|class| *class == entry.class())
                .collect();
            assert_eq!(matched.len(), 1, "{} maps to {matched:?}", entry.name);
        }
    }

    #[test]
    fn every_class_from_tiny_to_large_has_an_entry() {
        for class in [
            SizeClass::Tiny,
            SizeClass::Small,
            SizeClass::Medium,
            SizeClass::Large,
        ] {
            assert!(
                ENTRIES.iter().any(|entry| entry.class() == class),
                "no entry is in the {} class",
                class.name()
            );
        }
    }

    #[test]
    fn the_class_boundaries_are_half_open_so_no_size_falls_in_two() {
        assert_eq!(SizeClass::of(0), SizeClass::Tiny);
        assert_eq!(SizeClass::of(1023), SizeClass::Tiny);
        assert_eq!(SizeClass::of(1024), SizeClass::Small);
        assert_eq!(SizeClass::of(65_535), SizeClass::Small);
        assert_eq!(SizeClass::of(65_536), SizeClass::Medium);
        assert_eq!(SizeClass::of(4 * 1024 * 1024 - 1), SizeClass::Medium);
        assert_eq!(SizeClass::of(4 * 1024 * 1024), SizeClass::Large);
        assert_eq!(SizeClass::of(100 * 1024 * 1024 - 1), SizeClass::Large);
        assert_eq!(SizeClass::of(100 * 1024 * 1024), SizeClass::Huge);
    }

    #[test]
    fn every_class_name_round_trips() {
        for class in CLASSES {
            assert_eq!(
                SizeClass::parse(class.name()).map(SizeClass::name),
                Some(class.name())
            );
        }
        assert!(SizeClass::parse("enormous").is_none());
        assert!(SizeClass::names().contains("medium"));
    }

    #[test]
    fn every_entry_name_is_unique() {
        let mut names: Vec<&str> = ENTRIES.iter().map(|entry| entry.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn every_entry_carries_a_digest_of_the_stated_form() {
        for entry in ENTRIES {
            assert_eq!(
                entry.digest.len(),
                "sha256:".len().saturating_add(64),
                "{} carries no sha256 digest",
                entry.name
            );
            assert!(entry.digest.starts_with("sha256:"), "{}", entry.name);
        }
    }

    #[test]
    fn every_fetched_entry_states_where_it_comes_from_and_what_pins_it() {
        for entry in ENTRIES {
            let Origin::Fetched(download) = &entry.origin else {
                continue;
            };
            assert!(download.url.starts_with("http"), "{}", entry.name);
            assert!(!download.revision.trim().is_empty(), "{}", entry.name);
            assert!(download.archive_bytes > 0, "{}", entry.name);
            assert!(
                download.archive_digest.starts_with("sha256:"),
                "{}",
                entry.name
            );
        }
    }

    #[test]
    fn every_generated_entry_carries_its_own_seed() {
        let mut seeds: Vec<u64> = generated()
            .iter()
            .filter_map(|entry| match &entry.origin {
                Origin::Generated(recipe) => Some(recipe.seed),
                Origin::Fetched(_) => None,
            })
            .collect();
        let count = seeds.len();
        seeds.sort_unstable();
        seeds.dedup();
        assert_eq!(seeds.len(), count, "two generated entries share a seed");
    }

    #[test]
    fn the_project_corpus_covers_every_production_shape() {
        for shape in SHAPES {
            assert!(
                generated().iter().any(|entry| match &entry.origin {
                    Origin::Generated(recipe) => recipe.shape == *shape,
                    Origin::Fetched(_) => false,
                }),
                "the project corpus has no {} entry",
                shape.name()
            );
        }
    }

    #[test]
    fn every_group_holds_at_least_one_entry() {
        for group in GROUPS {
            assert!(
                ENTRIES.iter().any(|entry| entry.group == *group),
                "the {} group is empty",
                group.name()
            );
        }
    }

    #[test]
    fn every_group_name_round_trips() {
        for group in GROUPS {
            assert_eq!(
                Group::parse(group.name()).map(Group::name),
                Some(group.name())
            );
        }
        assert!(Group::parse("silesia").is_none());
    }

    #[test]
    fn a_selection_names_the_entries_it_covers() {
        assert_eq!(Selection::All.entries().len(), ENTRIES.len());
        assert!(!Selection::Group(Group::Gutenberg).entries().is_empty());
        assert_eq!(Selection::Entry("enwik8").entries().len(), 1);
        assert!(Selection::Entry("silesia").entries().is_empty());
    }

    #[test]
    fn an_entry_nobody_registered_is_not_found() {
        assert!(find("silesia").is_none());
        assert!(find("").is_none());
        assert!(find("enwik8").is_some());
    }

    #[test]
    fn a_generated_entry_is_named_for_the_shape_it_carries() {
        let entry = find("project-json-medium");
        assert_eq!(
            entry.map(Entry::file).as_deref(),
            Some("project-json-medium.json")
        );
    }

    #[test]
    fn a_fetched_entry_is_cached_under_its_own_name() {
        let entry = find("enwik8");
        assert_eq!(entry.map(Entry::file).as_deref(), Some("enwik8"));
    }
}
