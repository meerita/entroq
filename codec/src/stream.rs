//! Owns the streaming state machine: incremental input, incremental output, explicit finish,
//! and the buffer budget the machine holds.
//!
//! This module does not own encode or decode logic. It drives them within a declared bound.
