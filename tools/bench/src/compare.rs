//! Owns the byte comparison between a recorded library and the one a rebuild produced.
//!
//! A digest says two files differ. It does not say how much, or where. A rebuild that
//! differs in thirty-two bytes spread across sixteen archive headers is a different fact from
//! one that differs in half its bytes, and only the second calls the build into question.
//!
//! This module measures. It draws no conclusion about what a difference means.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::error::{Error, Result};

const CHUNK: usize = 64 * 1024;

/// How many differing offsets a record keeps, so one measurement cannot fill a file.
const RECORDED_OFFSETS: usize = 16;

/// What two files that are not identical differ in.
pub struct Difference {
    pub recorded_bytes: u64,
    pub rebuilt_bytes: u64,
    /// Differing bytes over the length the two files share.
    pub differing_bytes: u64,
    /// The first differing offsets, up to a fixed count.
    pub first_offsets: Vec<u64>,
}

impl Difference {
    /// The one line a rebuild record carries for this difference.
    pub fn describe(&self) -> String {
        let size = if self.recorded_bytes == self.rebuilt_bytes {
            format!("{} bytes", self.recorded_bytes)
        } else {
            format!(
                "{} bytes recorded against {} rebuilt",
                self.recorded_bytes, self.rebuilt_bytes
            )
        };
        let offsets = self
            .first_offsets
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{size}, {} differing, first at {offsets}",
            self.differing_bytes
        )
    }
}

/// Compares two files byte for byte, without holding either in memory.
///
/// Returns `None` when they are identical.
///
/// # Errors
///
/// Fails when either file cannot be opened or read.
pub fn files(recorded: &Path, rebuilt: &Path) -> Result<Option<Difference>> {
    let mut left = File::open(recorded).map_err(|e| Error::at("open", recorded, e))?;
    let mut right = File::open(rebuilt).map_err(|e| Error::at("open", rebuilt, e))?;
    let mut left_buffer = vec![0_u8; CHUNK];
    let mut right_buffer = vec![0_u8; CHUNK];

    let mut offset: u64 = 0;
    let mut differing: u64 = 0;
    let mut first_offsets: Vec<u64> = Vec::new();

    loop {
        let read_left = fill(&mut left, &mut left_buffer, recorded)?;
        let read_right = fill(&mut right, &mut right_buffer, rebuilt)?;
        let common = read_left.min(read_right);
        if common == 0 {
            break;
        }
        for index in 0..common {
            if left_buffer.get(index) != right_buffer.get(index) {
                differing = differing.saturating_add(1);
                if first_offsets.len() < RECORDED_OFFSETS {
                    first_offsets.push(offset.saturating_add(index as u64));
                }
            }
        }
        offset = offset.saturating_add(common as u64);
        if read_left != read_right {
            break;
        }
    }

    let recorded_bytes = size(recorded)?;
    let rebuilt_bytes = size(rebuilt)?;
    if differing == 0 && recorded_bytes == rebuilt_bytes {
        return Ok(None);
    }
    Ok(Some(Difference {
        recorded_bytes,
        rebuilt_bytes,
        differing_bytes: differing,
        first_offsets,
    }))
}

fn fill(file: &mut File, buffer: &mut [u8], path: &Path) -> Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let Some(rest) = buffer.get_mut(filled..) else {
            break;
        };
        let read = file.read(rest).map_err(|e| Error::at("read", path, e))?;
        if read == 0 {
            break;
        }
        filled = filled.saturating_add(read);
    }
    Ok(filled)
}

fn size(path: &Path) -> Result<u64> {
    std::fs::metadata(path)
        .map(|data| data.len())
        .map_err(|e| Error::at("measure", path, e))
}

#[cfg(test)]
mod tests {
    use super::files;
    use std::path::PathBuf;

    fn write(name: &str, bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("entroq-bench-compare-{name}"));
        assert!(std::fs::write(&path, bytes).is_ok());
        path
    }

    #[test]
    fn two_identical_files_carry_no_difference() {
        let left = write("same-a", b"laboratory");
        let right = write("same-b", b"laboratory");
        assert!(matches!(files(&left, &right), Ok(None)));
    }

    #[test]
    fn a_difference_counts_the_bytes_and_names_the_first_offsets() {
        let left = write("one-a", b"abcdef");
        let right = write("one-b", b"abXdeY");
        let found = files(&left, &right).ok().flatten();
        assert_eq!(found.as_ref().map(|d| d.differing_bytes), Some(2));
        assert_eq!(
            found.as_ref().map(|d| d.first_offsets.clone()),
            Some(vec![2, 5])
        );
        assert_eq!(found.as_ref().map(|d| d.recorded_bytes), Some(6));
    }

    #[test]
    fn files_of_different_length_are_a_difference_even_with_a_shared_prefix() {
        let left = write("len-a", b"abc");
        let right = write("len-b", b"abcdef");
        let found = files(&left, &right).ok().flatten();
        assert_eq!(found.as_ref().map(|d| d.recorded_bytes), Some(3));
        assert_eq!(found.as_ref().map(|d| d.rebuilt_bytes), Some(6));
        assert_eq!(found.as_ref().map(|d| d.differing_bytes), Some(0));
    }

    #[test]
    fn a_difference_records_no_more_than_a_fixed_count_of_offsets() {
        let left = write("many-a", &[0_u8; 100]);
        let right = write("many-b", &[1_u8; 100]);
        let found = files(&left, &right).ok().flatten();
        assert_eq!(found.as_ref().map(|d| d.differing_bytes), Some(100));
        assert_eq!(found.as_ref().map(|d| d.first_offsets.len()), Some(16));
    }

    #[test]
    fn a_difference_beyond_one_read_is_still_found() {
        let mut left_bytes = vec![9_u8; 200 * 1024];
        let mut right_bytes = left_bytes.clone();
        if let Some(byte) = right_bytes.get_mut(150_000) {
            *byte = 8;
        }
        if let Some(byte) = left_bytes.get_mut(0) {
            *byte = 9;
        }
        let left = write("big-a", &left_bytes);
        let right = write("big-b", &right_bytes);
        let found = files(&left, &right).ok().flatten();
        assert_eq!(found.as_ref().map(|d| d.differing_bytes), Some(1));
        assert_eq!(
            found.as_ref().map(|d| d.first_offsets.clone()),
            Some(vec![150_000])
        );
    }

    #[test]
    fn a_missing_file_is_reported() {
        let left = write("missing-a", b"x");
        assert!(files(&left, std::path::Path::new("/entroq/no/such/lib.a")).is_err());
    }

    #[test]
    fn a_description_states_the_size_the_count_and_the_offsets() {
        let left = write("text-a", b"abcdef");
        let right = write("text-b", b"abXdef");
        let text = files(&left, &right)
            .ok()
            .flatten()
            .map(|d| d.describe())
            .unwrap_or_default();
        assert!(text.contains("6 bytes"), "{text}");
        assert!(text.contains("1 differing"), "{text}");
        assert!(text.contains("first at 2"), "{text}");
    }
}
