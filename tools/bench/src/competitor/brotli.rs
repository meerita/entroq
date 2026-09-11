//! Owns the Brotli boundary.
//!
//! Every unsafe operation in this module is a call across the C boundary of the linked
//! library. There is no safe way to reach a C library, and nothing here is an optimization.
//!
//! Brotli's encoder and decoder instances carry the state of one stream and are not reusable
//! once that stream finishes, so a compression creates its instance and releases it. That is
//! what the project's own one-shot entry point does, so the measured work is the work a
//! caller pays.
//!
//! Both instances are created with the harness allocator, so every byte Brotli allocates is
//! counted by the same counter that counts the harness. The project publishes no state-size
//! call, so the bytes the codec owns come from that counter instead.
#![allow(unsafe_code)]
// Every method takes a receiver because the dispatching enum calls them uniformly, and
// every one that can fail for another competitor keeps the same shape here.
#![allow(clippy::unused_self, clippy::unnecessary_wraps)]

use std::ffi::c_void;

use super::{Chunk, Method, Notes, parameter};
use crate::alloc;
use crate::clock;
use crate::error::{Error, Result};

/// `BROTLI_PARAM_QUALITY`.
const PARAM_QUALITY: u32 = 1;
/// `BROTLI_PARAM_SIZE_HINT`, which lets the encoder size its window for the input it will
/// see. Leaving it unset makes a large input look like an unknown one.
const PARAM_SIZE_HINT: u32 = 5;
/// `BROTLI_OPERATION_PROCESS`.
const OPERATION_PROCESS: u32 = 0;
/// `BROTLI_OPERATION_FINISH`.
const OPERATION_FINISH: u32 = 2;
/// `BROTLI_DECODER_RESULT_SUCCESS`.
const DECODER_SUCCESS: u32 = 1;

type AllocFn = Option<unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void>;
type FreeFn = Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>;

unsafe extern "C" {
    fn BrotliEncoderVersion() -> u32;
    fn BrotliEncoderMaxCompressedSize(input_size: usize) -> usize;
    fn BrotliEncoderCreateInstance(
        alloc: AllocFn,
        free: FreeFn,
        opaque: *mut c_void,
    ) -> *mut c_void;
    fn BrotliEncoderDestroyInstance(state: *mut c_void);
    fn BrotliEncoderSetParameter(state: *mut c_void, param: u32, value: u32) -> i32;
    fn BrotliEncoderCompressStream(
        state: *mut c_void,
        operation: u32,
        available_in: *mut usize,
        next_in: *mut *const u8,
        available_out: *mut usize,
        next_out: *mut *mut u8,
        total_out: *mut usize,
    ) -> i32;
    fn BrotliEncoderIsFinished(state: *mut c_void) -> i32;
    fn BrotliDecoderCreateInstance(
        alloc: AllocFn,
        free: FreeFn,
        opaque: *mut c_void,
    ) -> *mut c_void;
    fn BrotliDecoderDestroyInstance(state: *mut c_void);
    fn BrotliDecoderDecompressStream(
        state: *mut c_void,
        available_in: *mut usize,
        next_in: *mut *const u8,
        available_out: *mut usize,
        next_out: *mut *mut u8,
        total_out: *mut usize,
    ) -> u32;
}

pub struct Session {
    quality: u32,
}

impl Session {
    pub fn open(point: &str) -> Result<Self> {
        let quality = parameter(point, "q")
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                Error::measure(
                    format!("the Brotli operating point `{point}`"),
                    "does not name a quality",
                )
            })?;
        Ok(Self { quality })
    }

    pub fn linked_version(&self) -> Option<String> {
        // SAFETY: the call takes no argument and returns a packed version number.
        let packed = unsafe { BrotliEncoderVersion() };
        let major = packed >> 24;
        let minor = (packed >> 12) & 0xFFF;
        let patch = packed & 0xFFF;
        Some(format!("{major}.{minor}.{patch}"))
    }

    pub const fn notes(&self) -> Notes {
        Notes {
            format: "the Brotli stream, as BrotliEncoderCompressStream produces it. The \
                     window is left at the encoder's own default for the quality.",
            state_bytes: Method::By(
                "the harness allocator, which BrotliEncoderCreateInstance is given. The \
                 project publishes no state-size call.",
            ),
            allocations: Method::By(
                "the harness allocator, which BrotliEncoderCreateInstance and \
                 BrotliDecoderCreateInstance are given",
            ),
            streaming: Method::By("BrotliEncoderCompressStream"),
            threads: Method::Absent(
                "Brotli publishes no thread parameter. Its library compresses on the \
                 calling thread.",
            ),
        }
    }

    pub fn compress_bound(&self, input: usize) -> usize {
        // SAFETY: the bound is a pure function of the length.
        let bound = unsafe { BrotliEncoderMaxCompressedSize(input) };
        // The call reports zero for an input larger than the format's one-shot limit, and a
        // streamed compression of that input still fits a slightly larger buffer.
        if bound == 0 {
            input.saturating_add(input.div_ceil(4)).saturating_add(1024)
        } else {
            bound
        }
    }

    pub fn compress(&self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        let encoder = Encoder::open(self.quality, src.len())?;
        encoder.run(src, dst, OPERATION_FINISH).map(|(_, out)| out)
    }

    pub fn decompress(&self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        let decoder = Decoder::open()?;
        let mut available_in = src.len();
        let mut next_in = src.as_ptr();
        let mut available_out = dst.len();
        let mut next_out = dst.as_mut_ptr();
        let mut total_out = 0_usize;
        // SAFETY: the instance is live, and every pointer addresses a live local or a live
        // buffer for the length beside it.
        let result = unsafe {
            BrotliDecoderDecompressStream(
                decoder.handle,
                &raw mut available_in,
                &raw mut next_in,
                &raw mut available_out,
                &raw mut next_out,
                &raw mut total_out,
            )
        };
        if result == DECODER_SUCCESS {
            Ok(total_out)
        } else {
            Err(Error::measure(
                "the Brotli decompression",
                format!("returned result {result} rather than success"),
            ))
        }
    }

    pub fn stream(&self, src: &[u8], chunk: usize, dst: &mut [u8]) -> Result<Vec<Chunk>> {
        let encoder = Encoder::open(self.quality, src.len())?;
        let mut chunks = Vec::new();
        let mut read = 0_usize;
        let mut written = 0_usize;
        while read < src.len() {
            let take = chunk.min(src.len().saturating_sub(read));
            let end = read.saturating_add(take);
            let last = end >= src.len();
            let slice = src
                .get(read..end)
                .ok_or_else(|| Error::measure("the Brotli stream", "read past its input"))?;
            let room = dst
                .get_mut(written..)
                .ok_or_else(|| Error::measure("the Brotli stream", "ran out of output room"))?;
            let operation = if last {
                OPERATION_FINISH
            } else {
                OPERATION_PROCESS
            };
            let (outcome, elapsed_ns) = clock::time(|| encoder.run(slice, room, operation));
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

/// One Brotli encoder instance, released when it goes out of scope.
struct Encoder {
    handle: *mut c_void,
}

impl Encoder {
    fn open(quality: u32, size_hint: usize) -> Result<Self> {
        // SAFETY: the allocator pair is the one this module publishes, and the opaque
        // pointer is the null the library passes back unread.
        let handle = unsafe {
            BrotliEncoderCreateInstance(
                Some(alloc::reserve),
                Some(alloc::release),
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() {
            return Err(Error::measure(
                "the Brotli encoder",
                "could not be created by the library",
            ));
        }
        let encoder = Self { handle };
        encoder.set(PARAM_QUALITY, quality)?;
        encoder.set(
            PARAM_SIZE_HINT,
            u32::try_from(size_hint).unwrap_or(u32::MAX),
        )?;
        Ok(encoder)
    }

    fn set(&self, param: u32, value: u32) -> Result<()> {
        // SAFETY: `handle` is the live instance this value holds.
        if unsafe { BrotliEncoderSetParameter(self.handle, param, value) } == 0 {
            return Err(Error::measure(
                "the Brotli encoder",
                format!("refused parameter {param} at value {value}"),
            ));
        }
        Ok(())
    }

    /// Feeds one slice and drains what the encoder produced. Reports what it consumed and
    /// what it wrote.
    fn run(&self, src: &[u8], dst: &mut [u8], operation: u32) -> Result<(usize, usize)> {
        let mut available_in = src.len();
        let mut next_in = src.as_ptr();
        let mut available_out = dst.len();
        let mut next_out = dst.as_mut_ptr();
        let mut total_out = 0_usize;
        loop {
            // SAFETY: the instance is live, and every pointer addresses a live local or a
            // live buffer for the length beside it.
            let ok = unsafe {
                BrotliEncoderCompressStream(
                    self.handle,
                    operation,
                    &raw mut available_in,
                    &raw mut next_in,
                    &raw mut available_out,
                    &raw mut next_out,
                    &raw mut total_out,
                )
            };
            if ok == 0 {
                return Err(Error::measure(
                    "the Brotli compression",
                    "was refused by the encoder",
                ));
            }
            // SAFETY: `handle` is the live instance this value holds.
            let finished = unsafe { BrotliEncoderIsFinished(self.handle) } != 0;
            if available_in == 0 && (operation != OPERATION_FINISH || finished) {
                let consumed = src.len().saturating_sub(available_in);
                return Ok((consumed, dst.len().saturating_sub(available_out)));
            }
            if available_out == 0 {
                return Err(Error::measure(
                    "the Brotli compression",
                    "ran out of output room",
                ));
            }
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: `handle` is the instance this value holds and has not released.
        unsafe { BrotliEncoderDestroyInstance(self.handle) }
    }
}

/// One Brotli decoder instance, released when it goes out of scope.
struct Decoder {
    handle: *mut c_void,
}

impl Decoder {
    fn open() -> Result<Self> {
        // SAFETY: the allocator pair is the one this module publishes, and the opaque
        // pointer is the null the library passes back unread.
        let handle = unsafe {
            BrotliDecoderCreateInstance(
                Some(alloc::reserve),
                Some(alloc::release),
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() {
            return Err(Error::measure(
                "the Brotli decoder",
                "could not be created by the library",
            ));
        }
        Ok(Self { handle })
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: `handle` is the instance this value holds and has not released.
        unsafe { BrotliDecoderDestroyInstance(self.handle) }
    }
}
