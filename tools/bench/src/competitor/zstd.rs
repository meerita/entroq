//! Owns the Zstandard boundary.
//!
//! Every unsafe operation in this module is a call across the C boundary of the linked
//! library. There is no safe way to reach a C library, and nothing here is an optimization.
//!
//! The measured format is the Zstandard frame, which is the only format the library
//! produces. The one-shot path and the streaming path produce the same format, so a
//! streaming number and a ratio number describe the same work.
//!
//! The contexts are created with the harness allocator, so every byte Zstandard allocates is
//! counted by the same counter that counts the harness. They are reused across samples,
//! which is what a caller that compresses more than once does, so a steady-state sample
//! measures compression and not context creation. The allocation metric opens a fresh
//! session inside its own interval, which is what makes it a cold number.
#![allow(unsafe_code)]
// Every method takes a receiver because the dispatching enum calls them uniformly.
#![allow(clippy::unused_self)]

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};

use super::{Chunk, Method, Notes, parameter};
use crate::alloc;
use crate::clock;
use crate::error::{Error, Result};

/// The three function pointers Zstandard takes to allocate with.
#[repr(C)]
struct CustomMem {
    alloc: Option<unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void>,
    free: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    opaque: *mut c_void,
}

/// The buffer descriptor the streaming interface reads from.
#[repr(C)]
struct InBuffer {
    src: *const c_void,
    size: usize,
    pos: usize,
}

/// The buffer descriptor the streaming interface writes to.
#[repr(C)]
struct OutBuffer {
    dst: *mut c_void,
    size: usize,
    pos: usize,
}

/// `ZSTD_c_compressionLevel`.
const PARAM_COMPRESSION_LEVEL: c_int = 100;
/// `ZSTD_c_nbWorkers`.
const PARAM_WORKERS: c_int = 400;
/// `ZSTD_e_continue`.
const END_CONTINUE: c_int = 0;
/// `ZSTD_e_end`.
const END_END: c_int = 2;

unsafe extern "C" {
    fn ZSTD_versionString() -> *const c_char;
    fn ZSTD_compressBound(src_size: usize) -> usize;
    fn ZSTD_isError(code: usize) -> c_uint;
    fn ZSTD_getErrorName(code: usize) -> *const c_char;
    fn ZSTD_createCCtx_advanced(custom: CustomMem) -> *mut c_void;
    fn ZSTD_freeCCtx(cctx: *mut c_void) -> usize;
    fn ZSTD_createDCtx_advanced(custom: CustomMem) -> *mut c_void;
    fn ZSTD_freeDCtx(dctx: *mut c_void) -> usize;
    fn ZSTD_CCtx_setParameter(cctx: *mut c_void, param: c_int, value: c_int) -> usize;
    fn ZSTD_sizeof_CCtx(cctx: *const c_void) -> usize;
    fn ZSTD_sizeof_DCtx(dctx: *const c_void) -> usize;
    fn ZSTD_compress2(
        cctx: *mut c_void,
        dst: *mut c_void,
        dst_capacity: usize,
        src: *const c_void,
        src_size: usize,
    ) -> usize;
    fn ZSTD_decompressDCtx(
        dctx: *mut c_void,
        dst: *mut c_void,
        dst_capacity: usize,
        src: *const c_void,
        src_size: usize,
    ) -> usize;
    fn ZSTD_compressStream2(
        cctx: *mut c_void,
        output: *mut OutBuffer,
        input: *mut InBuffer,
        end_op: c_int,
    ) -> usize;
}

/// The allocator every context in this module is created with.
const fn harness_allocator() -> CustomMem {
    CustomMem {
        alloc: Some(alloc::reserve),
        free: Some(alloc::release),
        opaque: std::ptr::null_mut(),
    }
}

pub struct Session {
    level: c_int,
    cctx: Context,
    dctx: Context,
}

impl Session {
    pub fn open(point: &str) -> Result<Self> {
        let level = parameter(point, "level-").ok_or_else(|| {
            Error::measure(
                format!("the Zstandard operating point `{point}`"),
                "does not name a compression level",
            )
        })?;
        // SAFETY: the allocator pair is the one this module publishes, and the opaque
        // pointer is the null the library passes back unread.
        let created = unsafe { ZSTD_createCCtx_advanced(harness_allocator()) };
        let cctx = Context::hold(created, Kind::Compress)?;
        // SAFETY: as above.
        let created = unsafe { ZSTD_createDCtx_advanced(harness_allocator()) };
        let dctx = Context::hold(created, Kind::Decompress)?;
        // SAFETY: `cctx` is the live context created above.
        let set = unsafe { ZSTD_CCtx_setParameter(cctx.handle, PARAM_COMPRESSION_LEVEL, level) };
        let _ = check(set, "setting the Zstandard compression level")?;
        Ok(Self { level, cctx, dctx })
    }

    pub fn linked_version(&self) -> Option<String> {
        // SAFETY: the call takes no argument.
        let pointer = unsafe { ZSTD_versionString() };
        // SAFETY: the pointer is the library's own static, NUL-terminated string.
        let text = unsafe { CStr::from_ptr(pointer) };
        text.to_str().ok().map(String::from)
    }

    pub const fn notes(&self) -> Notes {
        Notes {
            format: "the Zstandard frame format, which is the only format the library \
                     produces. The one-shot and streaming paths produce the same format.",
            integrity: "None. ZSTD_c_checksumFlag is left at the library default of 0, so \
                        the frame carries no content checksum. The frame header is still \
                        produced, and its bytes are counted in the compressed length.",
            state_bytes: Method::By("ZSTD_sizeof_CCtx and ZSTD_sizeof_DCtx"),
            allocations: Method::By(
                "ZSTD_createCCtx_advanced and ZSTD_createDCtx_advanced, which take the \
                 harness allocator",
            ),
            streaming: Method::By("ZSTD_compressStream2"),
            threads: Method::By("the ZSTD_c_nbWorkers parameter"),
        }
    }

    pub fn compress_bound(&self, input: usize) -> usize {
        // SAFETY: the bound is a pure function of the length.
        unsafe { ZSTD_compressBound(input) }
    }

    pub fn encoder_state_bytes(&self) -> Option<u64> {
        // SAFETY: `cctx` is the live context this session owns.
        u64::try_from(unsafe { ZSTD_sizeof_CCtx(self.cctx.handle) }).ok()
    }

    pub fn decoder_state_bytes(&self) -> Option<u64> {
        // SAFETY: `dctx` is the live context this session owns.
        u64::try_from(unsafe { ZSTD_sizeof_DCtx(self.dctx.handle) }).ok()
    }

    pub fn compress(&mut self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        // SAFETY: the context is live and both buffers are live for their stated lengths.
        let produced = unsafe {
            ZSTD_compress2(
                self.cctx.handle,
                dst.as_mut_ptr().cast::<c_void>(),
                dst.len(),
                src.as_ptr().cast::<c_void>(),
                src.len(),
            )
        };
        check(produced, "the Zstandard compression")
    }

    pub fn decompress(&mut self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        // SAFETY: the context is live and both buffers are live for their stated lengths.
        let produced = unsafe {
            ZSTD_decompressDCtx(
                self.dctx.handle,
                dst.as_mut_ptr().cast::<c_void>(),
                dst.len(),
                src.as_ptr().cast::<c_void>(),
                src.len(),
            )
        };
        check(produced, "the Zstandard decompression")
    }

    pub fn stream(&self, src: &[u8], chunk: usize, dst: &mut [u8]) -> Result<Vec<Chunk>> {
        // A streamed compression starts a new frame, and the context carries the state of
        // the last one until it is told to end.
        let mut output = OutBuffer {
            dst: dst.as_mut_ptr().cast::<c_void>(),
            size: dst.len(),
            pos: 0,
        };
        let mut chunks = Vec::new();
        let mut read = 0_usize;
        while read < src.len() {
            let take = chunk.min(src.len().saturating_sub(read));
            let end = read.saturating_add(take);
            let last = end >= src.len();
            let slice = src
                .get(read..end)
                .ok_or_else(|| Error::measure("the Zstandard stream", "read past its input"))?;
            let before = output.pos;
            let (outcome, elapsed_ns) = clock::time(|| {
                push(
                    self.cctx.handle,
                    slice,
                    &mut output,
                    if last { END_END } else { END_CONTINUE },
                )
            });
            outcome?;
            chunks.push(Chunk {
                input_bytes: take,
                output_bytes: output.pos.saturating_sub(before),
                elapsed_ns,
            });
            read = end;
        }
        Ok(chunks)
    }

    /// Compresses with `workers` worker threads, through a context of its own.
    ///
    /// The session's own context keeps its thread count, so a threaded measurement cannot
    /// change what a later single-threaded measurement reports.
    pub fn threaded(&self, workers: u32, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        // SAFETY: the allocator pair is the one this module publishes.
        let created = unsafe { ZSTD_createCCtx_advanced(harness_allocator()) };
        let cctx = Context::hold(created, Kind::Compress)?;
        let level = self.level;
        // SAFETY: `cctx` is the live context created above.
        let set = unsafe { ZSTD_CCtx_setParameter(cctx.handle, PARAM_COMPRESSION_LEVEL, level) };
        let _ = check(set, "setting the Zstandard compression level")?;
        let count = c_int::try_from(workers)
            .map_err(|_| Error::measure("the Zstandard thread count", "is not an int"))?;
        // SAFETY: `cctx` is the live context created above.
        let set = unsafe { ZSTD_CCtx_setParameter(cctx.handle, PARAM_WORKERS, count) };
        let _ = check(
            set,
            "setting the Zstandard worker count, which a build without multithreading refuses",
        )?;
        // SAFETY: the context is live and both buffers are live for their stated lengths.
        let produced = unsafe {
            ZSTD_compress2(
                cctx.handle,
                dst.as_mut_ptr().cast::<c_void>(),
                dst.len(),
                src.as_ptr().cast::<c_void>(),
                src.len(),
            )
        };
        check(produced, "the threaded Zstandard compression")
    }
}

/// Drives one streaming call until the library has taken the whole chunk.
fn push(cctx: *mut c_void, src: &[u8], output: &mut OutBuffer, end_op: c_int) -> Result<()> {
    let mut input = InBuffer {
        src: src.as_ptr().cast::<c_void>(),
        size: src.len(),
        pos: 0,
    };
    loop {
        // SAFETY: the context is live, and both descriptors point at live buffers.
        let remaining = unsafe { ZSTD_compressStream2(cctx, output, &raw mut input, end_op) };
        let _ = check(remaining, "the Zstandard stream")?;
        if input.pos >= input.size && (end_op != END_END || remaining == 0) {
            return Ok(());
        }
        if output.pos >= output.size {
            return Err(Error::measure(
                "the Zstandard stream",
                "ran out of output room before it took the whole chunk",
            ));
        }
    }
}

/// Which constructor produced a context, so the matching release is called.
#[derive(Clone, Copy)]
enum Kind {
    Compress,
    Decompress,
}

/// One Zstandard context, released when it goes out of scope.
struct Context {
    handle: *mut c_void,
    kind: Kind,
}

impl Context {
    fn hold(handle: *mut c_void, kind: Kind) -> Result<Self> {
        if handle.is_null() {
            return Err(Error::measure(
                "the Zstandard context",
                "could not be created by the library",
            ));
        }
        Ok(Self { handle, kind })
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        match self.kind {
            Kind::Compress => {
                // SAFETY: `handle` is the context this value holds and has not released.
                let _ = unsafe { ZSTD_freeCCtx(self.handle) };
            }
            Kind::Decompress => {
                // SAFETY: `handle` is the context this value holds and has not released.
                let _ = unsafe { ZSTD_freeDCtx(self.handle) };
            }
        }
    }
}

/// The length a Zstandard entry point produced, or the failure it named.
fn check(code: usize, what: &str) -> Result<usize> {
    // SAFETY: the predicate reads only the code it was given.
    if unsafe { ZSTD_isError(code) } == 0 {
        return Ok(code);
    }
    // SAFETY: the call reads only the code it was given.
    let pointer = unsafe { ZSTD_getErrorName(code) };
    // SAFETY: the pointer is the library's own static, NUL-terminated string.
    let text = unsafe { CStr::from_ptr(pointer) };
    Err(Error::measure(
        format!("the {what}"),
        format!("failed: {}", text.to_string_lossy()),
    ))
}
