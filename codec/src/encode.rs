//! Owns the encoder engine: the path from input bytes to a compliant stream.
//!
//! This module does not own the format contract, match finding, parse strategy, or entropy
//! coding. It composes them.
