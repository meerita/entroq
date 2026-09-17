//! Owns the Entroq codec.
//!
//! The crate exports the format contract, the entropy coding the format's tables are declared
//! and validated through, the sequence representation their symbols are drawn from, the
//! compressed block that carries all three, the match finder and the parser that produce the
//! sequences, the kernel dispatch the finder runs on, and the streaming pair that compresses
//! through every block type the format defines and reads every one back. Every other module is
//! private until the mechanism it owns is designed and measured.

pub mod block;
mod checksum;
mod decode;
mod encode;
pub mod entropy;
pub mod format;
mod index;
pub mod matchfinder;
pub mod parser;
pub mod sequence;
pub mod simd;
pub mod stream;
