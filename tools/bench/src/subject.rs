//! Owns what a measurement drives: this repository's codec, or one pinned competitor.
//!
//! A comparison has two sides and they are not the same kind of thing. A competitor is a
//! build: it is pinned to an upstream commit, built into the laboratory, and linked. Entroq is
//! the subject: it is this repository, at the revision the run records, and nothing fetches
//! it. Describing both with the competitor's own struct would give Entroq an upstream commit
//! it does not have and a static archive nobody built.
//!
//! What they do share is the axis a segment measures along: a name, a version, and a set of
//! operating points grouped by what one segment can afford. That is what this module states
//! once, so every caller below it asks the same questions of both sides.
//!
//! This module does not own how either side is driven, what is measured, or where it is
//! written.

use std::fmt;

use crate::catalog::{self, Codec, PointGroup};

/// The operating points this revision of Entroq exposes.
///
/// The codec admits two modes, FAST and BALANCED, and both write the same format. The group
/// holds both because one segment can afford them together: each is cheaper than the
/// competitor groups the tier measures beside it. FAST stays the default, because it is the
/// shipped mode and the one the dev tier measures.
const ENTROQ_POINTS: &[PointGroup] = &[PointGroup {
    name: "default",
    points: &["fast", "balanced"],
}];

/// The identifier a segment, a directory, and a result use for this repository's codec.
pub const ENTROQ: &str = "entroq";

/// What one segment measures.
///
/// Two subjects are the same when they name the same codec. The competitor behind one is a
/// static description and never two of them, so the name is the identity and the description
/// is what hangs off it.
#[derive(Clone, Copy)]
pub enum Subject {
    /// This repository's codec, driven through its own crate.
    Entroq,
    /// One pinned competitor, driven through the library its project publishes.
    Competitor(&'static Codec),
}

impl PartialEq for Subject {
    fn eq(&self, other: &Self) -> bool {
        self.name() == other.name()
    }
}

impl Eq for Subject {}

impl fmt::Debug for Subject {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(self.name())
    }
}

impl Subject {
    /// The subject a `--codec` argument names, whether it is the subject or a competitor.
    pub fn parse(name: &str) -> Option<Self> {
        if name == ENTROQ {
            return Some(Self::Entroq);
        }
        catalog::find(name).map(Self::Competitor)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Entroq => ENTROQ,
            Self::Competitor(codec) => codec.name,
        }
    }

    pub const fn display(self) -> &'static str {
        match self {
            Self::Entroq => "Entroq",
            Self::Competitor(codec) => codec.display,
        }
    }

    /// The version a result records: the encoder version for Entroq, the pinned release tag
    /// for a competitor.
    pub const fn version(self) -> &'static str {
        match self {
            Self::Entroq => codec::VERSION,
            Self::Competitor(codec) => codec.version,
        }
    }

    /// The competitor behind this subject, or nothing when it is the subject itself.
    ///
    /// A caller that needs the upstream pin, the build, or the linked archive asks for this
    /// and gets nothing for Entroq, which has none of the three.
    pub const fn competitor(self) -> Option<&'static Codec> {
        match self {
            Self::Entroq => None,
            Self::Competitor(codec) => Some(codec),
        }
    }

    pub const fn point_groups(self) -> &'static [PointGroup] {
        match self {
            Self::Entroq => ENTROQ_POINTS,
            Self::Competitor(codec) => codec.point_groups,
        }
    }

    /// The point this subject selects when a caller states no level.
    pub const fn default_point(self) -> &'static str {
        match self {
            Self::Entroq => "fast",
            Self::Competitor(codec) => codec.default_point,
        }
    }

    /// Every operating point this subject is measured at, in group order.
    ///
    /// A competitor answers through the catalog, which owns what its groups mean. The codec
    /// has no catalog entry, so it answers from the groups declared above.
    pub fn operating_points(self) -> Vec<&'static str> {
        match self {
            Self::Entroq => self
                .point_groups()
                .iter()
                .flat_map(|group| group.points.iter().copied())
                .collect(),
            Self::Competitor(codec) => codec.operating_points(),
        }
    }

    /// The group a `--points` argument names.
    pub fn group(self, name: &str) -> Option<&'static PointGroup> {
        match self {
            Self::Entroq => self.point_groups().iter().find(|group| group.name == name),
            Self::Competitor(codec) => codec.group(name),
        }
    }

    /// Every group name, in order, for a usage message.
    pub fn group_names(self) -> String {
        match self {
            Self::Entroq => self
                .point_groups()
                .iter()
                .map(|group| group.name)
                .collect::<Vec<_>>()
                .join(", "),
            Self::Competitor(codec) => codec.group_names(),
        }
    }
}

/// Every subject a measurement may name, the codec first.
pub fn names() -> String {
    let mut listed = vec![ENTROQ];
    listed.extend(catalog::CODECS.iter().map(|codec| codec.name));
    listed.join(", ")
}

#[cfg(test)]
mod tests {
    use super::{ENTROQ, Subject, names};

    #[test]
    fn the_subject_and_every_competitor_are_reachable_by_name() {
        assert_eq!(Subject::parse(ENTROQ), Some(Subject::Entroq));
        for name in ["lz4", "zstd", "brotli", "snappy", "zlib"] {
            assert_eq!(
                Subject::parse(name).map(Subject::name),
                Some(name),
                "{name}"
            );
        }
    }

    #[test]
    fn a_name_nobody_declared_is_not_a_subject() {
        assert!(Subject::parse("lzma").is_none());
        assert!(Subject::parse("").is_none());
    }

    #[test]
    fn the_codec_has_no_upstream_pin_and_a_competitor_does() {
        assert!(Subject::Entroq.competitor().is_none());
        assert!(
            Subject::parse("zstd")
                .and_then(Subject::competitor)
                .is_some()
        );
    }

    #[test]
    fn the_codec_reports_the_version_its_crate_declares() {
        assert_eq!(Subject::Entroq.version(), codec::VERSION);
        assert!(!Subject::Entroq.version().is_empty());
    }

    #[test]
    fn the_codec_exposes_both_modes_and_defaults_to_the_shipped_one() {
        assert_eq!(Subject::Entroq.operating_points(), vec!["fast", "balanced"]);
        assert_eq!(Subject::Entroq.default_point(), "fast");
        assert!(
            Subject::Entroq
                .operating_points()
                .contains(&Subject::Entroq.default_point())
        );
    }

    #[test]
    fn every_subject_defaults_to_a_point_it_names() {
        let mut subjects = vec![Subject::Entroq];
        subjects.extend(crate::catalog::CODECS.iter().map(Subject::Competitor));
        for subject in subjects {
            assert!(
                subject
                    .operating_points()
                    .contains(&subject.default_point()),
                "{} defaults to a point it does not name",
                subject.name()
            );
            let first = subject.point_groups().first().map(|group| group.name);
            assert!(first.and_then(|name| subject.group(name)).is_some());
        }
    }

    #[test]
    fn a_group_nobody_declared_is_not_found() {
        assert!(Subject::Entroq.group("max").is_none());
        assert!(Subject::Entroq.group("default").is_some());
        assert!(Subject::Entroq.group_names().contains("default"));
    }

    #[test]
    fn the_usage_message_names_the_codec_first_and_every_competitor_after_it() {
        let listed = names();
        assert!(listed.starts_with(ENTROQ), "{listed}");
        for codec in crate::catalog::CODECS {
            assert!(listed.contains(codec.name), "{listed}");
        }
    }
}
