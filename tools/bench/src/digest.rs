//! Owns the checksum that identifies a built library.
//!
//! A manifest checksum says which artifact a result came from. It is not a claim that the
//! build is bit reproducible, and nothing in this module makes one.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

const CHUNK: usize = 64 * 1024;

/// Hashes a file without holding it in memory.
///
/// Returns the digest in the `sha256:<hex>` form a manifest carries.
///
/// # Errors
///
/// Fails when the file cannot be opened or read.
pub fn file(path: &Path) -> Result<String> {
    let mut handle = File::open(path).map_err(|e| Error::at("open", path, e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; CHUNK];
    loop {
        let read = handle
            .read(&mut buffer)
            .map_err(|e| Error::at("read", path, e))?;
        if read == 0 {
            break;
        }
        match buffer.get(..read) {
            Some(chunk) => hasher.update(chunk),
            None => return Err(Error::at("read", path, std::io::ErrorKind::Other.into())),
        }
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

/// Hashes bytes already in memory, in the same form `file` reports.
pub fn bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::{bytes, file};

    #[test]
    fn hashing_bytes_agrees_with_hashing_the_file_that_holds_them() {
        let path = std::env::temp_dir().join("entroq-bench-digest-agree");
        let data = vec![3_u8; 100 * 1024];
        assert!(std::fs::write(&path, &data).is_ok());
        assert_eq!(file(&path).ok(), Some(bytes(&data)));
    }

    #[test]
    fn an_empty_file_hashes_to_the_known_empty_digest() {
        let path = std::env::temp_dir().join("entroq-bench-digest-empty");
        assert!(std::fs::write(&path, b"").is_ok());
        assert_eq!(
            file(&path).ok().as_deref(),
            Some("sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
    }

    #[test]
    fn a_digest_changes_with_one_byte() {
        let dir = std::env::temp_dir();
        let first = dir.join("entroq-bench-digest-a");
        let second = dir.join("entroq-bench-digest-b");
        assert!(std::fs::write(&first, b"laboratory").is_ok());
        assert!(std::fs::write(&second, b"laboratorx").is_ok());
        assert_ne!(file(&first).ok(), file(&second).ok());
    }

    #[test]
    fn a_digest_spans_more_than_one_read() {
        let path = std::env::temp_dir().join("entroq-bench-digest-large");
        assert!(std::fs::write(&path, vec![7_u8; 200 * 1024]).is_ok());
        assert!(file(&path).is_ok());
    }

    #[test]
    fn a_missing_file_is_reported() {
        assert!(file(std::path::Path::new("/entroq/no/such/library.a")).is_err());
    }
}
