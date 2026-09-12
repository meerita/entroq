//! Owns CPU dispatch and the vector kernels behind it.
//!
//! This module does not own portable codec code. No intrinsic type crosses this boundary, and
//! every kernel keeps the scalar implementation that is its oracle.
