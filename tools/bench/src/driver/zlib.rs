//! Owns the zlib boundary.
//!
//! Every unsafe operation in this module is a call across the C boundary of the linked
//! library. There is no safe way to reach a C library, and nothing here is an optimization.
//!
//! The measured format is raw deflate, which is the format the project defines. The zlib
//! container and the gzip container both wrap it with a header and a checksum, so a number
//! measured inside one of them is not a number about deflate. The one-shot path and the
//! streaming path both use raw deflate, so a streaming number and a ratio number describe
//! the same work.
//!
//! Every stream is created with the harness allocator, so every byte zlib allocates is
//! counted by the same counter that counts the harness. The project publishes no state-size
//! call, so the bytes the codec owns come from that counter instead.
#![allow(unsafe_code)]
// Every method takes a receiver because the dispatching enum calls them uniformly.
#![allow(clippy::unused_self)]

use std::ffi::{CStr, c_char, c_int, c_uint, c_ulong, c_void};

use super::{Chunk, Method, Notes, parameter};
use crate::alloc;
use crate::clock;
use crate::error::{Error, Result};

/// The stream descriptor, at the layout `zlib.h` declares.
#[repr(C)]
struct Stream {
    next_in: *const u8,
    avail_in: c_uint,
    total_in: c_ulong,
    next_out: *mut u8,
    avail_out: c_uint,
    total_out: c_ulong,
    msg: *const c_char,
    state: *mut c_void,
    zalloc: Option<unsafe extern "C" fn(*mut c_void, c_uint, c_uint) -> *mut c_void>,
    zfree: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    opaque: *mut c_void,
    data_type: c_int,
    adler: c_ulong,
    reserved: c_ulong,
}

/// `Z_DEFLATED`, the only method the format defines.
const METHOD_DEFLATED: c_int = 8;
/// A negative window selects raw deflate, with no container around it.
const WINDOW_RAW: c_int = -15;
/// The library's own default, which is what a caller that states nothing gets.
const MEM_LEVEL: c_int = 8;
/// `Z_DEFAULT_STRATEGY`.
const STRATEGY_DEFAULT: c_int = 0;
/// `Z_SYNC_FLUSH`, which ends a block so the bytes so far can be read.
const FLUSH_SYNC: c_int = 2;
/// `Z_FINISH`.
const FLUSH_FINISH: c_int = 4;
/// `Z_OK`.
const OK: c_int = 0;
/// `Z_STREAM_END`.
const STREAM_END: c_int = 1;

unsafe extern "C" {
    fn zlibVersion() -> *const c_char;
    fn compressBound(source_len: c_ulong) -> c_ulong;
    fn deflateInit2_(
        stream: *mut Stream,
        level: c_int,
        method: c_int,
        window_bits: c_int,
        mem_level: c_int,
        strategy: c_int,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;
    fn deflate(stream: *mut Stream, flush: c_int) -> c_int;
    fn deflateEnd(stream: *mut Stream) -> c_int;
    fn inflateInit2_(
        stream: *mut Stream,
        window_bits: c_int,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;
    fn inflate(stream: *mut Stream, flush: c_int) -> c_int;
    fn inflateEnd(stream: *mut Stream) -> c_int;
}

impl Stream {
    /// A descriptor that allocates through the harness and points at nothing yet.
    const fn empty() -> Self {
        Self {
            next_in: std::ptr::null(),
            avail_in: 0,
            total_in: 0,
            next_out: std::ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: std::ptr::null(),
            state: std::ptr::null_mut(),
            zalloc: Some(alloc::reserve_items),
            zfree: Some(alloc::release),
            opaque: std::ptr::null_mut(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        }
    }
}

pub struct Session {
    level: c_int,
}

impl Session {
    pub fn open(point: &str) -> Result<Self> {
        let level = parameter(point, "level-").ok_or_else(|| {
            Error::measure(
                format!("the zlib operating point `{point}`"),
                "does not name a compression level",
            )
        })?;
        Ok(Self { level })
    }

    pub fn linked_version(&self) -> Option<String> {
        // SAFETY: the call takes no argument.
        let pointer = unsafe { zlibVersion() };
        // SAFETY: the pointer is the library's own static, NUL-terminated string.
        let text = unsafe { CStr::from_ptr(pointer) };
        text.to_str().ok().map(String::from)
    }

    pub const fn notes(&self) -> Notes {
        Notes {
            format: "raw deflate, with no container. The zlib and gzip containers each add \
                     a header and a checksum this format does not carry.",
            integrity: "None. Raw deflate defines no checksum. The Adler-32 of the zlib \
                        container and the CRC-32 of the gzip container are outside what this \
                        measurement produced.",
            state_bytes: Method::By(
                "the harness allocator, which the stream's zalloc hook is set to. The \
                 project publishes no state-size call.",
            ),
            allocations: Method::By("the stream's zalloc and zfree hooks"),
            streaming: Method::By("deflate with Z_NO_FLUSH per chunk"),
            threads: Method::Absent(
                "zlib publishes no thread parameter. Its library compresses on the calling \
                 thread.",
            ),
        }
    }

    pub fn compress_bound(&self, input: usize) -> usize {
        let Ok(len) = c_ulong::try_from(input) else {
            return 0;
        };
        // SAFETY: the bound is a pure function of the length. It bounds the zlib container,
        // which is larger than the raw deflate stream measured here.
        let bound = unsafe { compressBound(len) };
        usize::try_from(bound).unwrap_or(0)
    }

    pub fn compress(&self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        let mut stream = Deflate::open(self.level)?;
        let (_, produced) = stream.run(src, dst, FLUSH_FINISH)?;
        Ok(produced)
    }

    pub fn decompress(&self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        let mut stream = Inflate::open()?;
        stream.run(src, dst)
    }

    pub fn stream(&self, src: &[u8], chunk: usize, dst: &mut [u8]) -> Result<Vec<Chunk>> {
        let mut stream = Deflate::open(self.level)?;
        let mut chunks = Vec::new();
        let mut read = 0_usize;
        let mut written = 0_usize;
        while read < src.len() {
            let take = chunk.min(src.len().saturating_sub(read));
            let end = read.saturating_add(take);
            let last = end >= src.len();
            let slice = src
                .get(read..end)
                .ok_or_else(|| Error::measure("the zlib stream", "read past its input"))?;
            let room = dst
                .get_mut(written..)
                .ok_or_else(|| Error::measure("the zlib stream", "ran out of output room"))?;
            // A chunk is ended with a sync flush so its output is available when it is fed,
            // which is what a first-output latency measures. The last chunk finishes the
            // stream instead.
            let flush = if last { FLUSH_FINISH } else { FLUSH_SYNC };
            let (outcome, elapsed_ns) = clock::time(|| stream.run(slice, room, flush));
            let (_, produced) = outcome?;
            chunks.push(Chunk {
                input_bytes: take,
                output_bytes: produced,
                elapsed_ns,
            });
            read = end;
            written = written.saturating_add(produced);
        }
        Ok(chunks)
    }
}

/// One open deflate stream, released when it goes out of scope.
///
/// The descriptor is boxed because zlib stores the address it was initialized at inside its
/// own state and rejects a later call that arrives through a different one. A descriptor on
/// the stack would move when this value is returned, and every call after that would fail.
struct Deflate {
    stream: Box<Stream>,
}

impl Deflate {
    fn open(level: c_int) -> Result<Self> {
        let mut held = Self {
            stream: Box::new(Stream::empty()),
        };
        let size = stream_size()?;
        // SAFETY: the call takes no argument.
        let version = unsafe { zlibVersion() };
        // SAFETY: the descriptor is live, the version string is the library's own static
        // string, and the declared size matches the struct the library was compiled with.
        let status = unsafe {
            deflateInit2_(
                &raw mut *held.stream,
                level,
                METHOD_DEFLATED,
                WINDOW_RAW,
                MEM_LEVEL,
                STRATEGY_DEFAULT,
                version,
                size,
            )
        };
        if status != OK {
            return Err(Error::measure(
                "the zlib deflate stream",
                format!("could not be opened: status {status}"),
            ));
        }
        Ok(held)
    }

    /// Feeds one slice and drains what the stream produced.
    fn run(&mut self, src: &[u8], dst: &mut [u8], flush: c_int) -> Result<(usize, usize)> {
        self.stream.next_in = src.as_ptr();
        self.stream.avail_in = count(src.len(), "the zlib input")?;
        self.stream.next_out = dst.as_mut_ptr();
        self.stream.avail_out = count(dst.len(), "the zlib output buffer")?;
        loop {
            // SAFETY: the descriptor is live and points at the two buffers above for the
            // lengths beside them.
            let status = unsafe { deflate(&raw mut *self.stream, flush) };
            if status != OK && status != STREAM_END {
                return Err(Error::measure(
                    "the zlib compression",
                    format!("failed with status {status}"),
                ));
            }
            let taken = src
                .len()
                .saturating_sub(usize::try_from(self.stream.avail_in).unwrap_or(0));
            let produced = dst
                .len()
                .saturating_sub(usize::try_from(self.stream.avail_out).unwrap_or(0));
            // A finish is done only when the library says the stream ended. Any other
            // flush is done once it has taken the whole slice and still has room, which is
            // how zlib says it has nothing more to hand back.
            let done = if flush == FLUSH_FINISH {
                status == STREAM_END
            } else {
                self.stream.avail_in == 0 && self.stream.avail_out > 0
            };
            if done {
                return Ok((taken, produced));
            }
            if self.stream.avail_out == 0 {
                return Err(Error::measure(
                    "the zlib compression",
                    "ran out of output room",
                ));
            }
        }
    }
}

impl Drop for Deflate {
    fn drop(&mut self) {
        // SAFETY: the descriptor was opened by `deflateInit2_` and has not been ended.
        let _ = unsafe { deflateEnd(&raw mut *self.stream) };
    }
}

/// One open inflate stream, released when it goes out of scope.
///
/// Boxed for the same reason the deflate stream is.
struct Inflate {
    stream: Box<Stream>,
}

impl Inflate {
    fn open() -> Result<Self> {
        let mut held = Self {
            stream: Box::new(Stream::empty()),
        };
        let size = stream_size()?;
        // SAFETY: the call takes no argument.
        let version = unsafe { zlibVersion() };
        // SAFETY: the descriptor is live, the version string is the library's own static
        // string, and the declared size matches the struct the library was compiled with.
        let status = unsafe { inflateInit2_(&raw mut *held.stream, WINDOW_RAW, version, size) };
        if status != OK {
            return Err(Error::measure(
                "the zlib inflate stream",
                format!("could not be opened: status {status}"),
            ));
        }
        Ok(held)
    }

    fn run(&mut self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        self.stream.next_in = src.as_ptr();
        self.stream.avail_in = count(src.len(), "the zlib compressed input")?;
        self.stream.next_out = dst.as_mut_ptr();
        self.stream.avail_out = count(dst.len(), "the zlib output buffer")?;
        // SAFETY: the descriptor is live and points at the two buffers above for the
        // lengths beside them.
        let status = unsafe { inflate(&raw mut *self.stream, FLUSH_FINISH) };
        if status != STREAM_END {
            return Err(Error::measure(
                "the zlib decompression",
                format!("ended with status {status} rather than the end of the stream"),
            ));
        }
        Ok(dst
            .len()
            .saturating_sub(usize::try_from(self.stream.avail_out).unwrap_or(0)))
    }
}

impl Drop for Inflate {
    fn drop(&mut self) {
        // SAFETY: the descriptor was opened by `inflateInit2_` and has not been ended.
        let _ = unsafe { inflateEnd(&raw mut *self.stream) };
    }
}

/// The length zlib's 32-bit counters accept.
fn count(len: usize, what: &str) -> Result<c_uint> {
    c_uint::try_from(len).map_err(|_| {
        Error::measure(
            what,
            format!("is {len} bytes, and zlib's stream counters are 32 bits wide"),
        )
    })
}

fn stream_size() -> Result<c_int> {
    c_int::try_from(size_of::<Stream>())
        .map_err(|_| Error::measure("the zlib stream descriptor", "is larger than an int"))
}
