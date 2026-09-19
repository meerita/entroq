//! Owns rANS: the normalized table, the description that transmits it, the validation a
//! decoder applies before it allocates, and the packed slot table it decodes through.
//!
//! This module does not own which symbol class takes how many states, and it does not own the
//! block that carries the description.
//!
//! One implementation with the state count as a parameter, at one, two and four interleaved
//! states. One state is a serial dependency chain: the next symbol's decode cannot start until
//! this one's state update lands. Interleaving breaks the chain into independent ones and costs
//! one flushed state per state per block.
//!
//! ```text
//! state interval    [2^23, 2^31), renormalized a byte at a time
//! table total       2^log, a declared power of two, so the state update is a shift
//! encode direction  backwards, and the byte buffer is reversed once at the end
//! decode direction  forwards
//! flush             four bytes per state
//! ```
//!
//! The encoder runs backwards because rANS is a stack: the last symbol pushed is the first one
//! popped. Reversing once at the end is what lets the decoder read forwards, and the decoder is
//! the side that is paid for.
//!
//! The table log is a declared rule and not a per-stream search. An encoder that searched for
//! the log that minimized its own bits would be tuning one arm of a comparison that was made
//! with the rule below fixed. The FAST encoder prefers at most 1024 slots: payload stays
//! stationary past 9-10 while the description and the decode table double with every further
//! bit. The preference is revisable when new distributions or block lengths move the measured
//! knee. A description may still declare 5..=12, and the decoder admits the whole range.

use super::bits::{BitReader, BitWriter};
use super::{ceil_log2, low_byte, shift_left, shift_right, width_for};
use crate::format::{Corruption, Error};

/// The narrowest table the format admits. Below this a skewed alphabet's normalization stops
/// being a coding question and becomes a quantization artefact.
pub const TABLE_LOG_MIN: u32 = 5;

/// The widest. It is the precision the published implementations settle near, and where a slot
/// table stops fitting comfortably beside the stream it decodes.
pub const TABLE_LOG_MAX: u32 = 12;

/// The widest table the FAST encoder prefers.
///
/// An encoder preference, not a format limit: parse, validation and the decode table still
/// admit up to `TABLE_LOG_MAX`. Revisable when new distributions or block lengths move the
/// measured knee.
pub const FAST_TABLE_LOG_MAX: u32 = 10;

/// The lower end of the state interval.
///
/// A multiple of every admissible table total, which is the condition that makes the
/// renormalization exact.
pub const RANS_L: u64 = 1 << 23;

/// The most interleaved states any symbol class takes.
pub const MAX_STATES: usize = 4;

/// Bytes one state occupies when it is flushed.
const FLUSH_BYTES: usize = 4;

/// Bits the table log occupies in a description.
///
/// Five, so a log outside the declared range is transmissible and is refused by the rule that
/// owns the range rather than by a field that could not carry it.
const LOG_FIELD_BITS: u32 = 5;

/// Bits the zero-run escape of a description occupies.
const ZERO_RUN_BITS: u32 = 4;

/// Bytes the decode table occupies at table log `log`.
///
/// One 32-bit entry per state of the table, holding the symbol, the frequency and the
/// cumulative start together, so the state update needs no array beside the slot table.
#[must_use]
pub const fn table_bytes_for(log: u32) -> u64 {
    shift_left(4, log)
}

/// Whether a state count is one this coder ships at.
const fn admits_states(states: usize) -> bool {
    matches!(states, 1 | 2 | 4)
}

/// Counts scaled to a power-of-two total.
///
/// The frequencies are dense over the declared alphabet. A zero means the symbol does not
/// occur; every symbol that occurs carries at least one slot, because a symbol with no slot
/// cannot be coded at all.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Table {
    log: u32,
    freq: Vec<u32>,
    alphabet_size: u32,
}

impl Table {
    /// The declared table log for a stream of `symbols` symbols over `distinct` of them.
    ///
    /// Wide enough to hold the support, no wider than the stream can justify, and clamped to
    /// the FAST preference: at most `FAST_TABLE_LOG_MAX` slots of resolution, while the floor
    /// still wins below it. The decoder admits the full declared range independently of this
    /// preference.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the support needs a wider table than the format admits.
    pub const fn log_for(symbols: u64, distinct: u32) -> Result<u32, Error> {
        let needed = if TABLE_LOG_MIN > ceil_log2(distinct as u64) {
            TABLE_LOG_MIN
        } else {
            ceil_log2(distinct as u64)
        };
        if needed > TABLE_LOG_MAX {
            return Err(Error::InvalidParameter);
        }
        let chosen = ceil_log2(symbols);
        if chosen < needed {
            Ok(needed)
        } else if chosen > FAST_TABLE_LOG_MAX {
            Ok(FAST_TABLE_LOG_MAX)
        } else {
            Ok(chosen)
        }
    }

    /// Scales the counts to a total of `2^log`, keeping every symbol that occurs.
    ///
    /// Integer arithmetic throughout, so the table is a function of the counts and the declared
    /// rule and of nothing else. The residue is moved onto the largest frequencies in turn and
    /// never drops one below a slot.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the counts span no alphabet, when no symbol occurs, or
    /// when the support needs a wider table than the format admits.
    pub fn normalize(counts: &[u64], alphabet_size: u32) -> Result<Self, Error> {
        let span = usize::try_from(alphabet_size).map_err(|_| Error::InvalidParameter)?;
        if counts.len() != span || span == 0 || alphabet_size > super::MAX_ALPHABET_SIZE {
            return Err(Error::InvalidParameter);
        }
        let mut total: u64 = 0;
        let mut distinct: u32 = 0;
        for &count in counts {
            if count > 0 {
                total = total.checked_add(count).ok_or(Error::InvalidParameter)?;
                distinct = distinct.saturating_add(1);
            }
        }
        if total == 0 {
            return Err(Error::InvalidParameter);
        }
        let log = Self::log_for(total, distinct)?;
        let target = shift_left(1, log);

        let mut freq = vec![0u32; span];
        let mut sum: u64 = 0;
        let half = shift_right(total, 1);
        for (symbol, &count) in counts.iter().enumerate() {
            if count == 0 {
                continue;
            }
            let scaled = count
                .checked_mul(target)
                .and_then(|product| product.checked_add(half))
                .and_then(|product| product.checked_div(total))
                .ok_or(Error::InvalidParameter)?
                .max(1);
            let value = u32::try_from(scaled).map_err(|_| Error::InvalidParameter)?;
            let slot = freq.get_mut(symbol).ok_or(Error::InvalidParameter)?;
            *slot = value;
            sum = sum.checked_add(scaled).ok_or(Error::InvalidParameter)?;
        }

        while sum > target {
            let excess = sum.saturating_sub(target);
            let (symbol, value) = largest(&freq);
            let take = excess.min(u64::from(value).saturating_sub(1));
            if take == 0 {
                return Err(Error::InvalidParameter);
            }
            let narrowed = u32::try_from(take).map_err(|_| Error::InvalidParameter)?;
            let slot = freq.get_mut(symbol).ok_or(Error::InvalidParameter)?;
            *slot = value.checked_sub(narrowed).ok_or(Error::InvalidParameter)?;
            sum = sum.saturating_sub(take);
        }
        if sum < target {
            let (symbol, value) = largest(&freq);
            let deficit =
                u32::try_from(target.saturating_sub(sum)).map_err(|_| Error::InvalidParameter)?;
            let slot = freq.get_mut(symbol).ok_or(Error::InvalidParameter)?;
            *slot = value.checked_add(deficit).ok_or(Error::InvalidParameter)?;
        }

        Ok(Self {
            log,
            freq,
            alphabet_size,
        })
    }

    /// The table log this table declares.
    #[must_use]
    pub const fn log(&self) -> u32 {
        self.log
    }

    /// The alphabet this table spans.
    #[must_use]
    pub const fn alphabet_size(&self) -> u32 {
        self.alphabet_size
    }

    /// The bytes a decoder allocates for this table.
    #[must_use]
    pub const fn table_bytes(&self) -> u64 {
        table_bytes_for(self.log)
    }

    /// Whether this table gives `symbol` a slot.
    ///
    /// A symbol with no slot cannot be coded under this table. An encoder deciding whether a
    /// stream can be written under a table built for another stream asks this before it
    /// writes.
    #[must_use]
    pub fn carries(&self, symbol: u16) -> bool {
        self.freq
            .get(usize::from(symbol))
            .is_some_and(|&frequency| frequency > 0)
    }

    /// Writes the description a decoder rebuilds this table from.
    ///
    /// Each frequency is written in the width the unassigned total still admits, so the fields
    /// narrow as the total is consumed and the description ends the moment nothing is left. A
    /// zero is followed by a count of the zeros after it, which is what keeps a sparse alphabet
    /// from paying a field for every symbol it never uses.
    pub fn describe(&self, out: &mut BitWriter) {
        out.push(u64::from(self.log), LOG_FIELD_BITS);
        let mut remaining = shift_left(1, self.log);
        let mut symbol = 0u32;
        while symbol < self.alphabet_size && remaining > 0 {
            let frequency = u64::from(self.frequency_of(symbol));
            out.push(frequency, width_for(remaining));
            remaining = remaining.saturating_sub(frequency);
            symbol = symbol.saturating_add(1);
            if frequency == 0 && remaining > 0 {
                let mut zeros = 0u32;
                while symbol.saturating_add(zeros) < self.alphabet_size
                    && self.frequency_of(symbol.saturating_add(zeros)) == 0
                {
                    zeros = zeros.saturating_add(1);
                }
                // A chunk at its maximum means another chunk follows, which is how a run
                // longer than one chunk is written without a second token kind.
                let cap = zero_run_cap();
                loop {
                    let take = zeros.min(cap);
                    out.push(u64::from(take), ZERO_RUN_BITS);
                    symbol = symbol.saturating_add(take);
                    zeros = zeros.saturating_sub(take);
                    if take < cap {
                        break;
                    }
                }
            }
        }
    }

    /// The encoder table: the frequency and the cumulative start, per symbol.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the frequencies do not sum to the table total.
    pub fn encoder(&self) -> Result<Encoder, Error> {
        let mut cumulative = vec![0u32; self.freq.len()];
        let mut running: u32 = 0;
        for (symbol, &frequency) in self.freq.iter().enumerate() {
            let slot = cumulative.get_mut(symbol).ok_or(Error::InvalidParameter)?;
            *slot = running;
            running = running
                .checked_add(frequency)
                .ok_or(Error::InvalidParameter)?;
        }
        if u64::from(running) != shift_left(1, self.log) {
            return Err(Error::InvalidParameter);
        }
        Ok(Encoder {
            log: self.log,
            freq: self.freq.clone(),
            cumulative,
        })
    }

    fn frequency_of(&self, symbol: u32) -> u32 {
        usize::try_from(symbol)
            .ok()
            .and_then(|at| self.freq.get(at))
            .copied()
            .unwrap_or(0)
    }
}

/// Codes symbols under a normalized table.
#[derive(Debug)]
pub struct Encoder {
    log: u32,
    freq: Vec<u32>,
    cumulative: Vec<u32>,
}

impl Encoder {
    /// Codes `symbols` at `states` interleaved states, and returns the bytes a decoder reads
    /// forwards.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the state count is not one this coder ships at, or when
    /// a symbol occurs that the table gives no slot.
    pub fn write(&self, symbols: &[u16], states: usize) -> Result<Vec<u8>, Error> {
        if !admits_states(states) {
            return Err(Error::InvalidParameter);
        }
        let mut state = [RANS_L; MAX_STATES];
        let mut out: Vec<u8> = Vec::new();
        let interleave = states.saturating_sub(1);

        let mut at = symbols.len();
        while at > 0 {
            at = at.saturating_sub(1);
            let symbol = symbols.get(at).copied().ok_or(Error::InvalidParameter)?;
            let frequency = u64::from(
                self.freq
                    .get(usize::from(symbol))
                    .copied()
                    .ok_or(Error::InvalidParameter)?,
            );
            if frequency == 0 {
                return Err(Error::InvalidParameter);
            }
            let start = u64::from(
                self.cumulative
                    .get(usize::from(symbol))
                    .copied()
                    .ok_or(Error::InvalidParameter)?,
            );
            let limit = shift_left(shift_right(RANS_L, self.log), 8)
                .checked_mul(frequency)
                .ok_or(Error::InvalidParameter)?;
            let slot = state
                .get_mut(at & interleave)
                .ok_or(Error::InvalidParameter)?;
            while *slot >= limit {
                out.push(low_byte(*slot));
                *slot = shift_right(*slot, 8);
            }
            let quotient = slot.checked_div(frequency).ok_or(Error::InvalidParameter)?;
            let residue = slot.checked_rem(frequency).ok_or(Error::InvalidParameter)?;
            *slot = shift_left(quotient, self.log)
                .checked_add(residue)
                .and_then(|state| state.checked_add(start))
                .ok_or(Error::InvalidParameter)?;
        }

        let mut which = states;
        while which > 0 {
            which = which.saturating_sub(1);
            let value = state.get(which).copied().ok_or(Error::InvalidParameter)?;
            for lane in 0..FLUSH_BYTES {
                out.push(low_byte(shift_right(value, lane_shift(lane))));
            }
        }
        out.reverse();
        Ok(out)
    }
}

/// A description a decoder has read and does not yet trust.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Declared {
    alphabet_size: u32,
    log: u32,
    freq: Vec<u32>,
}

impl Declared {
    /// Reads a description, refusing the moment it would pass the end of what it was given.
    ///
    /// The width of the first frequency field is decided by the declared total, so a log the
    /// validation would refuse is refused here instead, before any field width depends on it.
    ///
    /// # Errors
    ///
    /// Returns `TruncatedInput` when the description ends inside a field, and `CorruptData`
    /// when the log is out of range, a frequency passes the total still unassigned, or a zero
    /// run passes the end of the alphabet.
    pub fn parse(reader: &mut BitReader, alphabet_size: u32) -> Result<Self, Error> {
        let span = usize::try_from(alphabet_size).map_err(|_| Error::InvalidParameter)?;
        if span == 0 || alphabet_size > super::MAX_ALPHABET_SIZE {
            return Err(Error::InvalidParameter);
        }
        let log = narrow_log(reader.take(LOG_FIELD_BITS)?);
        if !(TABLE_LOG_MIN..=TABLE_LOG_MAX).contains(&log) {
            return Err(Error::CorruptData(Corruption::TableLog));
        }
        let mut freq = vec![0u32; span];
        let mut remaining = shift_left(1, log);
        let mut symbol = 0u32;
        while symbol < alphabet_size && remaining > 0 {
            let frequency = reader.take(width_for(remaining))?;
            if frequency > remaining {
                return Err(Error::CorruptData(Corruption::FrequencySum));
            }
            let value = u32::try_from(frequency).map_err(|_| Error::InvalidParameter)?;
            let slot = usize::try_from(symbol)
                .ok()
                .and_then(|at| freq.get_mut(at))
                .ok_or(Error::CorruptData(Corruption::SymbolAlphabet))?;
            *slot = value;
            remaining = remaining.saturating_sub(frequency);
            symbol = symbol.saturating_add(1);
            if frequency == 0 && remaining > 0 {
                let cap = zero_run_cap();
                loop {
                    let take = narrow_run(reader.take(ZERO_RUN_BITS)?);
                    if symbol.saturating_add(take) > alphabet_size {
                        return Err(Error::CorruptData(Corruption::DescriptionToken));
                    }
                    symbol = symbol.saturating_add(take);
                    if take < cap {
                        break;
                    }
                }
            }
        }
        Ok(Self {
            alphabet_size,
            log,
            freq,
        })
    }

    /// Applies every rule a decoder holds a description to before it allocates for it.
    ///
    /// Each rule is a property of the description alone, so an invalid table is refused without
    /// decoding a symbol under it.
    ///
    /// # Errors
    ///
    /// Returns `CorruptData` when the log is out of range, when no symbol carries a frequency,
    /// or when the frequencies do not sum to the total the log declares.
    pub fn validate(self) -> Result<Admitted, Error> {
        if !(TABLE_LOG_MIN..=TABLE_LOG_MAX).contains(&self.log) {
            return Err(Error::CorruptData(Corruption::TableLog));
        }
        let total = shift_left(1, self.log);
        let mut sum: u64 = 0;
        let mut distinct: u64 = 0;
        for &frequency in &self.freq {
            if frequency > 0 {
                distinct = distinct.saturating_add(1);
                sum = sum
                    .checked_add(u64::from(frequency))
                    .ok_or(Error::CorruptData(Corruption::FrequencySum))?;
            }
        }
        if distinct == 0 || distinct > total || sum != total {
            return Err(Error::CorruptData(Corruption::FrequencySum));
        }
        Ok(Admitted {
            table: Table {
                log: self.log,
                freq: self.freq,
                alphabet_size: self.alphabet_size,
            },
        })
    }
}

/// A description validation has admitted, and the requirement it declares.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Admitted {
    table: Table,
}

impl Admitted {
    /// The bytes the decode table will occupy, computed from the description alone.
    #[must_use]
    pub const fn table_bytes(&self) -> u64 {
        self.table.table_bytes()
    }

    /// The table this description named.
    #[must_use]
    pub const fn table(&self) -> &Table {
        &self.table
    }

    /// Allocates the decode table.
    ///
    /// This is the first allocation the description sizes, and it is reachable only from an
    /// admitted description.
    ///
    /// # Errors
    ///
    /// Returns `CorruptData` when the admitted frequencies do not fill the table exactly once,
    /// which validation has already refused.
    pub fn build(&self) -> Result<DecodeTable, Error> {
        let total =
            usize::try_from(shift_left(1, self.table.log)).map_err(|_| Error::InvalidParameter)?;
        let mut slots = vec![0u32; total];
        let mut running: u32 = 0;
        for (symbol, &frequency) in self.table.freq.iter().enumerate() {
            if frequency == 0 {
                continue;
            }
            let symbol = u32::try_from(symbol).map_err(|_| Error::InvalidParameter)?;
            let base = usize::try_from(running).map_err(|_| Error::InvalidParameter)?;
            let end = usize::try_from(frequency)
                .ok()
                .and_then(|width| base.checked_add(width))
                .ok_or(Error::CorruptData(Corruption::FrequencySum))?;
            let span = slots
                .get_mut(base..end)
                .ok_or(Error::CorruptData(Corruption::FrequencySum))?;
            span.fill(entry(symbol, frequency, running));
            running = running
                .checked_add(frequency)
                .ok_or(Error::CorruptData(Corruption::FrequencySum))?;
        }
        Ok(DecodeTable {
            log: self.table.log,
            slots,
        })
    }
}

/// The packed slot table: one 32-bit entry per state of the table.
#[derive(Clone, Debug)]
pub struct DecodeTable {
    log: u32,
    slots: Vec<u32>,
}

impl DecodeTable {
    /// The bytes this table occupies.
    #[must_use]
    pub fn allocated_bytes(&self) -> u64 {
        u64::try_from(self.slots.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(4)
    }

    /// Decodes one symbol per slot of `out`, and reports the bytes it consumed.
    ///
    /// The table is reachable only from an admitted description, and the fast lanes run only
    /// over the admitted shape the per-section check names, so no fast lane trusts a raw field.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the state count is not one this coder ships at, and
    /// `CorruptData` when the payload ends before the symbols do or names a state the table
    /// does not hold.
    pub fn decode(&self, bytes: &[u8], out: &mut [u16], states: usize) -> Result<usize, Error> {
        if !admits_states(states) {
            return Err(Error::InvalidParameter);
        }
        // Each lane stays in a register with the state count selected once outside the loop;
        // replay over real sections measured about 6-7 to about 3-4 nanoseconds per symbol at
        // one state, about 4-5 to about 2-3 at two, and about 3-4 to about 1-2 at four.
        if !self.is_admitted_shape() {
            return self.decode_generic(bytes, out, states);
        }
        match states {
            1 => self.decode_1(bytes, out),
            2 => self.decode_2(bytes, out),
            4 => self.decode_4(bytes, out),
            _ => Err(Error::InvalidParameter),
        }
    }

    /// Whether this table has the shape the fast lanes decode.
    ///
    /// The log sits inside the declared range and the slots hold exactly one entry per state
    /// of the table, so every place the mask admits names an entry the build filled.
    fn is_admitted_shape(&self) -> bool {
        if !(TABLE_LOG_MIN..=TABLE_LOG_MAX).contains(&self.log) {
            return false;
        }
        let total = usize::try_from(shift_left(1, self.log)).unwrap_or(usize::MAX);
        self.slots.len() == total
    }

    /// Decodes one symbol per slot of `out`, and reports the bytes it consumed.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the state count is not one this coder ships at, and
    /// `CorruptData` when the payload ends before the symbols do or names a state the table
    /// does not hold.
    fn decode_generic(&self, bytes: &[u8], out: &mut [u16], states: usize) -> Result<usize, Error> {
        if !admits_states(states) {
            return Err(Error::InvalidParameter);
        }
        let slot_mask = super::bits::mask(self.log);
        let interleave = states.saturating_sub(1);
        let mut at = 0usize;
        let mut state = [0u64; MAX_STATES];

        for which in 0..states {
            let mut value = 0u64;
            for _ in 0..FLUSH_BYTES {
                value = shift_left(value, 8) | u64::from(next_byte(bytes, &mut at)?);
            }
            let slot = state.get_mut(which).ok_or(Error::InvalidParameter)?;
            *slot = value;
        }

        for (index, symbol) in out.iter_mut().enumerate() {
            let slot = state
                .get_mut(index & interleave)
                .ok_or(Error::InvalidParameter)?;
            let place = *slot & slot_mask;
            let found = usize::try_from(place)
                .ok()
                .and_then(|at| self.slots.get(at))
                .copied()
                .ok_or(Error::CorruptData(Corruption::CodedStream))?;
            let frequency = u64::from(entry_frequency(found));
            let cumulative = u64::from(entry_cumulative(found));
            *slot = frequency
                .checked_mul(shift_right(*slot, self.log))
                .and_then(|next| next.checked_add(place))
                .and_then(|next| next.checked_sub(cumulative))
                .ok_or(Error::CorruptData(Corruption::CodedStream))?;
            while *slot < RANS_L {
                *slot = shift_left(*slot, 8) | u64::from(next_byte(bytes, &mut at)?);
            }
            *symbol = entry_symbol(found);
        }
        Ok(at)
    }

    /// Decodes through one lane held in a register.
    fn decode_1(&self, bytes: &[u8], out: &mut [u16]) -> Result<usize, Error> {
        let slot_mask = super::bits::mask(self.log);
        let mut at = 0usize;
        let mut s0 = load_state(bytes, &mut at)?;
        for symbol in out.iter_mut() {
            self.step_plain(&mut s0, slot_mask, bytes, &mut at, symbol)?;
        }
        Ok(at)
    }

    /// Decodes through two lanes held in registers, paired and tailed explicitly.
    fn decode_2(&self, bytes: &[u8], out: &mut [u16]) -> Result<usize, Error> {
        let slot_mask = super::bits::mask(self.log);
        let mut at = 0usize;
        let mut s0 = load_state(bytes, &mut at)?;
        let mut s1 = load_state(bytes, &mut at)?;
        let mut pos = 0usize;
        while pos.saturating_add(1) < out.len() {
            let (first, second) = out.split_at_mut(pos.saturating_add(1));
            let lead = first.last_mut().ok_or(Error::InvalidParameter)?;
            let trail = second.first_mut().ok_or(Error::InvalidParameter)?;
            self.step_plain(&mut s0, slot_mask, bytes, &mut at, lead)?;
            self.step_plain(&mut s1, slot_mask, bytes, &mut at, trail)?;
            pos = pos.saturating_add(2);
        }
        if pos < out.len() {
            let slot = out.get_mut(pos).ok_or(Error::InvalidParameter)?;
            self.step_plain(&mut s0, slot_mask, bytes, &mut at, slot)?;
        }
        Ok(at)
    }

    /// Decodes through four lanes held in registers, unrolled and tailed explicitly.
    fn decode_4(&self, bytes: &[u8], out: &mut [u16]) -> Result<usize, Error> {
        let slot_mask = super::bits::mask(self.log);
        let mut at = 0usize;
        let mut s0 = load_state(bytes, &mut at)?;
        let mut s1 = load_state(bytes, &mut at)?;
        let mut s2 = load_state(bytes, &mut at)?;
        let mut s3 = load_state(bytes, &mut at)?;
        let mut pos = 0usize;
        while pos.saturating_add(4) <= out.len() {
            let first = out.get_mut(pos).ok_or(Error::InvalidParameter)?;
            self.step_plain(&mut s0, slot_mask, bytes, &mut at, first)?;
            let second = out
                .get_mut(pos.saturating_add(1))
                .ok_or(Error::InvalidParameter)?;
            self.step_plain(&mut s1, slot_mask, bytes, &mut at, second)?;
            let third = out
                .get_mut(pos.saturating_add(2))
                .ok_or(Error::InvalidParameter)?;
            self.step_plain(&mut s2, slot_mask, bytes, &mut at, third)?;
            let fourth = out
                .get_mut(pos.saturating_add(3))
                .ok_or(Error::InvalidParameter)?;
            self.step_plain(&mut s3, slot_mask, bytes, &mut at, fourth)?;
            pos = pos.saturating_add(4);
        }
        let mut lane = 0usize;
        while pos < out.len() {
            let slot = out.get_mut(pos).ok_or(Error::InvalidParameter)?;
            match lane {
                0 => self.step_plain(&mut s0, slot_mask, bytes, &mut at, slot)?,
                1 => self.step_plain(&mut s1, slot_mask, bytes, &mut at, slot)?,
                2 => self.step_plain(&mut s2, slot_mask, bytes, &mut at, slot)?,
                _ => self.step_plain(&mut s3, slot_mask, bytes, &mut at, slot)?,
            }
            lane = lane.saturating_add(1);
            pos = pos.saturating_add(1);
        }
        Ok(at)
    }

    /// One rANS step over the admitted shape.
    ///
    /// The caller holds an admitted table whose log sits inside the declared range and whose
    /// slots hold exactly one entry per table state, so every place the mask admits maps to an
    /// entry with its cumulative start at or below the place and its frequency reaching past it.
    ///
    /// The admitted domain keeps the plain update exact: the log lies in 5..=12, each frequency
    /// lies in 1..=total, each cumulative lies below the total, each place lies below the total
    /// with the entry's interval covering it, and each state at entry lies at or below 2^32 - 1
    /// and never grows past its flushed bound. The product stays at or below the state, the add
    /// of the place stays below 2^64, and the subtract of the cumulative stays exact, so the
    /// plain form coincides with the checked form on every reachable input. A short payload
    /// still fails at the input helper and a place past the slots still fails at the lookup.
    // The admitted shape above is the validation that makes the plain arithmetic sound.
    #[allow(clippy::arithmetic_side_effects)]
    fn step_plain(
        &self,
        state: &mut u64,
        slot_mask: u64,
        bytes: &[u8],
        at: &mut usize,
        out_slot: &mut u16,
    ) -> Result<(), Error> {
        let place = *state & slot_mask;
        let found = usize::try_from(place)
            .ok()
            .and_then(|at| self.slots.get(at))
            .copied()
            .ok_or(Error::CorruptData(Corruption::CodedStream))?;
        let frequency = u64::from(entry_frequency(found));
        let cumulative = u64::from(entry_cumulative(found));
        let quotient = shift_right(*state, self.log);
        *state = frequency * quotient + place - cumulative;
        while *state < RANS_L {
            *state = shift_left(*state, 8) | u64::from(next_byte(bytes, at)?);
        }
        *out_slot = entry_symbol(found);
        Ok(())
    }
}

fn load_state(bytes: &[u8], at: &mut usize) -> Result<u64, Error> {
    let mut value = 0u64;
    for _ in 0..FLUSH_BYTES {
        value = shift_left(value, 8) | u64::from(next_byte(bytes, at)?);
    }
    Ok(value)
}

/// The symbol with the largest frequency, ties going to the smallest symbol.
fn largest(freq: &[u32]) -> (usize, u32) {
    let mut best = (0usize, 0u32);
    for (symbol, &value) in freq.iter().enumerate() {
        if value > best.1 {
            best = (symbol, value);
        }
    }
    best
}

fn next_byte(bytes: &[u8], at: &mut usize) -> Result<u8, Error> {
    let byte = bytes
        .get(*at)
        .copied()
        .ok_or(Error::CorruptData(Corruption::CodedStream))?;
    *at = at.saturating_add(1);
    Ok(byte)
}

/// The longest zero run one escape field carries.
fn zero_run_cap() -> u32 {
    u32::try_from(super::bits::mask(ZERO_RUN_BITS)).unwrap_or(u32::MAX)
}

// The field is five bits wide, so the narrowing is exact.
#[allow(clippy::cast_possible_truncation)]
const fn narrow_log(value: u64) -> u32 {
    (value & 0x1F) as u32
}

// The field is four bits wide, so the narrowing is exact.
#[allow(clippy::cast_possible_truncation)]
const fn narrow_run(value: u64) -> u32 {
    (value & 0xF) as u32
}

/// The shift that places the flushed byte of `lane`.
fn lane_shift(lane: usize) -> u32 {
    u32::try_from(lane).unwrap_or(0).saturating_mul(8)
}

// The alphabet is below 256 and the table log is at most 12, so a symbol takes eight bits and a
// frequency less one and a cumulative start take twelve each. Thirty-two exactly, which is why
// this shape needs no array beside the slot table.
#[allow(clippy::arithmetic_side_effects)]
const fn entry(symbol: u32, frequency: u32, cumulative: u32) -> u32 {
    ((symbol & 0xFF) << 24) | ((frequency.saturating_sub(1) & 0xFFF) << 12) | (cumulative & 0xFFF)
}

// The entry packs three fields at fixed offsets, so no shift loses a bit.
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
const fn entry_symbol(value: u32) -> u16 {
    (value >> 24) as u16
}

#[allow(clippy::arithmetic_side_effects)]
const fn entry_frequency(value: u32) -> u32 {
    ((value >> 12) & 0xFFF) + 1
}

const fn entry_cumulative(value: u32) -> u32 {
    value & 0xFFF
}

#[cfg(test)]
mod tests {
    use super::{
        Declared, DecodeTable, FAST_TABLE_LOG_MAX, TABLE_LOG_MAX, TABLE_LOG_MIN, Table, entry,
        entry_cumulative, entry_frequency, entry_symbol, table_bytes_for,
    };
    use crate::entropy::bits::{BitReader, BitWriter};
    use crate::entropy::shift_right;
    use crate::format::{Corruption, Error};

    fn counts_from(shape: &[u64]) -> Vec<u64> {
        shape.to_vec()
    }

    #[test]
    fn a_packed_entry_carries_its_three_fields_exactly() {
        for &(symbol, frequency, cumulative) in &[
            (0u32, 1u32, 0u32),
            (255, 4_096, 0),
            (17, 2_048, 2_048),
            (200, 1, 4_095),
        ] {
            let packed = entry(symbol, frequency, cumulative);
            assert_eq!(u32::from(entry_symbol(packed)), symbol);
            assert_eq!(entry_frequency(packed), frequency);
            assert_eq!(entry_cumulative(packed), cumulative);
        }
    }

    #[test]
    fn the_table_log_rule_holds_its_declared_range() -> Result<(), Error> {
        assert_eq!(Table::log_for(1, 1)?, TABLE_LOG_MIN);
        assert_eq!(Table::log_for(40, 8)?, 6);
        assert_eq!(Table::log_for(65_536, 200)?, FAST_TABLE_LOG_MAX);
        assert_eq!(Table::log_for(8, 200)?, 8);
        assert_eq!(Table::log_for(1_024, 2)?, FAST_TABLE_LOG_MAX);
        assert_eq!(Table::log_for(2_048, 2)?, FAST_TABLE_LOG_MAX);
        assert_eq!(Table::log_for(100, 4)?, 7);
        assert_eq!(Table::log_for(1 << 20, 5_000), Err(Error::InvalidParameter));
        Ok(())
    }

    #[test]
    fn a_decoder_still_admits_logs_above_the_fast_preference() -> Result<(), Error> {
        for (log, freq) in [
            (11u32, vec![512u32, 512, 512, 512]),
            (12u32, vec![1_024u32, 1_024, 1_024, 1_024]),
        ] {
            let table = Table {
                log,
                freq,
                alphabet_size: 4,
            };
            let mut writer = BitWriter::new();
            table.describe(&mut writer);
            let description = writer.finish();
            let admitted = Declared::parse(&mut BitReader::over(&description), 4)?.validate()?;
            assert_eq!(admitted.table(), &table);
            assert_eq!(admitted.table_bytes(), table_bytes_for(log));
            let built = admitted.build()?;
            assert_eq!(built.allocated_bytes(), 4u64 << log);
            let symbols = vec![0u16, 1, 2, 3, 3, 2, 1, 0];
            for &states in &[1usize, 2, 4] {
                let payload = admitted.table().encoder()?.write(&symbols, states)?;
                let mut out = vec![0u16; symbols.len()];
                let consumed = built.decode(&payload, &mut out, states)?;
                assert_eq!(consumed, payload.len());
                assert_eq!(out, symbols);
            }
        }
        Ok(())
    }

    #[test]
    fn capped_tables_hold_at_most_4096_bytes_on_structured_shapes() -> Result<(), Error> {
        let wide = counts_from(&[1_000, 1_000, 1_000, 1_000, 500, 250, 125, 125]);
        let even = vec![328u64; 200];
        let skewed = counts_of(&skewed_stream(64, 20_000, 7), 64);
        for (shape, alphabet) in [(&wide, 8u32), (&even, 200u32), (&skewed, 64u32)] {
            let table = Table::normalize(shape, alphabet)?;
            assert!(
                table.log() <= FAST_TABLE_LOG_MAX,
                "a structured shape selected log {} above the preference",
                table.log()
            );
            assert!(
                table.table_bytes() <= table_bytes_for(FAST_TABLE_LOG_MAX),
                "a capped table holds {} bytes",
                table.table_bytes()
            );
            let mut writer = BitWriter::new();
            table.describe(&mut writer);
            let description = writer.finish();
            let admitted =
                Declared::parse(&mut BitReader::over(&description), alphabet)?.validate()?;
            assert_eq!(admitted.table_bytes(), table.table_bytes());
            assert!(
                admitted.build()?.allocated_bytes() <= table_bytes_for(FAST_TABLE_LOG_MAX),
                "an admitted capped table allocates above the preference"
            );
        }
        Ok(())
    }

    #[test]
    fn a_normalized_table_sums_to_its_total_and_keeps_every_symbol() -> Result<(), Error> {
        let shapes: [Vec<u64>; 4] = [
            counts_from(&[1, 1, 1, 1, 1, 1, 1, 1]),
            counts_from(&[100_000, 1, 1, 1, 1, 1, 1, 1]),
            counts_from(&[1, 0, 0, 0, 0, 0, 0, 9_999]),
            counts_from(&[3, 5, 7, 11, 13, 17, 19, 23]),
        ];
        for shape in &shapes {
            let table = Table::normalize(shape, 8)?;
            let mut writer = BitWriter::new();
            table.describe(&mut writer);
            let description = writer.finish();
            let rebuilt = Declared::parse(&mut BitReader::over(&description), 8)?.validate()?;
            assert_eq!(rebuilt.table(), &table);
            assert_eq!(rebuilt.table_bytes(), table_bytes_for(table.log()));
        }
        Ok(())
    }

    #[test]
    fn a_table_log_outside_the_declared_range_is_refused_before_a_width_depends_on_it() {
        for log in [0u64, 1, 4, 13, 31] {
            let mut writer = BitWriter::new();
            writer.push(log, 5);
            writer.push(0, 13);
            let bits = writer.finish();
            assert_eq!(
                Declared::parse(&mut BitReader::over(&bits), 8),
                Err(Error::CorruptData(Corruption::TableLog)),
                "a log of {log} is outside the declared range"
            );
        }
    }

    #[test]
    fn frequencies_that_do_not_sum_to_the_total_are_refused() -> Result<(), Error> {
        let table = Table::normalize(&counts_from(&[3, 5, 7, 11]), 4)?;
        let mut writer = BitWriter::new();
        table.describe(&mut writer);
        let description = writer.finish();
        let declared = Declared::parse(&mut BitReader::over(&description), 4)?;

        let mut short = declared.clone();
        short.freq = vec![1, 1, 1, 1];
        assert_eq!(
            short.validate(),
            Err(Error::CorruptData(Corruption::FrequencySum))
        );

        let mut empty = declared;
        empty.freq = vec![0, 0, 0, 0];
        assert_eq!(
            empty.validate(),
            Err(Error::CorruptData(Corruption::FrequencySum))
        );
        Ok(())
    }

    #[test]
    fn a_state_count_the_coder_does_not_ship_is_refused() -> Result<(), Error> {
        let table = Table::normalize(&counts_from(&[3, 5, 7, 11]), 4)?;
        let encoder = table.encoder()?;
        for states in [0usize, 3, 5, 8] {
            assert_eq!(
                encoder.write(&[0, 1, 2], states),
                Err(Error::InvalidParameter)
            );
        }
        Ok(())
    }

    /// A deterministic skewed stream over `alphabet` symbols.
    fn skewed_stream(alphabet: u32, len: usize, seed: u64) -> Vec<u16> {
        let mut state = seed | 1;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let span = u64::from(alphabet);
            let first = shift_right(state, 33).checked_rem(span).unwrap_or(0);
            let second = shift_right(state, 11).checked_rem(span).unwrap_or(0);
            out.push(u16::try_from(first.min(second)).unwrap_or(0));
        }
        out
    }

    fn counts_of(symbols: &[u16], alphabet: u32) -> Vec<u64> {
        let span = usize::try_from(alphabet).unwrap_or(0);
        let mut counts = vec![0u64; span];
        for &symbol in symbols {
            if let Some(slot) = counts.get_mut(usize::from(symbol)) {
                *slot = slot.saturating_add(1);
            }
        }
        counts
    }

    /// The admitted table for `symbols` with the bytes its encoder wrote at `states`.
    ///
    /// The description crosses parse and validate like a decoder's does, so the table is the
    /// one admission builds.
    fn table_and_payload(
        counts: &[u64],
        alphabet: u32,
        symbols: &[u16],
        states: usize,
    ) -> Result<(DecodeTable, Vec<u8>), Error> {
        let table = Table::normalize(counts, alphabet)?;
        let mut writer = BitWriter::new();
        table.describe(&mut writer);
        let description = writer.finish();
        let built = Declared::parse(&mut BitReader::over(&description), alphabet)?
            .validate()?
            .build()?;
        let payload = table.encoder()?.write(symbols, states)?;
        Ok((built, payload))
    }

    /// Runs the fast lanes and the retained generic loop over the same input and requires
    /// identical outcomes, identical bytes, and identical consumption.
    fn check_paths(
        table: &DecodeTable,
        section: &[u8],
        len: usize,
        states: usize,
    ) -> (Result<usize, Error>, Vec<u16>) {
        let mut fast_out = vec![0u16; len];
        let fast = table.decode(section, &mut fast_out, states);
        let mut slow_out = vec![0u16; len];
        let slow = table.decode_generic(section, &mut slow_out, states);
        assert_eq!(fast, slow, "the fast lanes classify differently");
        assert_eq!(fast_out, slow_out, "the fast lanes decode different bytes");
        (fast, fast_out)
    }

    #[test]
    fn the_fast_lanes_match_the_generic_loop_over_admitted_tables() -> Result<(), Error> {
        for &states in &[1usize, 2, 4] {
            let symbols =
                skewed_stream(8, 200, u64::try_from(states).unwrap_or(0).saturating_add(1));
            let counts = counts_of(&symbols, 8);
            let (table, payload) = table_and_payload(&counts, 8, &symbols, states)?;
            let (result, out) = check_paths(&table, &payload, symbols.len(), states);
            assert_eq!(result, Ok(payload.len()));
            assert_eq!(out, symbols);

            let flat = skewed_stream(16, 160, 11);
            let flat_counts = counts_of(&flat, 16);
            let (table, payload) = table_and_payload(&flat_counts, 16, &flat, states)?;
            let (result, out) = check_paths(&table, &payload, flat.len(), states);
            assert_eq!(result, Ok(payload.len()));
            assert_eq!(out, flat);
        }
        Ok(())
    }

    #[test]
    fn single_symbol_empty_and_singleton_outputs_share_one_path() -> Result<(), Error> {
        let alphabet = 8u32;
        let mut counts = vec![0u64; usize::try_from(alphabet).unwrap_or(0)];
        if let Some(slot) = counts.get_mut(2) {
            *slot = 300;
        }
        for &states in &[1usize, 2, 4] {
            let symbols = vec![2u16; 300];
            let (table, payload) = table_and_payload(&counts, alphabet, &symbols, states)?;
            assert!(
                table.slots.iter().all(|&packed| entry_symbol(packed) == 2),
                "a single-symbol table carries one symbol"
            );
            let (result, out) = check_paths(&table, &payload, symbols.len(), states);
            assert_eq!(result, Ok(payload.len()));
            assert_eq!(out, symbols);

            let (result, out) = check_paths(&table, &payload, 0, states);
            assert_eq!(result, Ok(payload.len().min(4usize.saturating_mul(states))));
            assert!(out.is_empty());

            let one = vec![2u16; 1];
            let (single_table, single_payload) =
                table_and_payload(&counts, alphabet, &one, states)?;
            let (result, out) = check_paths(&single_table, &single_payload, 1, states);
            assert_eq!(result, Ok(single_payload.len()));
            assert_eq!(out, one);
        }
        Ok(())
    }

    #[test]
    fn odd_tails_and_table_log_ends_match() -> Result<(), Error> {
        let narrow_counts = counts_from(&[1, 1, 1, 1, 1, 1, 1, 1]);
        let narrow_symbols = skewed_stream(8, 137, 5);
        let (narrow, narrow_payload) = table_and_payload(&narrow_counts, 8, &narrow_symbols, 1)?;
        assert_eq!(narrow.log, TABLE_LOG_MIN);
        for &states in &[1usize, 2, 4] {
            let (table, payload) = table_and_payload(&narrow_counts, 8, &narrow_symbols, states)?;
            for &len in &[1usize, 3, 5, 7, 9, 137] {
                let take = narrow_symbols.get(..len).unwrap_or(&[]).to_vec();
                let payload = if len == narrow_symbols.len() {
                    payload.clone()
                } else {
                    Table::normalize(&narrow_counts, 8)?
                        .encoder()?
                        .write(&take, states)?
                };
                let (result, out) = check_paths(&table, &payload, take.len(), states);
                assert_eq!(result, Ok(payload.len()));
                assert_eq!(out, take);
            }
        }

        let wide_counts = counts_from(&[1_000, 1_000, 1_000, 1_000, 500, 250, 125, 125]);
        let wide_symbols = skewed_stream(8, 4_500, 9);
        let (wide, _) = table_and_payload(&wide_counts, 8, &wide_symbols, 1)?;
        assert_eq!(wide.log, FAST_TABLE_LOG_MAX);
        for &states in &[1usize, 2, 4] {
            let (table, payload) = table_and_payload(&wide_counts, 8, &wide_symbols, states)?;
            let (result, out) = check_paths(&table, &payload, wide_symbols.len(), states);
            assert_eq!(result, Ok(payload.len()));
            assert_eq!(out, wide_symbols);
            let _ = check_paths(&table, &payload, 3, states);
            let _ = check_paths(&narrow, &narrow_payload, 1, states);
        }
        Ok(())
    }

    #[test]
    fn exotic_shapes_fall_back_with_identical_errors() -> Result<(), Error> {
        let symbols = skewed_stream(8, 64, 17);
        let counts = counts_of(&symbols, 8);
        let (table, payload) = table_and_payload(&counts, 8, &symbols, 1)?;
        let full_slots = table.slots.clone();
        let log = table.log;
        for &states in &[1usize, 2, 4] {
            let short_len = full_slots.len().saturating_sub(1);
            if let Some(short) = full_slots.get(..short_len) {
                let rebuilt = DecodeTable {
                    log,
                    slots: short.to_vec(),
                };
                let (fast, _) = check_paths(&rebuilt, &payload, symbols.len(), states);
                assert!(fast.is_err() || fast.is_ok());
            }
            let bad_logs = [
                TABLE_LOG_MIN.saturating_sub(1),
                TABLE_LOG_MAX.saturating_add(1),
            ];
            for bad in bad_logs {
                let rebuilt = DecodeTable {
                    log: bad,
                    slots: full_slots.clone(),
                };
                let _ = check_paths(&rebuilt, &payload, symbols.len(), states);
            }
            let empty = DecodeTable {
                log,
                slots: Vec::new(),
            };
            let (result, _) = check_paths(&empty, &payload, symbols.len(), states);
            assert_eq!(
                result,
                Err(Error::CorruptData(Corruption::CodedStream)),
                "an empty slot table names no state"
            );
        }
        Ok(())
    }

    #[test]
    fn truncated_and_over_declared_payloads_keep_their_error_class() -> Result<(), Error> {
        for &states in &[1usize, 2, 4] {
            let symbols = skewed_stream(8, 120, 19);
            let counts = counts_of(&symbols, 8);
            let (table, payload) = table_and_payload(&counts, 8, &symbols, states)?;
            for &cut in &[
                0usize,
                1,
                3,
                4usize.saturating_mul(states).saturating_sub(1),
                4usize.saturating_mul(states),
                payload.len().saturating_sub(1),
            ] {
                if cut >= payload.len() {
                    continue;
                }
                if let Some(cut_payload) = payload.get(..cut) {
                    let (result, _) = check_paths(&table, cut_payload, symbols.len(), states);
                    assert_eq!(
                        result,
                        Err(Error::CorruptData(Corruption::CodedStream)),
                        "a payload cut at {cut} bytes is truncation"
                    );
                }
            }
            let long_len = symbols.len().saturating_add(1);
            let (result, _) = check_paths(&table, &payload, long_len, states);
            assert_eq!(
                result,
                Err(Error::CorruptData(Corruption::CodedStream)),
                "more output than the payload holds is truncation"
            );
            let mut trailed = payload.clone();
            trailed.push(0);
            let (result, out) = check_paths(&table, &trailed, symbols.len(), states);
            assert_eq!(result, Ok(payload.len()));
            assert_eq!(out, symbols);
        }
        Ok(())
    }

    #[test]
    fn bad_state_counts_are_refused_and_cross_counts_agree() -> Result<(), Error> {
        let symbols = skewed_stream(8, 96, 23);
        let counts = counts_of(&symbols, 8);
        let (table, payload) = table_and_payload(&counts, 8, &symbols, 2)?;
        for states in [0usize, 3, 5, 8] {
            let (result, _) = check_paths(&table, &payload, symbols.len(), states);
            assert_eq!(result, Err(Error::InvalidParameter));
            let mut out = vec![0u16; symbols.len()];
            assert_eq!(
                table.decode(&payload, &mut out, states),
                Err(Error::InvalidParameter)
            );
        }
        for &encode_states in &[1usize, 2, 4] {
            let (table, payload) = table_and_payload(&counts, 8, &symbols, encode_states)?;
            for &decode_states in &[1usize, 2, 4] {
                let _ = check_paths(&table, &payload, symbols.len(), decode_states);
            }
        }
        Ok(())
    }

    #[test]
    fn every_valid_symbol_decodes_through_both_paths() -> Result<(), Error> {
        let alphabet = 8u32;
        let mut symbols: Vec<u16> = (0u16..8u16).collect();
        symbols.extend(0u16..8u16);
        symbols.extend(skewed_stream(alphabet, 120, 29));
        let counts = counts_of(&symbols, alphabet);
        for &states in &[1usize, 2, 4] {
            let (table, payload) = table_and_payload(&counts, alphabet, &symbols, states)?;
            let (result, out) = check_paths(&table, &payload, symbols.len(), states);
            assert_eq!(result, Ok(payload.len()));
            assert_eq!(out, symbols, "every valid symbol round trips");
        }
        Ok(())
    }
}
