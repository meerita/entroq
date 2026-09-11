//! Owns the decoder engine: the path from a validated stream back to the original bytes.
//!
//! This module does not own field validation, which the format module performs once at its
//! boundary, or the streaming state machine that drives it.
