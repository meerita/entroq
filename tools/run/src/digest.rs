//! Owns the digest that identifies an input set.
//!
//! The digest covers every input's path and its working-tree bytes, in path order, so any
//! change to any input changes it. It answers one question: which inputs produced this
//! result. It is not an integrity check on the repository, and it is not a codec checksum.
//!
//! Each input is digested on its own and the per-input digests are folded into the set
//! digest with their paths. A path carries no NUL byte, so the folded encoding is
//! unambiguous without holding any input in memory.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Read size for digesting one input. No input is held whole in memory.
const CHUNK_BYTES: usize = 65_536;

/// Stands in for an input the set names and the working tree does not hold.
///
/// It is not a digest, so no file's contents can collide with it.
const ABSENT: &str = "absent";

/// Accumulates the digest of an input set, one input at a time.
pub struct SetDigest {
    hasher: Sha256,
    file_count: usize,
    byte_count: u64,
}

impl SetDigest {
    pub fn new() -> Self {
        Self {
            hasher: Sha256::new(),
            file_count: 0,
            byte_count: 0,
        }
    }

    /// Folds one input into the set.
    ///
    /// `relative` is the path the set is keyed by, and `path` is where its bytes are read
    /// from. Inputs must be folded in a stable order.
    ///
    /// # Errors
    ///
    /// Fails when the input cannot be read.
    pub fn add(&mut self, relative: &str, path: &Path) -> Result<()> {
        let (digest, bytes) = digest_file(path)?;
        self.fold(relative, &digest);
        self.file_count = self.file_count.saturating_add(1);
        self.byte_count = self.byte_count.saturating_add(bytes);
        Ok(())
    }

    /// Folds a path that the set names but the working tree does not hold.
    ///
    /// A deleted input is part of the set's identity, so it is folded in rather than
    /// skipped. It contributes no bytes and is not counted as a file.
    pub fn add_absent(&mut self, relative: &str) {
        self.fold(relative, ABSENT);
    }

    fn fold(&mut self, relative: &str, digest: &str) {
        self.hasher.update(relative.as_bytes());
        self.hasher.update(*b"\0");
        self.hasher.update(digest.as_bytes());
        self.hasher.update(*b"\n");
    }

    /// The set digest, the number of inputs, and their total size.
    pub fn finish(self) -> (String, usize, u64) {
        (
            format!("sha256:{:x}", self.hasher.finalize()),
            self.file_count,
            self.byte_count,
        )
    }
}

fn digest_file(path: &Path) -> Result<(String, u64)> {
    let mut file = File::open(path).map_err(|e| Error::at("read", path, e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; CHUNK_BYTES];
    let mut total: u64 = 0;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| Error::at("read", path, e))?;
        if read == 0 {
            break;
        }
        let Some(chunk) = buffer.get(..read) else {
            return Err(Error::at("read", path, std::io::Error::other("short read")));
        };
        hasher.update(chunk);
        total = total.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
    }
    Ok((format!("{:x}", hasher.finalize()), total))
}

#[cfg(test)]
mod tests {
    use super::SetDigest;
    use std::io::Write;

    fn write(dir: &std::path::Path, name: &str, contents: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        let written = std::fs::File::create(&path).and_then(|mut f| f.write_all(contents));
        assert!(written.is_ok());
        path
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("entroq-run-digest-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(std::fs::create_dir_all(&dir).is_ok());
        dir
    }

    fn digest_of(dir: &std::path::Path, inputs: &[(&str, &[u8])]) -> (String, usize, u64) {
        let mut set = SetDigest::new();
        for (name, contents) in inputs {
            let path = write(dir, name, contents);
            assert!(set.add(name, &path).is_ok());
        }
        set.finish()
    }

    #[test]
    fn an_empty_set_has_a_digest_and_no_inputs() {
        let (digest, files, bytes) = SetDigest::new().finish();
        assert!(digest.starts_with("sha256:"));
        assert_eq!((files, bytes), (0, 0));
    }

    #[test]
    fn the_same_inputs_give_the_same_digest() {
        let a = scratch("same-a");
        let b = scratch("same-b");
        let left = digest_of(&a, &[("one.rs", b"fn main() {}"), ("two.rs", b"")]);
        let right = digest_of(&b, &[("one.rs", b"fn main() {}"), ("two.rs", b"")]);
        assert_eq!(left, right);
    }

    #[test]
    fn changing_one_byte_changes_the_digest() {
        let a = scratch("content-a");
        let b = scratch("content-b");
        let left = digest_of(&a, &[("one.rs", b"fn main() {}")]);
        let right = digest_of(&b, &[("one.rs", b"fn main() { }")]);
        assert_ne!(left.0, right.0);
    }

    #[test]
    fn renaming_an_input_changes_the_digest() {
        let a = scratch("name-a");
        let b = scratch("name-b");
        let left = digest_of(&a, &[("one.rs", b"same")]);
        let right = digest_of(&b, &[("two.rs", b"same")]);
        assert_ne!(left.0, right.0);
    }

    #[test]
    fn an_absent_input_still_changes_the_digest() {
        let dir = scratch("absent");
        let present = digest_of(&dir, &[("one.rs", b"x")]);
        let mut set = SetDigest::new();
        set.add_absent("one.rs");
        let absent = set.finish();
        assert_ne!(present.0, absent.0);
        assert_ne!(SetDigest::new().finish().0, absent.0);
    }

    #[test]
    fn an_absent_input_counts_no_file_and_no_bytes() {
        let mut set = SetDigest::new();
        set.add_absent("gone.rs");
        let (_, files, bytes) = set.finish();
        assert_eq!((files, bytes), (0, 0));
    }

    #[test]
    fn the_set_counts_its_inputs_and_their_bytes() {
        let dir = scratch("counts");
        let (_, files, bytes) = digest_of(&dir, &[("one.rs", b"abc"), ("two.rs", b"de")]);
        assert_eq!((files, bytes), (2, 5));
    }

    #[test]
    fn an_input_larger_than_one_chunk_is_digested_whole() {
        let dir = scratch("chunked");
        let big = vec![b'x'; super::CHUNK_BYTES.saturating_add(17)];
        let (_, files, bytes) = digest_of(&dir, &[("big.bin", &big)]);
        assert_eq!(files, 1);
        assert_eq!(bytes, u64::try_from(big.len()).unwrap_or(0));
    }
}
