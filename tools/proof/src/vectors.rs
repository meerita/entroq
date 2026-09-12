//! Owns the cross-architecture proof: a fixed set of streams, produced from a catalog that is
//! code rather than data, and the comparison that decides whether two architectures wrote the
//! same bytes and each reads the other's.
//!
//! The catalog is code so that two lanes running the same revision produce the same request
//! without exchanging a description of it. Only the streams travel between lanes.
//!
//! Byte order is architectural, not microarchitectural, so a lane that emulates a platform
//! still settles the question this module asks. No timing is read here and no performance
//! claim is made from a lane.
//!
//! This module does not own the format's byte order. It observes it.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use codec::format::{
    DecoderPolicy, Error as FormatError, FrameHeader, IntegrityMode, RegionIndependence,
    ResourceClass,
};
use codec::stream::{Decoder, Encoder, StreamState};

use crate::content::{SHAPES, Shape};
use crate::error::{Error, Result};
use crate::host;

/// The name of the file that lists what a lane produced.
const MANIFEST: &str = "manifest.txt";

/// One vector: a frame the catalog asks both lanes to write.
struct Vector {
    name: &'static str,
    class: ResourceClass,
    independence: RegionIndependence,
    integrity: IntegrityMode,
    content_length: bool,
    dictionary: bool,
    region_bytes: usize,
    block_bytes: u32,
    shape: Shape,
    length: usize,
}

/// The lengths the catalog covers.
///
/// Each one sits on a structure the format defines: nothing, one byte, inside a block, on a
/// block edge, on a region edge of the narrow layout, and one byte past a region edge of the
/// wide one.
const LENGTHS: [usize; 8] = [0, 1, 255, 4_096, 16_384, 65_536, 262_144, 1_048_577];

const CLASSES: [ResourceClass; 5] = [
    ResourceClass::Minimal,
    ResourceClass::Small,
    ResourceClass::Medium,
    ResourceClass::Large,
    ResourceClass::Huge,
];

/// The two layouts the catalog writes under.
///
/// The narrow one puts a region boundary every few blocks, so a short vector still holds
/// several regions. The wide one is the encoder's own default.
const NARROW: (usize, u32) = (4_096, 512);
const WIDE: (usize, u32) = (codec::stream::DEFAULT_REGION_BYTES, 65_536);

/// The vectors both lanes write, in the order they write them.
fn catalog() -> Vec<Vector> {
    // The names are stable across revisions of this catalog: a lane compares by name, and a
    // renamed vector would read as a missing one rather than as a changed one.
    const NAMES: [&str; 16] = [
        "v00-empty",
        "v01-one-byte",
        "v02-short-mixed",
        "v03-block-edge",
        "v04-multi-block",
        "v05-region-edge",
        "v06-multi-region",
        "v07-past-region",
        "v08-empty-wide",
        "v09-one-byte-wide",
        "v10-short-wide",
        "v11-block-wide",
        "v12-multi-block-wide",
        "v13-region-wide",
        "v14-multi-region-wide",
        "v15-past-region-wide",
    ];

    let mut built = Vec::new();
    for (index, name) in NAMES.iter().enumerate() {
        let narrow = index < LENGTHS.len();
        let (region_bytes, block_bytes) = if narrow { NARROW } else { WIDE };
        let length = LENGTHS
            .get(index.checked_rem(LENGTHS.len()).unwrap_or(0))
            .copied()
            .unwrap_or(0);
        built.push(Vector {
            name,
            class: CLASSES
                .get(index.checked_rem(CLASSES.len()).unwrap_or(0))
                .copied()
                .unwrap_or(ResourceClass::Small),
            independence: if index.checked_rem(2) == Some(0) {
                RegionIndependence::Independent
            } else {
                RegionIndependence::Dependent
            },
            integrity: if index.checked_rem(2) == Some(0) {
                IntegrityMode::Absent
            } else {
                IntegrityMode::PerRegion
            },
            content_length: index.checked_rem(3) == Some(0),
            dictionary: index.checked_rem(4) == Some(0),
            region_bytes,
            block_bytes,
            shape: SHAPES
                .get(index.checked_rem(SHAPES.len()).unwrap_or(0))
                .copied()
                .unwrap_or(Shape::Mixed),
            length,
        });
    }
    built
}

impl Vector {
    fn header(&self) -> FrameHeader {
        let mut header = FrameHeader::new(self.class, self.independence, self.integrity);
        if self.content_length {
            header.content_length = Some(u64::try_from(self.length).unwrap_or(0));
        }
        if self.dictionary {
            header.dictionary_id = Some(0x0A0B_0C0D);
        }
        header
    }

    fn content(&self) -> Vec<u8> {
        let mut data = vec![0_u8; self.length];
        self.shape.fill(0, &mut data);
        data
    }

    fn file(&self) -> String {
        format!("{}.eqz", self.name)
    }

    /// The stream this vector describes.
    ///
    /// The chunking is the whole buffer in both directions. A stream is identical at every
    /// chunk size, which the permutation segments prove separately, so this segment fixes the
    /// chunking and varies only the architecture.
    fn encode(&self) -> Result<Vec<u8>> {
        let data = self.content();
        let mut encoder = Encoder::with_layout(self.header(), self.region_bytes, self.block_bytes)?;
        let mut stream = Vec::new();
        let mut room = vec![0_u8; 65_536];
        let mut fed = 0_usize;
        while fed < data.len() {
            let rest = data.get(fed..).ok_or(FormatError::InvalidParameter)?;
            let progress = encoder.encode(rest, &mut room)?;
            append(&mut stream, &room, progress.produced)?;
            fed = fed.saturating_add(progress.consumed);
        }
        loop {
            let progress = encoder.finish(&mut room)?;
            append(&mut stream, &room, progress.produced)?;
            if progress.state == StreamState::Finished {
                break;
            }
        }
        Ok(stream)
    }

    /// Decodes `stream` and reports whether it holds this vector's content.
    fn decodes(&self, stream: &[u8]) -> Result<()> {
        let policy = DecoderPolicy::with_max_history_bytes(ResourceClass::Huge.history_bytes());
        let mut decoder = Decoder::new(policy);
        let mut room = vec![0_u8; 4_096];
        let mut out = Vec::new();
        let mut at = 0_usize;
        while at < stream.len() {
            let rest = stream.get(at..).ok_or(FormatError::InvalidParameter)?;
            let progress = decoder.decode(rest, &mut room)?;
            append(&mut out, &room, progress.produced)?;
            at = at.saturating_add(progress.consumed);
            if progress.state == StreamState::Finished {
                break;
            }
            if progress.consumed == 0 && progress.produced == 0 {
                break;
            }
        }
        loop {
            let progress = decoder.decode(&[], &mut room)?;
            append(&mut out, &room, progress.produced)?;
            if progress.produced == 0 {
                break;
            }
        }
        decoder.finish()?;
        if out == self.content() {
            Ok(())
        } else {
            Err(Error::child(format!(
                "{} decoded to {} bytes, not the {} the catalog describes",
                self.name,
                out.len(),
                self.length
            )))
        }
    }

    fn manifest_line(&self, stream_bytes: usize) -> String {
        format!(
            "{} shape={} logical={} region={} block={} class={} stream={}\n",
            self.name,
            self.shape.name(),
            self.length,
            self.region_bytes,
            self.block_bytes,
            history_label(self.class),
            stream_bytes,
        )
    }
}

const fn history_label(class: ResourceClass) -> &'static str {
    match class {
        ResourceClass::Minimal => "minimal",
        ResourceClass::Small => "small",
        ResourceClass::Medium => "medium",
        ResourceClass::Large => "large",
        ResourceClass::Huge => "huge",
    }
}

fn append(sink: &mut Vec<u8>, room: &[u8], produced: usize) -> Result<()> {
    let written = room.get(..produced).ok_or(FormatError::InvalidParameter)?;
    sink.extend_from_slice(written);
    Ok(())
}

/// Writes every vector of the catalog into `dir`, with the manifest that lists them.
///
/// # Errors
///
/// Fails when the directory cannot be written, or when the codec refuses a catalog entry.
pub fn write(dir: &Path) -> Result<String> {
    fs::create_dir_all(dir).map_err(|e| Error::at("create", dir, e))?;
    let mut manifest = format!("# Entroq format vectors, written on {}\n", host::platform());
    let mut total = 0_usize;
    for vector in catalog() {
        let stream = vector.encode()?;
        let path = dir.join(vector.file());
        fs::write(&path, &stream).map_err(|e| Error::at("write", &path, e))?;
        manifest.push_str(&vector.manifest_line(stream.len()));
        total = total.saturating_add(stream.len());
    }
    let path = dir.join(MANIFEST);
    fs::write(&path, &manifest).map_err(|e| Error::at("write", &path, e))?;

    let mut report = String::new();
    let _ = writeln!(
        report,
        "wrote {} vectors, {total} stream bytes, into {}",
        catalog().len(),
        dir.display()
    );
    Ok(report)
}

/// What a comparison of two lanes found.
pub struct Comparison {
    pub identical: bool,
    pub findings: Vec<String>,
    pub vectors: usize,
    pub bytes: u64,
}

/// Compares the vectors two lanes wrote, and decodes each lane's output against the catalog.
///
/// # Errors
///
/// Fails when a vector cannot be read. A vector that differs, or that does not decode, is a
/// finding rather than an error.
pub fn cross(mine: &Path, theirs: &Path) -> Result<Comparison> {
    let mut findings = Vec::new();
    let mut bytes = 0_u64;
    let vectors = catalog();

    for side in [mine, theirs] {
        let path = side.join(MANIFEST);
        if !path.is_file() {
            return Err(Error::child(format!(
                "{} holds no {MANIFEST}, so that lane wrote no vectors",
                side.display()
            )));
        }
    }

    for vector in &vectors {
        let ours = read(mine, &vector.file())?;
        let yours = read(theirs, &vector.file())?;
        bytes = bytes.saturating_add(u64::try_from(ours.len()).unwrap_or(0));

        if ours == yours {
            if let Err(failure) = vector.decodes(&yours) {
                findings.push(format!("{}: {failure}", vector.name));
            }
            if let Err(failure) = vector.decodes(&ours) {
                findings.push(format!("{}, own lane: {failure}", vector.name));
            }
        } else {
            findings.push(difference(&ours, &yours).map_or_else(
                || {
                    format!(
                        "{}: the two lanes wrote {} and {} bytes",
                        vector.name,
                        ours.len(),
                        yours.len()
                    )
                },
                |at| {
                    format!(
                        "{}: the two lanes differ at byte {at}, {} against {} bytes long",
                        vector.name,
                        ours.len(),
                        yours.len()
                    )
                },
            ));
        }
    }

    Ok(Comparison {
        identical: findings.is_empty(),
        findings,
        vectors: vectors.len(),
        bytes,
    })
}

fn read(dir: &Path, name: &str) -> Result<Vec<u8>> {
    let path = dir.join(name);
    fs::read(&path).map_err(|e| Error::at("read", &path, e))
}

/// The first byte position at which two streams differ, when both reach it.
fn difference(left: &[u8], right: &[u8]) -> Option<usize> {
    left.iter()
        .zip(right.iter())
        .position(|(one, other)| one != other)
}

/// The comparison as a report.
#[must_use]
pub fn report(comparison: &Comparison, mine: &Path, theirs: &Path) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# The cross-architecture proof\n");
    let _ = writeln!(out, "Reading lane: {}", host::platform());
    let _ = writeln!(out, "Own vectors: {}", mine.display());
    let _ = writeln!(out, "Other vectors: {}", theirs.display());
    let _ = writeln!(
        out,
        "Vectors: {}, {} stream bytes\n",
        comparison.vectors, comparison.bytes
    );
    if comparison.identical {
        let _ = writeln!(
            out,
            "Every vector is byte identical across the two lanes, and each lane's output \
             decodes to the bytes the catalog describes."
        );
    } else {
        let _ = writeln!(out, "The two lanes do not agree:");
        for finding in &comparison.findings {
            let _ = writeln!(out, "* {finding}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Comparison, catalog, cross, difference, report, write};
    use crate::content::Shape;

    #[test]
    fn every_vector_name_is_unique_so_a_lane_compares_like_with_like() {
        let mut names: Vec<&str> = catalog().iter().map(|vector| vector.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn the_catalog_covers_every_length_and_both_layouts() {
        let vectors = catalog();
        let mut lengths: Vec<usize> = vectors.iter().map(|vector| vector.length).collect();
        lengths.sort_unstable();
        lengths.dedup();
        assert_eq!(lengths.len(), super::LENGTHS.len());
        assert!(vectors.iter().any(|vector| vector.region_bytes == 4_096));
        assert!(
            vectors
                .iter()
                .any(|vector| vector.region_bytes == codec::stream::DEFAULT_REGION_BYTES)
        );
    }

    #[test]
    fn every_vector_encodes_and_decodes_to_what_the_catalog_describes() {
        for vector in catalog() {
            let stream = vector.encode().unwrap_or_default();
            assert!(!stream.is_empty(), "{} did not encode", vector.name);
            assert!(
                vector.decodes(&stream).is_ok(),
                "{} did not decode",
                vector.name
            );
        }
    }

    #[test]
    fn two_lanes_that_wrote_the_same_bytes_agree() {
        let root = std::env::temp_dir().join("entroq-proof-vectors-agree");
        let _ = std::fs::remove_dir_all(&root);
        let mine = root.join("mine");
        let theirs = root.join("theirs");
        assert!(write(&mine).is_ok());
        assert!(write(&theirs).is_ok());

        let comparison = cross(&mine, &theirs);
        assert!(comparison.is_ok(), "the comparison could not run");
        let comparison = comparison.unwrap_or(Comparison {
            identical: false,
            findings: Vec::new(),
            vectors: 0,
            bytes: 0,
        });
        assert!(comparison.identical, "{:?}", comparison.findings);
        assert!(report(&comparison, &mine, &theirs).contains("byte identical"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_lane_that_wrote_one_byte_differently_is_a_finding() {
        let root = std::env::temp_dir().join("entroq-proof-vectors-differ");
        let _ = std::fs::remove_dir_all(&root);
        let mine = root.join("mine");
        let theirs = root.join("theirs");
        assert!(write(&mine).is_ok());
        assert!(write(&theirs).is_ok());

        let victim = catalog()
            .into_iter()
            .find(|vector| vector.length > 0)
            .map(|vector| vector.file());
        let victim = victim.unwrap_or_default();
        let path = theirs.join(&victim);
        let mut bytes = std::fs::read(&path).unwrap_or_default();
        if let Some(slot) = bytes.last_mut() {
            *slot = slot.wrapping_add(1);
        }
        assert!(std::fs::write(&path, &bytes).is_ok());

        let comparison = cross(&mine, &theirs);
        assert!(comparison.is_ok(), "the comparison could not run");
        let comparison = comparison.unwrap_or(Comparison {
            identical: true,
            findings: Vec::new(),
            vectors: 0,
            bytes: 0,
        });
        assert!(!comparison.identical);
        assert!(
            comparison
                .findings
                .iter()
                .any(|finding| finding.contains("differ at byte"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_lane_is_a_failure_rather_than_an_agreement() {
        let root = std::env::temp_dir().join("entroq-proof-vectors-missing");
        let _ = std::fs::remove_dir_all(&root);
        let mine = root.join("mine");
        assert!(write(&mine).is_ok());
        assert!(cross(&mine, &root.join("absent")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_first_difference_is_the_position_a_reader_looks_at() {
        assert_eq!(difference(b"abc", b"abd"), Some(2));
        assert_eq!(difference(b"abc", b"abc"), None);
        assert_eq!(difference(b"abc", b"ab"), None);
    }

    #[test]
    fn the_catalog_carries_more_than_one_content_shape() {
        let vectors = catalog();
        assert!(vectors.iter().any(|vector| vector.shape == Shape::Zeros));
        assert!(vectors.iter().any(|vector| vector.shape == Shape::Mixed));
    }
}
