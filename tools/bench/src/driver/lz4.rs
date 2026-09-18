//! Owns the LZ4 boundary.
//!
//! The measured format is the LZ4 block format, which is what the block interface produces
//! and what a ratio comparison against LZ4 is normally about. The frame format adds a header
//! and an optional checksum that the block format does not carry, so the streaming metric
//! uses the block streaming interface rather than the frame interface: the same format, the
//! same work, one chunk at a time.
//!
//! The compression state is owned by the harness rather than the library. LZ4 publishes the
//! size of that state and an entry point that takes it, so an encode allocates nothing and
//! the bytes the codec holds are exactly the bytes the project says it needs.

// Every unsafe operation in this module is a call across the C boundary of the linked
// library, or the pointer arithmetic one of those calls requires. There is no safe way
// to reach a C library, and nothing here is an optimization.
#![allow(unsafe_code)]
// Every method takes a receiver because the dispatching enum calls them uniformly. A
// competitor whose library needs no state for one of them still has to answer it.
#![allow(clippy::unused_self)]

use std::ffi::{CStr, c_char, c_int, c_void};

use super::{Chunk, Method, Notes, as_int, parameter};
use crate::clock;
use crate::error::{Error, Result};

unsafe extern "C" {
    fn LZ4_versionString() -> *const c_char;
    fn LZ4_compressBound(input_size: c_int) -> c_int;
    fn LZ4_sizeofState() -> c_int;
    fn LZ4_sizeofStateHC() -> c_int;
    fn LZ4_compress_fast_extState(
        state: *mut c_void,
        src: *const c_char,
        dst: *mut c_char,
        src_size: c_int,
        dst_capacity: c_int,
        acceleration: c_int,
    ) -> c_int;
    fn LZ4_compress_HC_extStateHC(
        state: *mut c_void,
        src: *const c_char,
        dst: *mut c_char,
        src_size: c_int,
        dst_capacity: c_int,
        level: c_int,
    ) -> c_int;
    fn LZ4_decompress_safe(
        src: *const c_char,
        dst: *mut c_char,
        compressed_size: c_int,
        dst_capacity: c_int,
    ) -> c_int;
    fn LZ4_createStream() -> *mut c_void;
    fn LZ4_freeStream(stream: *mut c_void) -> c_int;
    fn LZ4_compress_fast_continue(
        stream: *mut c_void,
        src: *const c_char,
        dst: *mut c_char,
        src_size: c_int,
        dst_capacity: c_int,
        acceleration: c_int,
    ) -> c_int;
    fn LZ4_createStreamHC() -> *mut c_void;
    fn LZ4_freeStreamHC(stream: *mut c_void) -> c_int;
    fn LZ4_resetStreamHC_fast(stream: *mut c_void, level: c_int);
    fn LZ4_compress_HC_continue(
        stream: *mut c_void,
        src: *const c_char,
        dst: *mut c_char,
        src_size: c_int,
        max_dst_size: c_int,
    ) -> c_int;
}

/// Which of the two encoders an operating point selects.
#[derive(Clone, Copy)]
enum Mode {
    /// The fast encoder, parameterized by acceleration.
    Fast(c_int),
    /// The high compression encoder, parameterized by level.
    High(c_int),
}

pub struct Session {
    mode: Mode,
    /// The compression state, owned here. It is `u64` rather than `u8` because the library
    /// requires the state to be aligned for pointers, and a byte vector is not.
    state: Vec<u64>,
}

impl Session {
    pub fn open(point: &str) -> Result<Self> {
        let mode = if let Some(acceleration) = parameter(point, "fast-") {
            Mode::Fast(acceleration)
        } else if let Some(level) = parameter(point, "hc-") {
            Mode::High(level)
        } else {
            return Err(Error::measure(
                format!("the LZ4 operating point `{point}`"),
                "names neither the fast encoder nor the high compression encoder",
            ));
        };
        let bytes = match mode {
            // SAFETY: the call reports a constant and takes no argument.
            Mode::Fast(_) => unsafe { LZ4_sizeofState() },
            // SAFETY: the call reports a constant and takes no argument.
            Mode::High(_) => unsafe { LZ4_sizeofStateHC() },
        };
        let words = usize::try_from(bytes)
            .map_err(|_| Error::measure("the LZ4 state size", "is not a length"))?
            .div_ceil(size_of::<u64>());
        Ok(Self {
            mode,
            state: vec![0; words],
        })
    }

    pub fn linked_version(&self) -> Option<String> {
        // SAFETY: the call takes no argument.
        let pointer = unsafe { LZ4_versionString() };
        // SAFETY: the pointer is the library's own static, NUL-terminated string.
        let text = unsafe { CStr::from_ptr(pointer) };
        text.to_str().ok().map(String::from)
    }

    pub const fn notes(&self) -> Notes {
        Notes {
            format: "the LZ4 block format, as LZ4_compress_fast_extState and \
                     LZ4_compress_HC_extStateHC produce it. The frame format carries a \
                     header and an optional checksum this format does not.",
            integrity: "None. The block format defines no checksum, so none was enabled and \
                        none was disabled. The optional content checksum of the frame format \
                        is outside what this measurement produced.",
            state_bytes: Method::By("LZ4_sizeofState and LZ4_sizeofStateHC"),
            allocations: Method::By(
                "the harness allocator, which owns the compression state the library's \
                 external-state entry points take",
            ),
            streaming: Method::By("LZ4_compress_fast_continue and LZ4_compress_HC_continue"),
            threads: Method::Absent(
                "LZ4 publishes no thread parameter. Its library compresses on the calling \
                 thread, and parallelism is left to whatever frames the blocks.",
            ),
        }
    }

    pub fn compress_bound(&self, input: usize) -> usize {
        let Ok(len) = c_int::try_from(input) else {
            return 0;
        };
        // SAFETY: the bound is a pure function of the length.
        let bound = unsafe { LZ4_compressBound(len) };
        usize::try_from(bound).unwrap_or(0)
    }

    pub fn encoder_state_bytes(&self) -> Option<u64> {
        u64::try_from(self.state.len().saturating_mul(size_of::<u64>())).ok()
    }

    pub fn compress(&mut self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        let src_len = as_int(src.len(), "LZ4 input")?;
        let dst_len = as_int(dst.len(), "LZ4 output buffer")?;
        let state = self.state.as_mut_ptr().cast::<c_void>();
        let produced = match self.mode {
            // SAFETY: the state holds what the library reported it needs, and both buffers
            // are live for their stated lengths.
            Mode::Fast(acceleration) => unsafe {
                LZ4_compress_fast_extState(
                    state,
                    src.as_ptr().cast::<c_char>(),
                    dst.as_mut_ptr().cast::<c_char>(),
                    src_len,
                    dst_len,
                    acceleration,
                )
            },
            // SAFETY: the state holds what the library reported it needs, and both buffers
            // are live for their stated lengths.
            Mode::High(level) => unsafe {
                LZ4_compress_HC_extStateHC(
                    state,
                    src.as_ptr().cast::<c_char>(),
                    dst.as_mut_ptr().cast::<c_char>(),
                    src_len,
                    dst_len,
                    level,
                )
            },
        };
        length(produced, "LZ4 compression")
    }

    pub fn decompress(&self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        let src_len = as_int(src.len(), "LZ4 compressed input")?;
        let dst_len = as_int(dst.len(), "LZ4 output buffer")?;
        // SAFETY: the safe decoder writes no more than `dst_len` bytes and reads no more
        // than `src_len`, which are the live lengths of the two buffers.
        let produced = unsafe {
            LZ4_decompress_safe(
                src.as_ptr().cast::<c_char>(),
                dst.as_mut_ptr().cast::<c_char>(),
                src_len,
                dst_len,
            )
        };
        length(produced, "LZ4 decompression")
    }

    /// Streams `src` in chunks through the block streaming interface.
    ///
    /// The stream keeps a window into the chunks it already read, which is why every chunk
    /// is a slice of one buffer that outlives the stream.
    pub fn stream(&self, src: &[u8], chunk: usize, dst: &mut [u8]) -> Result<Vec<Chunk>> {
        let mut stream = Stream::open(self.mode)?;
        let mut chunks = Vec::new();
        let mut read = 0_usize;
        let mut written = 0_usize;
        while read < src.len() {
            let take = chunk.min(src.len().saturating_sub(read));
            let input = src
                .get(read..read.saturating_add(take))
                .ok_or_else(|| Error::measure("the LZ4 stream", "read past its input"))?;
            let room = dst
                .get_mut(written..)
                .ok_or_else(|| Error::measure("the LZ4 stream", "ran out of output room"))?;
            let (produced, elapsed_ns) = clock::time(|| stream.push(input, room));
            let produced = produced?;
            chunks.push(Chunk {
                input_bytes: take,
                output_bytes: produced,
                elapsed_ns,
            });
            read = read.saturating_add(take);
            written = written.saturating_add(produced);
        }
        Ok(chunks)
    }
}

/// One open LZ4 block stream, released when it goes out of scope.
struct Stream {
    handle: *mut c_void,
    mode: Mode,
}

impl Stream {
    fn open(mode: Mode) -> Result<Self> {
        let handle = match mode {
            // SAFETY: the constructor takes no argument and returns a context or null.
            Mode::Fast(_) => unsafe { LZ4_createStream() },
            // SAFETY: the constructor takes no argument and returns a context or null.
            Mode::High(_) => unsafe { LZ4_createStreamHC() },
        };
        if handle.is_null() {
            return Err(Error::measure(
                "the LZ4 stream",
                "could not be created by the library",
            ));
        }
        if let Mode::High(level) = mode {
            // SAFETY: `handle` is the context the constructor above returned.
            unsafe { LZ4_resetStreamHC_fast(handle, level) };
        }
        Ok(Self { handle, mode })
    }

    fn push(&mut self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        let src_len = as_int(src.len(), "LZ4 stream chunk")?;
        let dst_len = as_int(dst.len(), "LZ4 stream output room")?;
        let produced = match self.mode {
            // SAFETY: `handle` is a live stream, and both buffers are live for their lengths.
            Mode::Fast(acceleration) => unsafe {
                LZ4_compress_fast_continue(
                    self.handle,
                    src.as_ptr().cast::<c_char>(),
                    dst.as_mut_ptr().cast::<c_char>(),
                    src_len,
                    dst_len,
                    acceleration,
                )
            },
            // SAFETY: `handle` is a live stream, and both buffers are live for their lengths.
            Mode::High(_) => unsafe {
                LZ4_compress_HC_continue(
                    self.handle,
                    src.as_ptr().cast::<c_char>(),
                    dst.as_mut_ptr().cast::<c_char>(),
                    src_len,
                    dst_len,
                )
            },
        };
        length(produced, "LZ4 stream chunk")
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        match self.mode {
            Mode::Fast(_) => {
                // SAFETY: `handle` is the context this stream opened and has not released.
                let _ = unsafe { LZ4_freeStream(self.handle) };
            }
            Mode::High(_) => {
                // SAFETY: `handle` is the context this stream opened and has not released.
                let _ = unsafe { LZ4_freeStreamHC(self.handle) };
            }
        }
    }
}

/// The length an LZ4 entry point produced, or the failure it reported.
///
/// Every one of them returns a positive length on success and zero or less on failure. A
/// successful compression of even an empty input produces at least one byte, so zero is
/// never a length here.
fn length(produced: c_int, what: &str) -> Result<usize> {
    usize::try_from(produced)
        .ok()
        .filter(|length| *length > 0)
        .ok_or_else(|| {
            Error::measure(
                format!("the {what}"),
                format!("failed, and the library returned {produced}"),
            )
        })
}
