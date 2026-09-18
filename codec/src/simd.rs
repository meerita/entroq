//! Owns CPU dispatch and the vector kernels behind it.
//!
//! This module does not own portable codec code. No intrinsic type crosses this boundary, and
//! every kernel keeps the scalar implementation that is its oracle.
//!
//! One kernel is defined here: the common prefix of two positions in one buffer, which is
//! where a match finder spends most of its time. Three implementations answer to it.
//!
//! ```text
//! Scalar   one byte per step                            every target    the oracle
//! Word     one 64-bit load per step, xor, count         every target    the default
//! Neon     one 16-byte load per step, compare, narrow   aarch64 only    the default there
//! ```
//!
//! A build that does not target aarch64 has no `Neon` variant at all, so no figure and no
//! result can be attributed to a vector kernel the build does not contain.
//!
//! The selection is made once, from the target the build was compiled for, and a caller may
//! force any kernel the build holds. Dispatch changes speed and never bytes, and the
//! differential test against the oracle is what says so.
//!
//! Only one architecture has ranked its kernels. On aarch64 the three were measured against
//! each other on an Apple M1 Pro and the vector kernel won, so it is the default there. No
//! other architecture has been measured at all, so the default there is the oracle: it makes
//! no claim, and the word kernel waits for the measurement that would admit it.

/// Which common-prefix kernel a caller runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kernel {
    /// One byte per step. Short enough to check by reading, and the oracle the others answer
    /// to.
    Scalar,
    /// Eight bytes per step, then a byte tail.
    Word,
    /// Sixteen bytes per step in ARM64 NEON, then the word body, then a byte tail.
    #[cfg(target_arch = "aarch64")]
    Neon,
}

/// The kernel this build runs unless a caller names another.
#[cfg(target_arch = "aarch64")]
pub const SELECTED: Kernel = Kernel::Neon;

/// The kernel this build runs unless a caller names another.
///
/// The oracle, on a target whose kernels no measurement has ranked. The word kernel is present
/// and a caller may name it, but nothing has yet compared the two on this architecture, and a
/// kernel is selected from a measurement rather than from the expectation that what won
/// elsewhere wins here. Until that measurement exists this target runs the implementation that
/// claims nothing.
#[cfg(not(target_arch = "aarch64"))]
pub const SELECTED: Kernel = Kernel::Scalar;

/// Every kernel this build holds, the oracle first.
#[cfg(target_arch = "aarch64")]
pub const ALL: &[Kernel] = &[Kernel::Scalar, Kernel::Word, Kernel::Neon];

/// Every kernel this build holds, the oracle first.
#[cfg(not(target_arch = "aarch64"))]
pub const ALL: &[Kernel] = &[Kernel::Scalar, Kernel::Word];

/// The bytes a kernel may read past the logical end of the buffer.
///
/// Zero, for every kernel. A wide load is issued only while the whole load lies inside the
/// bound the kernel clamped its cap to, so the widest load ends at or before the slice end.
/// No caller owes this module slack past a buffer.
pub const OVER_READ_BYTES: usize = 0;

impl Kernel {
    /// The name a record states this kernel by.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Scalar => "scalar-byte",
            Self::Word => "scalar-word8",
            #[cfg(target_arch = "aarch64")]
            Self::Neon => "neon-16",
        }
    }

    /// The widest load the kernel issues, in bytes.
    #[must_use]
    pub const fn widest_load_bytes(self) -> usize {
        match self {
            Self::Scalar => 1,
            Self::Word => 8,
            #[cfg(target_arch = "aarch64")]
            Self::Neon => 16,
        }
    }
}

/// The leading bytes that agree at `a` and at `b`, at most `cap` of them.
///
/// `earlier` and `later` are positions in the same buffer. The comparison may run past `later`,
/// which is the
/// self-referential copy an overlapping match describes, and a decoder reconstructing that copy
/// produces the same bytes.
///
/// The cap is clamped to what the buffer holds, so no argument can drive a read past the end.
#[must_use]
pub fn common_prefix(
    kernel: Kernel,
    data: &[u8],
    earlier: usize,
    later: usize,
    cap: usize,
) -> usize {
    match kernel {
        Kernel::Scalar => scalar(data, earlier, later, cap),
        Kernel::Word => word(data, earlier, later, cap),
        #[cfg(target_arch = "aarch64")]
        Kernel::Neon => neon(data, earlier, later, cap),
    }
}

/// The oracle. One byte per step, correct by inspection.
#[must_use]
pub fn scalar(data: &[u8], earlier: usize, later: usize, cap: usize) -> usize {
    let cap = bounded(data, earlier, later, cap);
    tail(data, earlier, later, cap, 0)
}

/// Eight bytes per step, then the byte tail.
#[must_use]
pub fn word(data: &[u8], earlier: usize, later: usize, cap: usize) -> usize {
    let cap = bounded(data, earlier, later, cap);
    let mut matched = 0usize;
    while matched.saturating_add(8) <= cap {
        let (Some(left), Some(right)) = (
            read_u64(data, earlier, matched),
            read_u64(data, later, matched),
        ) else {
            break;
        };
        if left != right {
            return matched.saturating_add(first_differing_byte(left ^ right));
        }
        matched = matched.saturating_add(8);
    }
    tail(data, earlier, later, cap, matched)
}

/// Sixteen bytes per step in ARM64 NEON, then the word body, then the byte tail.
///
/// The intrinsic form was written only after a sixteen-byte step had been measured to beat an
/// eight-byte one, and only after the safe sixteen-byte formulation was found to compile to
/// paired 64-bit general-purpose loads with no vector instruction in it at all. Against that
/// safe formulation this kernel costs 0.63 at a mean matched length near zero and 0.47 at the
/// length cap, and it beats the word kernel at both ends of the range. Its one loss region is
/// matches of roughly four to eleven bytes, which is narrow and real.
///
/// Those ratios were measured on an Apple M1 Pro, on darwin, at 8 performance cores and a
/// 128-byte cache line, with no cycle counter available: the figures are wall-clock
/// nanoseconds per call over 36 workloads at 5 samples each. A ratio between two kernels is
/// not portable across microarchitectures, and no other part has been measured.
///
/// `vceqq_u8` gives `0xFF` per equal byte and `0x00` per differing one. `vshrn_n_u16` at shift
/// 4 narrows each 16-bit lane to its low byte after a 4-bit right shift, which leaves one
/// nibble per input byte and loses no byte's result, so the first differing byte sits at the
/// count of trailing one bits divided by four. The reduction is exact, which is why this
/// kernel is required to equal the oracle rather than to agree with it usually.
// The only `unsafe` in this crate. It is admitted because the safe implementations exist
// beside it and are tested, because the measurement that a sixteen-byte step wins was taken
// before the intrinsic was written, and because the compiler does not reach this code on its
// own: the safe sixteen-byte formulation the measurement started from compiled to paired
// 64-bit general-purpose loads and emitted no vector instruction at all. The scalar kernel
// stays in the tree as the oracle and the differential test compares this path against it.
// Each block below carries one safety argument that covers every operation inside it, so
// grouping them keeps the argument whole instead of repeating it per instruction.
#[allow(unsafe_code, clippy::multiple_unsafe_ops_per_block)]
#[cfg(target_arch = "aarch64")]
#[must_use]
pub fn neon(data: &[u8], earlier: usize, later: usize, cap: usize) -> usize {
    use core::arch::aarch64::{
        vceqq_u8, vget_lane_u64, vld1q_u8, vreinterpret_u64_u8, vreinterpretq_u16_u8, vshrn_n_u16,
    };

    let cap = bounded(data, earlier, later, cap);
    let base = data.as_ptr();
    let mut matched = 0usize;
    while matched.saturating_add(16) <= cap {
        let (Some(at_earlier), Some(at_later)) =
            (earlier.checked_add(matched), later.checked_add(matched))
        else {
            break;
        };
        let from_earlier = base.wrapping_add(at_earlier);
        let from_later = base.wrapping_add(at_later);
        // SAFETY: both loads read sixteen bytes from a pointer, and both pointers are in
        // range. `bounded` clamped `cap` to `data.len() - max(a, b)` and the loop bound is
        // `i + 16 <= cap`, so `at_earlier + 16` and `at_later + 16` are both at or below `data.len()`
        // and each load lies wholly inside the slice. Each pointer is `data.as_ptr()`
        // advanced by an offset proved in range above, and this load requires no alignment
        // on this target. Both operations carry that one argument, which is why they share
        // one block.
        let (left, right) = unsafe { (vld1q_u8(from_earlier), vld1q_u8(from_later)) };
        // SAFETY: the reduction touches registers and no memory, so its only requirement is
        // that the instruction set is present. Advanced SIMD is part of the base AArch64
        // architecture and this function exists only on that target, so the requirement holds
        // for every operation in the block.
        let mask = unsafe {
            vget_lane_u64::<0>(vreinterpret_u64_u8(vshrn_n_u16::<4>(vreinterpretq_u16_u8(
                vceqq_u8(left, right),
            ))))
        };
        if mask != u64::MAX {
            return matched.saturating_add(mask.trailing_ones() as usize / 4);
        }
        matched = matched.saturating_add(16);
    }
    while matched.saturating_add(8) <= cap {
        let (Some(left), Some(right)) = (
            read_u64(data, earlier, matched),
            read_u64(data, later, matched),
        ) else {
            break;
        };
        if left != right {
            return matched.saturating_add(first_differing_byte(left ^ right));
        }
        matched = matched.saturating_add(8);
    }
    tail(data, earlier, later, cap, matched)
}

/// The cap a kernel may actually compare over.
///
/// Every kernel clamps here first, so the over-read guard is a property of this function and
/// of each loop bound rather than an obligation a caller can forget.
fn bounded(data: &[u8], earlier: usize, later: usize, cap: usize) -> usize {
    cap.min(data.len().saturating_sub(earlier.max(later)))
}

/// The byte body, from `from` to `cap`.
fn tail(data: &[u8], earlier: usize, later: usize, cap: usize, from: usize) -> usize {
    let mut matched = from;
    while matched < cap {
        let (Some(&left), Some(&right)) = (
            data.get(earlier.saturating_add(matched)),
            data.get(later.saturating_add(matched)),
        ) else {
            break;
        };
        if left != right {
            break;
        }
        matched = matched.saturating_add(1);
    }
    matched
}

/// Eight bytes of `data` at `at + offset`, or `None` when the slice does not hold them.
fn read_u64(data: &[u8], at: usize, offset: usize) -> Option<u64> {
    let start = at.checked_add(offset)?;
    let end = start.checked_add(8)?;
    let bytes: [u8; 8] = data.get(start..end)?.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

/// The index of the lowest differing byte of a non-zero exclusive or.
///
/// The operands were read with `from_le_bytes`, so byte 0 of the value is byte 0 of the
/// buffer whatever the host's byte order is.
const fn first_differing_byte(difference: u64) -> usize {
    difference.trailing_zeros() as usize / 8
}

#[cfg(test)]
mod tests {
    use super::{ALL, Kernel, common_prefix, scalar};

    /// The comparisons the generated set must reach.
    ///
    /// One case is one triple of two positions and a cap, and every kernel answers it, so the
    /// count is the comparisons each kernel made against the oracle and not the calls the set
    /// made in total.
    const DIFFERENTIAL_CASES: usize = 1_000_000;

    /// The alignments the constructed set places a position at.
    const ALIGNMENTS: usize = 16;

    /// The lengths the constructed set builds a match of.
    const LENGTHS: usize = 257;

    /// The seed the generated set is drawn from.
    ///
    /// A constant rather than a clock reading, so a failure reproduces from the test name
    /// alone.
    const SEED: u64 = 0x5EED_0000_0000_0004;

    struct Source(u64);

    impl Source {
        const fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(0x5851_F42D_4C95_7F2D)
                .wrapping_add(0x1405_7B7E_F767_814F);
            self.0
        }

        fn byte(&mut self) -> u8 {
            super::super::entropy::low_byte(self.next() >> 24)
        }

        fn below(&mut self, bound: usize) -> usize {
            if bound == 0 {
                0
            } else {
                usize::try_from(self.next() >> 16)
                    .unwrap_or(0)
                    .checked_rem(bound)
                    .unwrap_or(0)
            }
        }
    }

    /// One constructed case: a buffer, two positions, and the prefix they share.
    ///
    /// The two positions do not overlap, `later` ends the buffer exactly, and the byte after
    /// the shared prefix differs, so the answer is the constructed length whenever the cap
    /// admits it.
    fn constructed(
        length: usize,
        align_earlier: usize,
        align_later: usize,
    ) -> (Vec<u8>, usize, usize) {
        let earlier = align_earlier;
        let mut later = earlier.saturating_add(length).saturating_add(1);
        while later.checked_rem(ALIGNMENTS).unwrap_or(0) != align_later {
            later = later.saturating_add(1);
        }
        let len = later.saturating_add(length).saturating_add(1);
        let mut source = Source(SEED ^ (length as u64) ^ ((align_later as u64) << 32));
        let mut data: Vec<u8> = (0..len).map(|_| source.byte()).collect();
        for step in 0..length {
            let byte = data.get(later.saturating_add(step)).copied().unwrap_or(0);
            if let Some(slot) = data.get_mut(earlier.saturating_add(step)) {
                *slot = byte;
            }
        }
        let after = data.get(later.saturating_add(length)).copied().unwrap_or(0);
        if let Some(slot) = data.get_mut(earlier.saturating_add(length)) {
            *slot = after ^ 0xFF;
        }
        (data, earlier, later)
    }

    /// The buffers the generated set draws positions from.
    ///
    /// Four characters, so the generated set exercises an immediate mismatch, a periodic
    /// input that keeps the wide step firing, text-like content, and the input that drives
    /// every comparison to the cap.
    fn buffers() -> Vec<Vec<u8>> {
        let mut source = Source(SEED);
        let random: Vec<u8> = (0..8192).map(|_| source.byte()).collect();
        let periodic: Vec<u8> = (0..8192u32)
            .map(|at| super::super::entropy::low_byte(u64::from(at.checked_rem(16).unwrap_or(0))))
            .collect();
        let textlike: Vec<u8> = (0..8192)
            .map(|_| {
                let pick = source.below(8);
                b"the quick brown fox jumps over "
                    .get(pick.saturating_mul(3).checked_rem(31).unwrap_or(0))
                    .copied()
                    .unwrap_or(b' ')
            })
            .collect();
        let zeros = vec![0u8; 8192];
        vec![random, periodic, textlike, zeros]
    }

    #[test]
    fn every_kernel_agrees_with_the_oracle_on_every_length_and_alignment() {
        let mut cases = 0usize;
        for length in 0..LENGTHS {
            for align_earlier in 0..ALIGNMENTS {
                for align_later in 0..ALIGNMENTS {
                    let (data, earlier, later) = constructed(length, align_earlier, align_later);
                    for cap in [length, length.saturating_add(1), 256] {
                        let expected = scalar(&data, earlier, later, cap);
                        assert_eq!(expected, length.min(cap), "length {length} cap {cap}");
                        for &kernel in ALL {
                            assert_eq!(
                                common_prefix(kernel, &data, earlier, later, cap),
                                expected,
                                "{} at length {length}, alignments {align_earlier} and \
                                 {align_later}, cap {cap}",
                                kernel.name()
                            );
                            cases = cases.saturating_add(1);
                        }
                    }
                }
            }
        }
        assert_eq!(cases, LENGTHS * ALIGNMENTS * ALIGNMENTS * 3 * ALL.len());
    }

    #[test]
    fn every_kernel_agrees_with_the_oracle_over_a_million_generated_comparisons() {
        let buffers = buffers();
        let mut source = Source(SEED ^ 0xA5A5_A5A5);
        let mut cases = 0usize;
        while cases < DIFFERENTIAL_CASES {
            let data = buffers
                .get(source.below(buffers.len()))
                .map_or(&[][..], Vec::as_slice);
            let later = source.below(data.len());
            let earlier = if later == 0 { 0 } else { source.below(later) };
            let cap = source.below(257);
            let expected = scalar(data, earlier, later, cap);
            for &kernel in ALL {
                assert_eq!(
                    common_prefix(kernel, data, earlier, later, cap),
                    expected,
                    "{} at {earlier} and {later}, cap {cap}",
                    kernel.name()
                );
            }
            cases = cases.saturating_add(1);
        }
        assert_eq!(cases, DIFFERENTIAL_CASES);
    }

    #[test]
    fn a_cap_past_the_buffer_reads_nothing_past_it() {
        let data = vec![7u8; 64];
        for &kernel in ALL {
            assert_eq!(common_prefix(kernel, &data, 0, 32, usize::MAX), 32);
            assert_eq!(common_prefix(kernel, &data, 0, 64, 16), 0);
            assert_eq!(common_prefix(kernel, &[], 0, 0, 16), 0);
        }
    }

    #[test]
    fn an_overlapping_comparison_reports_the_self_referential_length() {
        let data = vec![9u8; 512];
        for &kernel in ALL {
            assert_eq!(common_prefix(kernel, &data, 0, 1, 256), 256);
            assert_eq!(common_prefix(kernel, &data, 250, 251, 256), 256);
        }
    }

    #[test]
    fn the_selected_kernel_is_the_one_this_build_holds() {
        assert!(ALL.contains(&super::SELECTED));
        assert_eq!(Kernel::Scalar.widest_load_bytes(), 1);
        assert_eq!(super::OVER_READ_BYTES, 0);
    }
}
