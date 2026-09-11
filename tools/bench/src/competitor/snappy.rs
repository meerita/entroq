//! Owns the Snappy boundary.
//!
//! Every unsafe operation in this module is a call across the C boundary of the linked
//! library. There is no safe way to reach a C library, and nothing here is an optimization.
//!
//! Snappy defines a block format and a framing format, and the two are not interchangeable.
//! The measured format is the block format, which is what the C interface the project
//! publishes produces. The framing format carries a stream header and per-chunk checksums
//! the block format does not, so a number from one says nothing about the other.
//!
//! Three metrics are unavailable here, and each one is unavailable because the project
//! publishes no interface that reports it. The C interface is one shot, so there is no
//! streaming latency to measure; it exposes no allocator hook, so the allocations its C++
//! implementation makes through `operator new` are invisible to the harness counter; and it
//! publishes no state size. None of the three is reported as a zero.
#![allow(unsafe_code)]
// Every method takes a receiver because the dispatching enum calls them uniformly, and
// every one that can fail for another competitor keeps the same shape here.
#![allow(clippy::unused_self, clippy::unnecessary_wraps)]

use std::ffi::{c_char, c_int};

use super::{Method, Notes};
use crate::error::{Error, Result};

/// `SNAPPY_OK`.
const OK: c_int = 0;

unsafe extern "C" {
    fn snappy_max_compressed_length(source_length: usize) -> usize;
    fn snappy_compress(
        input: *const c_char,
        input_length: usize,
        compressed: *mut c_char,
        compressed_length: *mut usize,
    ) -> c_int;
    fn snappy_uncompress(
        compressed: *const c_char,
        compressed_length: usize,
        uncompressed: *mut c_char,
        uncompressed_length: *mut usize,
    ) -> c_int;
}

pub struct Session;

impl Session {
    pub const fn open(_point: &str) -> Result<Self> {
        // The catalog pins one operating point for Snappy, because the library publishes no
        // level. `Session::open` has already checked the name against the catalog.
        Ok(Self)
    }

    pub const fn linked_version(&self) -> Option<String> {
        None
    }

    pub const fn notes(&self) -> Notes {
        Notes {
            format: "the Snappy block format, as snappy_compress produces it. The framing \
                     format carries a stream header and per-chunk checksums this format \
                     does not.",
            integrity: "None. The block format defines no checksum, so none was enabled and \
                        none was disabled. The per-chunk CRC-32C of the framing format is \
                        outside what this measurement produced.",
            state_bytes: Method::Absent(
                "Snappy publishes no state-size call and no allocator hook, so the working \
                 memory its C++ implementation holds is not reachable from the harness.",
            ),
            allocations: Method::Absent(
                "Snappy publishes no allocator hook. Its C++ implementation allocates \
                 through the C++ runtime, which the harness allocator does not serve.",
            ),
            streaming: Method::Absent(
                "The C interface Snappy publishes is one shot. Its incremental interface is \
                 a C++ Source and Sink pair, which is not reachable across a C boundary.",
            ),
            threads: Method::Absent(
                "Snappy publishes no thread parameter. Its library compresses on the \
                 calling thread.",
            ),
        }
    }

    pub fn compress_bound(&self, input: usize) -> usize {
        // SAFETY: the bound is a pure function of the length.
        unsafe { snappy_max_compressed_length(input) }
    }

    pub fn compress(&self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        let mut produced = dst.len();
        // SAFETY: both buffers are live for their stated lengths, and `produced` carries the
        // output capacity in and the output length out.
        let status = unsafe {
            snappy_compress(
                src.as_ptr().cast::<c_char>(),
                src.len(),
                dst.as_mut_ptr().cast::<c_char>(),
                &raw mut produced,
            )
        };
        status_or(status, produced, "the Snappy compression")
    }

    pub fn decompress(&self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        let mut produced = dst.len();
        // SAFETY: both buffers are live for their stated lengths, and `produced` carries the
        // output capacity in and the output length out.
        let status = unsafe {
            snappy_uncompress(
                src.as_ptr().cast::<c_char>(),
                src.len(),
                dst.as_mut_ptr().cast::<c_char>(),
                &raw mut produced,
            )
        };
        status_or(status, produced, "the Snappy decompression")
    }
}

fn status_or(status: c_int, produced: usize, what: &str) -> Result<usize> {
    if status == OK {
        Ok(produced)
    } else {
        Err(Error::measure(
            format!("the {what}"),
            format!("failed with status {status}"),
        ))
    }
}
