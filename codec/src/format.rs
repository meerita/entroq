//! Owns the format contract: the frame, region, and block structure, the declared limits a
//! decoder inspects, and the validated types a decoded field becomes.
//!
//! This module does not own encoder policy, decoder execution, or entropy table layout.
//!
//! Parsing allocates nothing. A decoded header is a fixed-size value and a decoded payload
//! stays a borrow of the caller's input, so a length read from a stream can never size an
//! allocation before the boundary that owns the field has validated it.
//!
//! Every multi-byte field is little-endian, and every one is read and written by explicit
//! byte position and shift. No native integer reaches the stream through its in-memory
//! representation, so the same bytes decode identically on every architecture.
//!
//! A frame is a header, then a sequence of tagged records, then nothing more:
//!
//! ```text
//! frame header          the parameters and the declared limits of the whole object
//! record tag 0x01       a region header, then the blocks the region holds
//! record tag 0x00       the terminator, which ends the frame
//! ```
//!
//! Reserved capacity is split by meaning, and the split is fixed at version 1. A set bit in
//! reject-reserved space is corruption. A set bit in ignore-reserved space that this version
//! does not define is a well-formed feature this build does not implement, and the two carry
//! different errors so a caller can tell them apart.

use core::fmt;

/// The first four bytes of every frame, stored little-endian as `E7 51 B3 2A`.
///
/// The value avoids a trivial pattern, carries byte values outside the ASCII range, and is
/// not a valid UTF-8 sequence, so a frame is identifiable by inspection.
pub const MAGIC: u32 = 0x2AB3_51E7;

/// The only major version this build decodes.
pub const MAJOR_VERSION: u8 = 1;

/// The bytes every frame header carries, before any optional field.
pub const FRAME_HEADER_FIXED_BYTES: usize = 16;

/// The bytes the widest frame header occupies.
pub const FRAME_HEADER_MAX_BYTES: usize = 36;

/// The bytes a region header carries, before its integrity field.
pub const REGION_HEADER_FIXED_BYTES: usize = 16;

/// The bytes the widest region header occupies, without its record tag.
pub const REGION_HEADER_MAX_BYTES: usize = 24;

/// The bytes the widest record occupies, tag included.
pub const RECORD_MAX_BYTES: usize = 25;

/// The width of the integrity field a region header carries when the frame declares one.
///
/// The field is reserved at this width and nothing computes it. The algorithm and the bytes
/// it covers are not decided.
pub const INTEGRITY_FIELD_BYTES: usize = 8;

/// The bytes every block header occupies.
pub const BLOCK_HEADER_BYTES: usize = 4;

/// The bytes a record tag occupies.
pub const RECORD_TAG_BYTES: usize = 1;

/// The bytes the terminator occupies.
pub const TERMINATOR_BYTES: usize = RECORD_TAG_BYTES;

/// The largest size a block header can declare.
///
/// The block size field is 29 bits wide. The width is provisional.
pub const MAX_BLOCK_BYTES: u32 = 0x1FFF_FFFF;

const CONTENT_LENGTH_BYTES: usize = 8;
const DICTIONARY_ID_BYTES: usize = 4;
const INDEX_LOCATION_BYTES: usize = 8;

const FLAG_REGION_INDEPENDENT: u8 = 0b0000_0001;
const FLAG_CONTENT_LENGTH: u8 = 0b0000_0010;
const FLAG_DICTIONARY_ID: u8 = 0b0000_0100;
const FLAG_INDEX: u8 = 0b0000_1000;

/// The flag bits that version 1 does not define, and that a decoder rejects when set.
const FLAG_REJECT_RESERVED: u8 = 0b1111_0000;

/// The ignore-reserved bits that version 1 defines, of which there are none.
const IGNORE_RESERVED_DEFINED: u32 = 0;

const TAG_TERMINATOR: u8 = 0x00;
const TAG_REGION: u8 = 0x01;

const BLOCK_LAST_BIT: u32 = 0b0000_0001;
const BLOCK_TYPE_MASK: u32 = 0b0000_0011;
const BLOCK_TYPE_SHIFT: u32 = 1;
const BLOCK_SIZE_SHIFT: u32 = 3;

const BLOCK_CODE_RAW: u32 = 0;
const BLOCK_CODE_RLE: u32 = 1;
const BLOCK_CODE_COMPRESSED: u32 = 2;

/// The class of a failure the format contract defines.
///
/// The set is matchable and stable. It names the contract that failed and carries only what
/// a caller needs to act on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// The bytes do not start an Entroq frame.
    InvalidFormat,
    /// The frame declares a major version this build does not decode.
    UnsupportedVersion {
        /// The version the frame declared.
        major: u8,
    },
    /// The stream is well formed and asks for something this build does not implement.
    UnsupportedFeature(Feature),
    /// The stream violates the format contract.
    CorruptData(Corruption),
    /// The structure needs more bytes than the input holds.
    ///
    /// A streaming caller treats this as a request for more input, not as a failure.
    TruncatedInput {
        /// The bytes the structure needs, counted from the start of the input it was read
        /// from.
        needed: usize,
    },
    /// A declared requirement is above what local policy allows.
    LimitExceeded {
        /// The requirement the stream declared.
        declared: u64,
        /// The largest requirement policy allows.
        allowed: u64,
    },
    /// The caller-supplied output buffer is too small for the declared output.
    OutputTooSmall {
        /// The bytes the output needs.
        needed: usize,
    },
    /// The caller asked for something the format cannot represent.
    InvalidParameter,
}

/// A well-formed feature that this build does not implement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Feature {
    /// A frame flag in ignore-reserved space that version 1 does not define.
    ReservedFrameFlag,
    /// A compressed block, which the type space reserves and no version yet defines.
    CompressedBlock,
}

/// The format contract a stream violated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Corruption {
    /// A frame flag in reject-reserved space is set.
    FrameFlag,
    /// A bit of the frame's reject-reserved word is set.
    FrameReserved,
    /// The declared resource class is outside the enumeration.
    ResourceClass,
    /// The declared integrity mode is outside the enumeration.
    IntegrityMode,
    /// The record tag is neither a region nor the terminator.
    RecordTag,
    /// The block type is reserved and no version can define it.
    BlockType,
    /// The block declares a size the format does not permit.
    BlockSize,
    /// The region declares a logical size the format does not permit.
    RegionLogicalSize,
    /// The region declares a physical size too small to hold one block.
    RegionPhysicalSize,
    /// The bytes a region holds do not match the sizes its header declared.
    RegionMismatch,
    /// The frame decoded to a length other than the one it declared.
    ContentLength,
    /// A header needs more bytes than the widest form the format defines.
    HeaderWidth,
}

impl fmt::Display for Error {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::InvalidFormat => out.write_str("the bytes do not start an Entroq frame"),
            Self::UnsupportedVersion { major } => {
                write!(out, "major version {major} is not implemented")
            }
            Self::UnsupportedFeature(feature) => write!(out, "unsupported feature: {feature}"),
            Self::CorruptData(corruption) => write!(out, "corrupt data: {corruption}"),
            Self::TruncatedInput { needed } => {
                write!(out, "the structure needs {needed} bytes")
            }
            Self::LimitExceeded { declared, allowed } => {
                write!(out, "declared {declared} bytes, policy allows {allowed}")
            }
            Self::OutputTooSmall { needed } => {
                write!(out, "the output needs {needed} bytes")
            }
            Self::InvalidParameter => out.write_str("the format cannot represent the request"),
        }
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(match *self {
            Self::ReservedFrameFlag => "a frame flag this version does not define",
            Self::CompressedBlock => "a compressed block",
        })
    }
}

impl fmt::Display for Corruption {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(match *self {
            Self::FrameFlag => "a reserved frame flag is set",
            Self::FrameReserved => "a reserved frame bit is set",
            Self::ResourceClass => "the resource class is undefined",
            Self::IntegrityMode => "the integrity mode is undefined",
            Self::RecordTag => "the record tag is undefined",
            Self::BlockType => "the block type is reserved",
            Self::BlockSize => "the block size is out of range",
            Self::RegionLogicalSize => "the region logical size is out of range",
            Self::RegionPhysicalSize => "the region physical size is out of range",
            Self::RegionMismatch => "the region contents contradict its header",
            Self::ContentLength => "the frame length contradicts its header",
            Self::HeaderWidth => "a header is wider than the format defines",
        })
    }
}

impl std::error::Error for Error {}

/// The memory class a decoder sizes itself from.
///
/// The class is enumerated rather than derived from a window length, so every accepted value
/// names a bound and no arithmetic sits between the field and the memory it implies. The
/// members are provisional until they are measured.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceClass {
    /// 64 KiB of decoder history.
    Minimal,
    /// 1 MiB of decoder history.
    Small,
    /// 8 MiB of decoder history.
    Medium,
    /// 64 MiB of decoder history.
    Large,
    /// 256 MiB of decoder history.
    Huge,
}

impl ResourceClass {
    /// The history a decoder reserves for a frame of this class.
    #[must_use]
    pub const fn history_bytes(self) -> u64 {
        match self {
            Self::Minimal => 65_536,
            Self::Small => 1_048_576,
            Self::Medium => 8_388_608,
            Self::Large => 67_108_864,
            Self::Huge => 268_435_456,
        }
    }

    const fn code(self) -> u8 {
        match self {
            Self::Minimal => 0,
            Self::Small => 1,
            Self::Medium => 2,
            Self::Large => 3,
            Self::Huge => 4,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Minimal),
            1 => Some(Self::Small),
            2 => Some(Self::Medium),
            3 => Some(Self::Large),
            4 => Some(Self::Huge),
            _ => None,
        }
    }
}

/// Whether a region inherits anything from the region before it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionIndependence {
    /// A region continues the state the region before it left.
    Dependent,
    /// A region inherits nothing: no history, no entropy state, no offset history.
    Independent,
}

/// Where the frame places its integrity fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegrityMode {
    /// The frame carries no integrity field.
    Absent,
    /// Every region header carries an integrity field.
    PerRegion,
}

impl IntegrityMode {
    const fn code(self) -> u8 {
        match self {
            Self::Absent => 0,
            Self::PerRegion => 1,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Absent),
            1 => Some(Self::PerRegion),
            _ => None,
        }
    }

    /// The bytes a region header spends on integrity under this mode.
    #[must_use]
    pub const fn field_bytes(self) -> usize {
        match self {
            Self::Absent => 0,
            Self::PerRegion => INTEGRITY_FIELD_BYTES,
        }
    }
}

/// What a decoder learns about a frame before it decodes a byte of it.
///
/// ```text
///  0   4  magic
///  4   1  major version
///  5   1  flags, of which bits 4 to 7 are reject-reserved
///  6   1  resource class
///  7   1  integrity mode
///  8   4  ignore-reserved bits
/// 12   4  reject-reserved bits
/// 16   8  logical content length, when the flags declare it
///     4   dictionary identifier, when the flags declare it
///     8   index location, when the flags declare it
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameHeader {
    /// The memory class a decoder sizes itself from.
    pub resource_class: ResourceClass,
    /// Whether a region inherits state from the region before it.
    pub independence: RegionIndependence,
    /// Where the frame places its integrity fields.
    pub integrity: IntegrityMode,
    /// The decoded length of the whole frame, when the encoder knew it.
    pub content_length: Option<u64>,
    /// The dictionary the frame was built against, by identity only.
    pub dictionary_id: Option<u32>,
    /// Where the index sits in the stream.
    ///
    /// Nothing reads the structure this points at. The field declares that an index is
    /// present and where it starts.
    pub index_location: Option<u64>,
}

impl FrameHeader {
    /// A frame header with no optional field set.
    #[must_use]
    pub const fn new(
        resource_class: ResourceClass,
        independence: RegionIndependence,
        integrity: IntegrityMode,
    ) -> Self {
        Self {
            resource_class,
            independence,
            integrity,
            content_length: None,
            dictionary_id: None,
            index_location: None,
        }
    }

    /// The bytes this header occupies on the wire.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        let mut length = FRAME_HEADER_FIXED_BYTES;
        if self.content_length.is_some() {
            length = length.saturating_add(CONTENT_LENGTH_BYTES);
        }
        if self.dictionary_id.is_some() {
            length = length.saturating_add(DICTIONARY_ID_BYTES);
        }
        if self.index_location.is_some() {
            length = length.saturating_add(INDEX_LOCATION_BYTES);
        }
        length
    }

    /// Serializes this header.
    #[must_use]
    pub fn encode(&self) -> Encoded<FRAME_HEADER_MAX_BYTES> {
        let mut writer = Writer::<FRAME_HEADER_MAX_BYTES>::new();
        writer.put(u64::from(MAGIC), 4);
        writer.put(u64::from(MAJOR_VERSION), 1);
        writer.put(u64::from(self.flags()), 1);
        writer.put(u64::from(self.resource_class.code()), 1);
        writer.put(u64::from(self.integrity.code()), 1);
        writer.put(u64::from(IGNORE_RESERVED_DEFINED), 4);
        writer.put(0, 4);
        if let Some(length) = self.content_length {
            writer.put(length, 8);
        }
        if let Some(identifier) = self.dictionary_id {
            writer.put(u64::from(identifier), 4);
        }
        if let Some(location) = self.index_location {
            writer.put(location, 8);
        }
        writer.finish()
    }

    /// Parses a frame header and reports the bytes it consumed.
    ///
    /// The magic is checked first, then the major version, then the two reserved classes,
    /// then every enumerated field. Nothing later in the stream is read, and nothing is
    /// allocated, so a caller learns whether it can proceed before it commits anything.
    ///
    /// # Errors
    ///
    /// Returns `InvalidFormat` when the magic does not match, `UnsupportedVersion` when the
    /// major version is not implemented, `CorruptData` when a reject-reserved bit is set or
    /// an enumerated field is outside its enumeration, `UnsupportedFeature` when an
    /// ignore-reserved bit this version does not define is set, and `TruncatedInput` when
    /// the input is shorter than the header the flags describe.
    pub fn decode(input: &[u8]) -> Result<(Self, usize), Error> {
        let fixed = FrameFixed::read(input).ok_or(Error::TruncatedInput {
            needed: FRAME_HEADER_FIXED_BYTES,
        })?;

        if fixed.magic != MAGIC {
            return Err(Error::InvalidFormat);
        }
        if fixed.major != MAJOR_VERSION {
            return Err(Error::UnsupportedVersion { major: fixed.major });
        }
        if fixed.flags & FLAG_REJECT_RESERVED != 0 {
            return Err(Error::CorruptData(Corruption::FrameFlag));
        }
        if fixed.reject_reserved != 0 {
            return Err(Error::CorruptData(Corruption::FrameReserved));
        }
        if fixed.ignore_reserved & !IGNORE_RESERVED_DEFINED != 0 {
            return Err(Error::UnsupportedFeature(Feature::ReservedFrameFlag));
        }

        let resource_class = ResourceClass::from_code(fixed.resource)
            .ok_or(Error::CorruptData(Corruption::ResourceClass))?;
        let integrity = IntegrityMode::from_code(fixed.integrity)
            .ok_or(Error::CorruptData(Corruption::IntegrityMode))?;
        let independence = if fixed.flags & FLAG_REGION_INDEPENDENT == 0 {
            RegionIndependence::Dependent
        } else {
            RegionIndependence::Independent
        };

        let mut reader = Reader::at(input, FRAME_HEADER_FIXED_BYTES);
        let content_length = if fixed.flags & FLAG_CONTENT_LENGTH == 0 {
            None
        } else {
            let short = reader.truncated(CONTENT_LENGTH_BYTES);
            Some(reader.u64().ok_or(short)?)
        };
        let dictionary_id = if fixed.flags & FLAG_DICTIONARY_ID == 0 {
            None
        } else {
            let short = reader.truncated(DICTIONARY_ID_BYTES);
            Some(reader.u32().ok_or(short)?)
        };
        let index_location = if fixed.flags & FLAG_INDEX == 0 {
            None
        } else {
            let short = reader.truncated(INDEX_LOCATION_BYTES);
            Some(reader.u64().ok_or(short)?)
        };

        Ok((
            Self {
                resource_class,
                independence,
                integrity,
                content_length,
                dictionary_id,
                index_location,
            },
            reader.position(),
        ))
    }

    /// The bytes a frame occupies when `logical_bytes` are stored as RAW blocks.
    ///
    /// ```text
    /// regions = ceil(logical / region_bytes)
    /// blocks  = sum over regions of ceil(region logical / block_bytes)
    /// total   = frame header
    ///         + regions * (1 + region header)
    ///         + blocks * 4
    ///         + logical
    ///         + 1
    /// ```
    ///
    /// The region header is 16 bytes, and 24 when the frame declares per-region integrity.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when a size is zero or when the total does not fit in 64
    /// bits.
    pub fn raw_frame_bytes(
        &self,
        logical_bytes: u64,
        region_bytes: u64,
        block_bytes: u32,
    ) -> Result<u64, Error> {
        if region_bytes == 0 || block_bytes == 0 || block_bytes > MAX_BLOCK_BYTES {
            return Err(Error::InvalidParameter);
        }
        let block_bytes = u64::from(block_bytes);
        let full_regions = logical_bytes
            .checked_div(region_bytes)
            .ok_or(Error::InvalidParameter)?;
        let last_region = logical_bytes
            .checked_rem(region_bytes)
            .ok_or(Error::InvalidParameter)?;

        let per_full_region = region_bytes.div_ceil(block_bytes);
        let blocks = full_regions
            .checked_mul(per_full_region)
            .and_then(|count| count.checked_add(last_region.div_ceil(block_bytes)))
            .ok_or(Error::InvalidParameter)?;
        let regions = full_regions
            .checked_add(u64::from(last_region != 0))
            .ok_or(Error::InvalidParameter)?;

        let region_cost = (RECORD_TAG_BYTES as u64)
            .checked_add(REGION_HEADER_FIXED_BYTES as u64)
            .and_then(|cost| cost.checked_add(self.integrity.field_bytes() as u64))
            .ok_or(Error::InvalidParameter)?;

        (self.encoded_len() as u64)
            .checked_add(
                regions
                    .checked_mul(region_cost)
                    .ok_or(Error::InvalidParameter)?,
            )
            .and_then(|total| total.checked_add(blocks.checked_mul(BLOCK_HEADER_BYTES as u64)?))
            .and_then(|total| total.checked_add(logical_bytes))
            .and_then(|total| total.checked_add(TERMINATOR_BYTES as u64))
            .ok_or(Error::InvalidParameter)
    }

    /// The bytes a RAW frame adds over a copy of its content.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` under the same conditions as `raw_frame_bytes`.
    pub fn raw_overhead_bytes(
        &self,
        logical_bytes: u64,
        region_bytes: u64,
        block_bytes: u32,
    ) -> Result<u64, Error> {
        let total = self.raw_frame_bytes(logical_bytes, region_bytes, block_bytes)?;
        total
            .checked_sub(logical_bytes)
            .ok_or(Error::InvalidParameter)
    }

    const fn flags(&self) -> u8 {
        let mut flags = match self.independence {
            RegionIndependence::Dependent => 0,
            RegionIndependence::Independent => FLAG_REGION_INDEPENDENT,
        };
        if self.content_length.is_some() {
            flags |= FLAG_CONTENT_LENGTH;
        }
        if self.dictionary_id.is_some() {
            flags |= FLAG_DICTIONARY_ID;
        }
        if self.index_location.is_some() {
            flags |= FLAG_INDEX;
        }
        flags
    }
}

/// The limits a caller places on a frame before a decoder commits memory to it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecoderPolicy {
    max_history_bytes: u64,
}

impl DecoderPolicy {
    /// The default policy, which admits a frame up to the small resource class.
    ///
    /// A caller raises the limit deliberately.
    pub const CONSERVATIVE: Self = Self {
        max_history_bytes: ResourceClass::Small.history_bytes(),
    };

    /// A policy that admits a frame declaring at most `bytes` of decoder history.
    #[must_use]
    pub const fn with_max_history_bytes(bytes: u64) -> Self {
        Self {
            max_history_bytes: bytes,
        }
    }

    /// The history a decoder must reserve for this frame, when policy admits it.
    ///
    /// # Errors
    ///
    /// Returns `LimitExceeded` with the declared requirement and the allowed maximum when
    /// the frame asks for more than policy allows.
    pub const fn admit(&self, header: &FrameHeader) -> Result<u64, Error> {
        let declared = header.resource_class.history_bytes();
        if declared > self.max_history_bytes {
            return Err(Error::LimitExceeded {
                declared,
                allowed: self.max_history_bytes,
            });
        }
        Ok(declared)
    }
}

/// What follows the frame header, and what follows each region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Record {
    /// A region, and the header that declares its sizes.
    Region(RegionHeader),
    /// The end of the frame.
    Terminator,
}

impl Record {
    /// The bytes this record occupies on the wire, tag included.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        match *self {
            Self::Region(header) => RECORD_TAG_BYTES.saturating_add(header.encoded_len()),
            Self::Terminator => RECORD_TAG_BYTES,
        }
    }

    /// Serializes this record, tag included.
    #[must_use]
    pub fn encode(&self) -> Encoded<RECORD_MAX_BYTES> {
        let mut writer = Writer::<RECORD_MAX_BYTES>::new();
        match *self {
            Self::Region(header) => {
                writer.put(u64::from(TAG_REGION), 1);
                header.put(&mut writer);
            }
            Self::Terminator => writer.put(u64::from(TAG_TERMINATOR), 1),
        }
        writer.finish()
    }

    /// Parses the next record and reports the bytes it consumed.
    ///
    /// # Errors
    ///
    /// Returns `CorruptData` when the tag is undefined or a region header is out of range,
    /// and `TruncatedInput` when the input is shorter than the record. The truncation count
    /// is the whole record, tag included, so a caller that waits for it reads a record and
    /// not a header.
    pub fn decode(input: &[u8], integrity: IntegrityMode) -> Result<(Self, usize), Error> {
        let mut reader = Reader::new(input);
        let short = reader.truncated(RECORD_TAG_BYTES);
        let tag = reader.u8().ok_or(short)?;
        match tag {
            TAG_TERMINATOR => Ok((Self::Terminator, reader.position())),
            TAG_REGION => {
                let rest = input.get(RECORD_TAG_BYTES..).ok_or(Error::TruncatedInput {
                    needed: RECORD_TAG_BYTES,
                })?;
                let (header, used) = RegionHeader::decode(rest, integrity).map_err(after_tag)?;
                let consumed = used
                    .checked_add(RECORD_TAG_BYTES)
                    .ok_or(Error::CorruptData(Corruption::RegionPhysicalSize))?;
                Ok((Self::Region(header), consumed))
            }
            _ => Err(Error::CorruptData(Corruption::RecordTag)),
        }
    }
}

/// What a decoder learns about one region before it reads a block of it.
///
/// ```text
///  0   8  logical size, the bytes the region decodes to
///  8   8  physical size, the bytes its blocks occupy after this header
/// 16   8  integrity field, when the frame declares per-region integrity
/// ```
///
/// The physical size is what lets a reader reach the next record without an index. Nothing
/// computes the integrity field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionHeader {
    /// The bytes this region decodes to.
    pub logical_size: u64,
    /// The bytes this region's blocks occupy, counted after this header.
    pub physical_size: u64,
    /// The reserved integrity field, present when the frame declares per-region integrity.
    pub integrity: Option<u64>,
}

/// The smallest region a block can fill: one block header and one payload byte.
const MIN_REGION_PHYSICAL_BYTES: u64 = 5;

impl RegionHeader {
    /// A region header for a frame with the given integrity mode.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the sizes cannot describe a region that holds at
    /// least one block.
    pub const fn new(
        logical_size: u64,
        physical_size: u64,
        integrity: IntegrityMode,
    ) -> Result<Self, Error> {
        if logical_size == 0 || physical_size < MIN_REGION_PHYSICAL_BYTES {
            return Err(Error::InvalidParameter);
        }
        Ok(Self {
            logical_size,
            physical_size,
            integrity: match integrity {
                IntegrityMode::Absent => None,
                IntegrityMode::PerRegion => Some(0),
            },
        })
    }

    /// The bytes this header occupies on the wire.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        if self.integrity.is_some() {
            REGION_HEADER_FIXED_BYTES.saturating_add(INTEGRITY_FIELD_BYTES)
        } else {
            REGION_HEADER_FIXED_BYTES
        }
    }

    /// Serializes this header, without its record tag.
    #[must_use]
    pub fn encode(&self) -> Encoded<REGION_HEADER_MAX_BYTES> {
        let mut writer = Writer::<REGION_HEADER_MAX_BYTES>::new();
        self.put(&mut writer);
        writer.finish()
    }

    fn put<const N: usize>(&self, writer: &mut Writer<N>) {
        writer.put(self.logical_size, 8);
        writer.put(self.physical_size, 8);
        if let Some(field) = self.integrity {
            writer.put(field, 8);
        }
    }

    /// Parses a region header, without its record tag, and reports the bytes it consumed.
    ///
    /// # Errors
    ///
    /// Returns `CorruptData` when a declared size cannot describe a region, and
    /// `TruncatedInput` when the input is shorter than the header.
    pub fn decode(input: &[u8], integrity: IntegrityMode) -> Result<(Self, usize), Error> {
        let mut reader = Reader::new(input);
        let short = reader.truncated(8);
        let logical_size = reader.u64().ok_or(short)?;
        let short = reader.truncated(8);
        let physical_size = reader.u64().ok_or(short)?;
        let field = match integrity {
            IntegrityMode::Absent => None,
            IntegrityMode::PerRegion => {
                let short = reader.truncated(INTEGRITY_FIELD_BYTES);
                Some(reader.u64().ok_or(short)?)
            }
        };

        if logical_size == 0 {
            return Err(Error::CorruptData(Corruption::RegionLogicalSize));
        }
        if physical_size < MIN_REGION_PHYSICAL_BYTES {
            return Err(Error::CorruptData(Corruption::RegionPhysicalSize));
        }

        Ok((
            Self {
                logical_size,
                physical_size,
                integrity: field,
            },
            reader.position(),
        ))
    }
}

/// How a block stores its content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockType {
    /// The payload is the content, byte for byte.
    Raw,
    /// The payload is one byte, repeated to the declared size.
    Rle,
}

/// What a decoder learns about one block before it reads the payload.
///
/// The header is one little-endian 32-bit word:
///
/// ```text
/// bit 0        last block in its region
/// bits 1 to 2  block type
/// bits 3 to 31 size
/// ```
///
/// The size is the decoded size for every type. A RAW block stores that many bytes and an
/// RLE block stores one, which is what makes a run cost one byte instead of a length and a
/// value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockHeader {
    /// Whether this is the last block of its region.
    pub last: bool,
    /// How this block stores its content.
    pub kind: BlockType,
    /// The bytes this block decodes to.
    pub size: u32,
}

impl BlockHeader {
    /// A RAW block header.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the size is zero or above the field's range.
    pub const fn raw(last: bool, size: u32) -> Result<Self, Error> {
        Self::build(last, BlockType::Raw, size)
    }

    /// An RLE block header, which declares the decoded size of a one-byte payload.
    ///
    /// # Errors
    ///
    /// Returns `InvalidParameter` when the size is zero or above the field's range.
    pub const fn rle(last: bool, size: u32) -> Result<Self, Error> {
        Self::build(last, BlockType::Rle, size)
    }

    /// The bytes this block's payload occupies on the wire.
    #[must_use]
    pub const fn stored_len(&self) -> u32 {
        match self.kind {
            BlockType::Raw => self.size,
            BlockType::Rle => 1,
        }
    }

    /// The bytes this block decodes to.
    #[must_use]
    pub const fn decoded_len(&self) -> u32 {
        self.size
    }

    /// Serializes this header.
    #[must_use]
    pub fn encode(&self) -> Encoded<BLOCK_HEADER_BYTES> {
        let code = match self.kind {
            BlockType::Raw => BLOCK_CODE_RAW,
            BlockType::Rle => BLOCK_CODE_RLE,
        };
        let word = u32::from(self.last)
            | shift_left(code, BLOCK_TYPE_SHIFT)
            | shift_left(self.size, BLOCK_SIZE_SHIFT);
        let mut writer = Writer::<BLOCK_HEADER_BYTES>::new();
        writer.put(u64::from(word), 4);
        writer.finish()
    }

    /// Parses a block header and reports the bytes it consumed.
    ///
    /// # Errors
    ///
    /// Returns `UnsupportedFeature` for a compressed block, `CorruptData` for the reserved
    /// block type or a size the format does not permit, and `TruncatedInput` when the input
    /// is shorter than the header.
    pub fn decode(input: &[u8]) -> Result<(Self, usize), Error> {
        let mut reader = Reader::new(input);
        let short = reader.truncated(BLOCK_HEADER_BYTES);
        let word = reader.u32().ok_or(short)?;

        let kind = match shift_right(word, BLOCK_TYPE_SHIFT) & BLOCK_TYPE_MASK {
            BLOCK_CODE_RAW => BlockType::Raw,
            BLOCK_CODE_RLE => BlockType::Rle,
            BLOCK_CODE_COMPRESSED => {
                return Err(Error::UnsupportedFeature(Feature::CompressedBlock));
            }
            _ => return Err(Error::CorruptData(Corruption::BlockType)),
        };
        let size = shift_right(word, BLOCK_SIZE_SHIFT);
        if size == 0 {
            return Err(Error::CorruptData(Corruption::BlockSize));
        }

        Ok((
            Self {
                last: word & BLOCK_LAST_BIT != 0,
                kind,
                size,
            },
            reader.position(),
        ))
    }

    /// Borrows this block's payload from the bytes that follow its header.
    ///
    /// # Errors
    ///
    /// Returns `TruncatedInput` when `input` is shorter than the payload.
    pub fn payload<'a>(&self, input: &'a [u8]) -> Result<&'a [u8], Error> {
        let stored = as_usize(self.stored_len())?;
        input
            .get(..stored)
            .ok_or(Error::TruncatedInput { needed: stored })
    }

    /// Writes this block's content into a caller-supplied buffer.
    ///
    /// Nothing is allocated. The caller sizes the buffer from `decoded_len`, which the
    /// header declared and this call validates against the payload it was given.
    ///
    /// # Errors
    ///
    /// Returns `OutputTooSmall` when the buffer is shorter than the declared size, and
    /// `CorruptData` when the payload does not match what the header declared.
    pub fn expand(&self, payload: &[u8], out: &mut [u8]) -> Result<usize, Error> {
        let stored = as_usize(self.stored_len())?;
        let decoded = as_usize(self.decoded_len())?;
        if payload.len() != stored {
            return Err(Error::CorruptData(Corruption::BlockSize));
        }
        let target = out
            .get_mut(..decoded)
            .ok_or(Error::OutputTooSmall { needed: decoded })?;
        match self.kind {
            BlockType::Raw => target.copy_from_slice(payload),
            BlockType::Rle => {
                let value = payload
                    .first()
                    .copied()
                    .ok_or(Error::CorruptData(Corruption::BlockSize))?;
                target.fill(value);
            }
        }
        Ok(decoded)
    }

    const fn build(last: bool, kind: BlockType, size: u32) -> Result<Self, Error> {
        if size == 0 || size > MAX_BLOCK_BYTES {
            return Err(Error::InvalidParameter);
        }
        Ok(Self { last, kind, size })
    }
}

/// The wire bytes of one header, and how many of them the header uses.
///
/// The buffer is sized for the widest form of its header, so serializing allocates nothing
/// and cannot fail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Encoded<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> Encoded<N> {
    /// The bytes this header occupies.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or(&[])
    }

    /// How many bytes this header occupies.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether this header occupies no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

struct FrameFixed {
    magic: u32,
    major: u8,
    flags: u8,
    resource: u8,
    integrity: u8,
    ignore_reserved: u32,
    reject_reserved: u32,
}

impl FrameFixed {
    fn read(input: &[u8]) -> Option<Self> {
        let mut reader = Reader::new(input);
        Some(Self {
            magic: reader.u32()?,
            major: reader.u8()?,
            flags: reader.u8()?,
            resource: reader.u8()?,
            integrity: reader.u8()?,
            ignore_reserved: reader.u32()?,
            reject_reserved: reader.u32()?,
        })
    }
}

struct Reader<'a> {
    input: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    const fn new(input: &'a [u8]) -> Self {
        Self { input, at: 0 }
    }

    const fn at(input: &'a [u8], at: usize) -> Self {
        Self { input, at }
    }

    const fn position(&self) -> usize {
        self.at
    }

    const fn truncated(&self, width: usize) -> Error {
        Error::TruncatedInput {
            needed: self.at.saturating_add(width),
        }
    }

    fn take(&mut self, width: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(width)?;
        let field = self.input.get(self.at..end)?;
        self.at = end;
        Some(field)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    fn u32(&mut self) -> Option<u32> {
        u32::try_from(little_endian(self.take(4)?)).ok()
    }

    fn u64(&mut self) -> Option<u64> {
        Some(little_endian(self.take(8)?))
    }
}

struct Writer<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> Writer<N> {
    const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
        }
    }

    fn put(&mut self, value: u64, width: u32) {
        debug_assert!(
            self.len.saturating_add(width as usize) <= N,
            "the encode buffer is sized for the widest form of its structure"
        );
        let mut lane = 0;
        while lane < width {
            if let Some(slot) = self.bytes.get_mut(self.len) {
                *slot = byte_of(value, lane);
                self.len = self.len.saturating_add(1);
            }
            lane = lane.saturating_add(1);
        }
    }

    const fn finish(self) -> Encoded<N> {
        Encoded {
            bytes: self.bytes,
            len: self.len,
        }
    }
}

/// Reads up to eight bytes as one little-endian value, by explicit byte position and shift.
fn little_endian(field: &[u8]) -> u64 {
    let mut value = 0;
    let mut lane = 0;
    for byte in field {
        value |= place(*byte, lane);
        lane = lane.saturating_add(1);
    }
    value
}

// The shift is below the width of the result at every call site, because a field is at most
// eight bytes wide, so it cannot overflow.
#[allow(clippy::arithmetic_side_effects)]
const fn place(byte: u8, lane: u32) -> u64 {
    (byte as u64) << (lane * 8)
}

// The mask makes the narrowing exact, and the shift is below the width of the value at every
// call site, so neither operation loses a bit.
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
const fn byte_of(value: u64, lane: u32) -> u8 {
    ((value >> (lane * 8)) & 0xFF) as u8
}

// The shift is a format constant below 32, so it cannot overflow the word.
#[allow(clippy::arithmetic_side_effects)]
const fn shift_left(value: u32, by: u32) -> u32 {
    value << by
}

// The shift is a format constant below 32, so it cannot overflow the word.
#[allow(clippy::arithmetic_side_effects)]
const fn shift_right(value: u32, by: u32) -> u32 {
    value >> by
}

/// Restates a region-header truncation as the bytes the whole record needs.
const fn after_tag(error: Error) -> Error {
    match error {
        Error::TruncatedInput { needed } => Error::TruncatedInput {
            needed: needed.saturating_add(RECORD_TAG_BYTES),
        },
        other => other,
    }
}

fn as_usize(value: u32) -> Result<usize, Error> {
    usize::try_from(value).map_err(|_| Error::LimitExceeded {
        declared: u64::from(value),
        allowed: u64::try_from(usize::MAX).unwrap_or(u64::MAX),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        BLOCK_HEADER_BYTES, BlockHeader, BlockType, Corruption, DecoderPolicy, Error,
        FLAG_REJECT_RESERVED, FRAME_HEADER_FIXED_BYTES, Feature, FrameHeader,
        INTEGRITY_FIELD_BYTES, IntegrityMode, MAGIC, MAJOR_VERSION, MAX_BLOCK_BYTES,
        RECORD_TAG_BYTES, REGION_HEADER_FIXED_BYTES, Record, RegionHeader, RegionIndependence,
        ResourceClass, TAG_REGION, TERMINATOR_BYTES, byte_of,
    };

    /// The largest block the test decoder will size a buffer for.
    ///
    /// The walker below models the contract the module states: a declared length is checked
    /// against a limit before anything is sized from it.
    const BLOCK_LIMIT: u32 = 1_048_576;

    const CLASSES: [ResourceClass; 5] = [
        ResourceClass::Minimal,
        ResourceClass::Small,
        ResourceClass::Medium,
        ResourceClass::Large,
        ResourceClass::Huge,
    ];
    const INDEPENDENCE: [RegionIndependence; 2] = [
        RegionIndependence::Dependent,
        RegionIndependence::Independent,
    ];
    const INTEGRITY: [IntegrityMode; 2] = [IntegrityMode::Absent, IntegrityMode::PerRegion];

    fn sample(len: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(len);
        let mut state: u32 = 0x1234_5678;
        while data.len() < len {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            data.push(byte_of(u64::from(state), 2));
        }
        data
    }

    fn build_raw_region(region: &[u8], block_bytes: usize) -> Result<Vec<u8>, Error> {
        let mut body = Vec::new();
        let mut at = 0;
        while at < region.len() {
            let end = at.saturating_add(block_bytes).min(region.len());
            let chunk = region.get(at..end).ok_or(Error::InvalidParameter)?;
            let size = u32::try_from(chunk.len()).map_err(|_| Error::InvalidParameter)?;
            let block = BlockHeader::raw(end == region.len(), size)?;
            body.extend_from_slice(block.encode().as_bytes());
            body.extend_from_slice(chunk);
            at = end;
        }
        Ok(body)
    }

    fn build_raw_frame(
        header: &FrameHeader,
        data: &[u8],
        region_bytes: usize,
        block_bytes: usize,
    ) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        out.extend_from_slice(header.encode().as_bytes());
        let mut offset = 0;
        while offset < data.len() {
            let end = offset.saturating_add(region_bytes).min(data.len());
            let region = data.get(offset..end).ok_or(Error::InvalidParameter)?;
            let body = build_raw_region(region, block_bytes)?;
            let logical = u64::try_from(region.len()).map_err(|_| Error::InvalidParameter)?;
            let physical = u64::try_from(body.len()).map_err(|_| Error::InvalidParameter)?;
            let region_header = RegionHeader::new(logical, physical, header.integrity)?;
            out.extend_from_slice(Record::Region(region_header).encode().as_bytes());
            out.extend_from_slice(&body);
            offset = end;
        }
        out.extend_from_slice(Record::Terminator.encode().as_bytes());
        Ok(out)
    }

    fn decode_region(
        bytes: &[u8],
        start: usize,
        region: &RegionHeader,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        let mut at = start;
        let mut logical: u64 = 0;
        loop {
            let rest = bytes
                .get(at..)
                .ok_or(Error::TruncatedInput { needed: at })?;
            let (block, used) = BlockHeader::decode(rest)?;
            let declared = block.decoded_len();
            if declared > BLOCK_LIMIT {
                return Err(Error::LimitExceeded {
                    declared: u64::from(declared),
                    allowed: u64::from(BLOCK_LIMIT),
                });
            }
            at = at.checked_add(used).ok_or(Error::InvalidParameter)?;
            let rest = bytes
                .get(at..)
                .ok_or(Error::TruncatedInput { needed: at })?;
            let payload = block.payload(rest)?;
            let decoded = usize::try_from(declared).map_err(|_| Error::InvalidParameter)?;
            let base = out.len();
            out.resize(base.checked_add(decoded).ok_or(Error::InvalidParameter)?, 0);
            let target = out.get_mut(base..).ok_or(Error::InvalidParameter)?;
            let written = block.expand(payload, target)?;
            logical = logical
                .checked_add(u64::try_from(written).map_err(|_| Error::InvalidParameter)?)
                .ok_or(Error::InvalidParameter)?;
            at = at
                .checked_add(payload.len())
                .ok_or(Error::InvalidParameter)?;
            if block.last {
                break;
            }
        }
        let spent = at.checked_sub(start).ok_or(Error::InvalidParameter)?;
        let physical = u64::try_from(spent).map_err(|_| Error::InvalidParameter)?;
        if logical != region.logical_size || physical != region.physical_size {
            return Err(Error::CorruptData(Corruption::RegionMismatch));
        }
        Ok(at)
    }

    fn decode_frame(bytes: &[u8]) -> Result<(FrameHeader, Vec<u8>), Error> {
        let (header, used) = FrameHeader::decode(bytes)?;
        let mut at = used;
        let mut out = Vec::new();
        loop {
            let rest = bytes
                .get(at..)
                .ok_or(Error::TruncatedInput { needed: at })?;
            let (record, consumed) = Record::decode(rest, header.integrity)?;
            at = at.checked_add(consumed).ok_or(Error::InvalidParameter)?;
            match record {
                Record::Terminator => break,
                Record::Region(region) => {
                    at = decode_region(bytes, at, &region, &mut out)?;
                }
            }
        }
        if let Some(declared) = header.content_length {
            let produced = u64::try_from(out.len()).map_err(|_| Error::InvalidParameter)?;
            if declared != produced {
                return Err(Error::CorruptData(Corruption::RegionMismatch));
            }
        }
        Ok((header, out))
    }

    fn every_legal_header() -> Vec<FrameHeader> {
        let mut headers = Vec::new();
        for class in CLASSES {
            for independence in INDEPENDENCE {
                for integrity in INTEGRITY {
                    for content in [None, Some(0x0102_0304_0506_0708_u64)] {
                        for dictionary in [None, Some(0x0A0B_0C0D_u32)] {
                            for index in [None, Some(0x1112_1314_1516_1718_u64)] {
                                let mut header = FrameHeader::new(class, independence, integrity);
                                header.content_length = content;
                                header.dictionary_id = dictionary;
                                header.index_location = index;
                                headers.push(header);
                            }
                        }
                    }
                }
            }
        }
        headers
    }

    #[test]
    fn the_magic_avoids_a_trivial_pattern() {
        let wire = [
            byte_of(u64::from(MAGIC), 0),
            byte_of(u64::from(MAGIC), 1),
            byte_of(u64::from(MAGIC), 2),
            byte_of(u64::from(MAGIC), 3),
        ];
        assert!(wire.iter().all(|byte| *byte != 0x00 && *byte != 0xFF));
        assert!(wire.iter().any(|byte| *byte >= 0x80));
        for (position, byte) in wire.iter().enumerate() {
            let later = wire.get(position.saturating_add(1)..).unwrap_or(&[]);
            assert!(!later.contains(byte), "the magic repeats a byte");
        }
        assert!(core::str::from_utf8(&wire).is_err(), "the magic is UTF-8");
    }

    #[test]
    fn a_frame_header_round_trips_over_every_legal_field_combination() -> Result<(), Error> {
        for header in every_legal_header() {
            let encoded = header.encode();
            assert_eq!(encoded.len(), header.encoded_len());
            let (decoded, consumed) = FrameHeader::decode(encoded.as_bytes())?;
            assert_eq!(decoded, header);
            assert_eq!(consumed, encoded.len());
        }
        Ok(())
    }

    #[test]
    fn every_multi_byte_field_is_little_endian() {
        let mut header = FrameHeader::new(
            ResourceClass::Medium,
            RegionIndependence::Independent,
            IntegrityMode::PerRegion,
        );
        header.content_length = Some(0x0807_0605_0403_0201);
        header.dictionary_id = Some(0x0D0C_0B0A);
        let encoded = header.encode();
        assert_eq!(
            encoded.as_bytes(),
            &[
                0xE7,
                0x51,
                0xB3,
                0x2A,
                0x01,
                0b0000_0111,
                0x02,
                0x01,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x01,
                0x02,
                0x03,
                0x04,
                0x05,
                0x06,
                0x07,
                0x08,
                0x0A,
                0x0B,
                0x0C,
                0x0D,
            ]
        );
    }

    #[test]
    fn bytes_that_are_not_a_frame_are_rejected_before_any_field() {
        let mut bytes = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Dependent,
            IntegrityMode::Absent,
        )
        .encode()
        .as_bytes()
        .to_vec();
        if let Some(slot) = bytes.first_mut() {
            *slot ^= 0xFF;
        }
        assert_eq!(FrameHeader::decode(&bytes), Err(Error::InvalidFormat));
    }

    #[test]
    fn an_unimplemented_major_version_has_its_own_error() {
        let base = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Dependent,
            IntegrityMode::Absent,
        )
        .encode();
        for major in 0..=u8::MAX {
            if major == MAJOR_VERSION {
                continue;
            }
            let mut bytes = base.as_bytes().to_vec();
            if let Some(slot) = bytes.get_mut(4) {
                *slot = major;
            }
            assert_eq!(
                FrameHeader::decode(&bytes),
                Err(Error::UnsupportedVersion { major }),
                "version {major}"
            );
        }
    }

    #[test]
    fn a_set_reject_reserved_bit_is_corrupt_data() {
        let base = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Dependent,
            IntegrityMode::Absent,
        )
        .encode();
        for bit in 0..8_u32 {
            let mask = byte_of(1, 0).checked_shl(bit).unwrap_or(0);
            if mask & FLAG_REJECT_RESERVED == 0 {
                continue;
            }
            let mut bytes = base.as_bytes().to_vec();
            if let Some(slot) = bytes.get_mut(5) {
                *slot |= mask;
            }
            assert_eq!(
                FrameHeader::decode(&bytes),
                Err(Error::CorruptData(Corruption::FrameFlag)),
                "flag bit {bit}"
            );
        }
        for position in 12..16 {
            for bit in 0..8_u32 {
                let mut bytes = base.as_bytes().to_vec();
                if let Some(slot) = bytes.get_mut(position) {
                    *slot |= byte_of(1, 0).checked_shl(bit).unwrap_or(0);
                }
                assert_eq!(
                    FrameHeader::decode(&bytes),
                    Err(Error::CorruptData(Corruption::FrameReserved)),
                    "reserved byte {position} bit {bit}"
                );
            }
        }
    }

    #[test]
    fn an_undefined_ignore_reserved_bit_is_an_unsupported_feature() {
        let base = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Dependent,
            IntegrityMode::Absent,
        )
        .encode();
        for position in 8..12 {
            for bit in 0..8_u32 {
                let mut bytes = base.as_bytes().to_vec();
                if let Some(slot) = bytes.get_mut(position) {
                    *slot |= byte_of(1, 0).checked_shl(bit).unwrap_or(0);
                }
                assert_eq!(
                    FrameHeader::decode(&bytes),
                    Err(Error::UnsupportedFeature(Feature::ReservedFrameFlag)),
                    "ignore-reserved byte {position} bit {bit}"
                );
            }
        }
    }

    #[test]
    fn a_resource_class_outside_the_enumeration_is_corrupt_data() {
        let base = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Dependent,
            IntegrityMode::Absent,
        )
        .encode();
        for code in 5..=u8::MAX {
            let mut bytes = base.as_bytes().to_vec();
            if let Some(slot) = bytes.get_mut(6) {
                *slot = code;
            }
            assert_eq!(
                FrameHeader::decode(&bytes),
                Err(Error::CorruptData(Corruption::ResourceClass)),
                "class {code}"
            );
        }
    }

    #[test]
    fn an_integrity_mode_outside_the_enumeration_is_corrupt_data() {
        let base = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Dependent,
            IntegrityMode::Absent,
        )
        .encode();
        for code in 2..=u8::MAX {
            let mut bytes = base.as_bytes().to_vec();
            if let Some(slot) = bytes.get_mut(7) {
                *slot = code;
            }
            assert_eq!(
                FrameHeader::decode(&bytes),
                Err(Error::CorruptData(Corruption::IntegrityMode)),
                "mode {code}"
            );
        }
    }

    #[test]
    fn a_short_frame_header_asks_for_more_input() {
        let mut header = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Dependent,
            IntegrityMode::Absent,
        );
        header.content_length = Some(1);
        let encoded = header.encode();
        for length in 0..encoded.len() {
            let short = encoded.as_bytes().get(..length).unwrap_or(&[]);
            let result = FrameHeader::decode(short);
            assert!(
                matches!(result, Err(Error::TruncatedInput { needed }) if needed > length),
                "length {length} gave {result:?}"
            );
        }
    }

    #[test]
    fn the_policy_rejects_a_frame_that_declares_more_than_it_allows() -> Result<(), Error> {
        let small = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Dependent,
            IntegrityMode::Absent,
        );
        let huge = FrameHeader::new(
            ResourceClass::Huge,
            RegionIndependence::Dependent,
            IntegrityMode::Absent,
        );
        assert_eq!(
            DecoderPolicy::CONSERVATIVE.admit(&small)?,
            ResourceClass::Small.history_bytes()
        );
        assert_eq!(
            DecoderPolicy::CONSERVATIVE.admit(&huge),
            Err(Error::LimitExceeded {
                declared: ResourceClass::Huge.history_bytes(),
                allowed: ResourceClass::Small.history_bytes(),
            })
        );
        Ok(())
    }

    #[test]
    fn a_region_is_reached_by_physical_size_alone() -> Result<(), Error> {
        let header = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Independent,
            IntegrityMode::PerRegion,
        );
        let data = sample(3_000);
        let bytes = build_raw_frame(&header, &data, 1_024, 300)?;
        assert!(header.index_location.is_none());

        let (decoded, used) = FrameHeader::decode(&bytes)?;
        let mut at = used;
        let mut regions: u32 = 0;
        loop {
            let rest = bytes.get(at..).ok_or(Error::InvalidParameter)?;
            let (record, consumed) = Record::decode(rest, decoded.integrity)?;
            at = at.checked_add(consumed).ok_or(Error::InvalidParameter)?;
            match record {
                Record::Terminator => break,
                Record::Region(region) => {
                    regions = regions.saturating_add(1);
                    let skip = usize::try_from(region.physical_size)
                        .map_err(|_| Error::InvalidParameter)?;
                    at = at.checked_add(skip).ok_or(Error::InvalidParameter)?;
                }
            }
        }
        assert_eq!(regions, 3);
        assert_eq!(at, bytes.len());
        Ok(())
    }

    #[test]
    fn raw_blocks_round_trip() -> Result<(), Error> {
        for header in every_legal_header() {
            let data = sample(700);
            let mut header = header;
            header.content_length = header.content_length.map(|_| 700);
            let bytes = build_raw_frame(&header, &data, 256, 100)?;
            let (decoded, content) = decode_frame(&bytes)?;
            assert_eq!(decoded, header);
            assert_eq!(content, data);
        }
        Ok(())
    }

    #[test]
    fn an_empty_frame_carries_no_region() -> Result<(), Error> {
        let header = FrameHeader::new(
            ResourceClass::Minimal,
            RegionIndependence::Independent,
            IntegrityMode::Absent,
        );
        let bytes = build_raw_frame(&header, &[], 1_024, 256)?;
        assert_eq!(
            bytes.len(),
            header.encoded_len().saturating_add(TERMINATOR_BYTES)
        );
        let (_, content) = decode_frame(&bytes)?;
        assert!(content.is_empty());
        Ok(())
    }

    #[test]
    fn the_raw_expansion_bound_holds_at_the_sizes_that_stress_it() -> Result<(), Error> {
        let region_bytes: usize = 4_096;
        let block_bytes: usize = 1_024;
        let sizes = [
            0, 1, 2, 1_023, 1_024, 1_025, 4_095, 4_096, 4_097, 8_192, 8_193, 12_290,
        ];
        for integrity in INTEGRITY {
            let header = FrameHeader::new(
                ResourceClass::Small,
                RegionIndependence::Independent,
                integrity,
            );
            for size in sizes {
                let data = sample(size);
                let bytes = build_raw_frame(&header, &data, region_bytes, block_bytes)?;
                let logical = u64::try_from(size).map_err(|_| Error::InvalidParameter)?;
                let predicted = header.raw_frame_bytes(
                    logical,
                    u64::try_from(region_bytes).map_err(|_| Error::InvalidParameter)?,
                    u32::try_from(block_bytes).map_err(|_| Error::InvalidParameter)?,
                )?;
                let actual = u64::try_from(bytes.len()).map_err(|_| Error::InvalidParameter)?;
                assert_eq!(predicted, actual, "size {size}");
                assert_eq!(
                    header.raw_overhead_bytes(
                        logical,
                        u64::try_from(region_bytes).map_err(|_| Error::InvalidParameter)?,
                        u32::try_from(block_bytes).map_err(|_| Error::InvalidParameter)?,
                    )?,
                    actual.saturating_sub(logical)
                );
            }
        }
        Ok(())
    }

    #[test]
    fn rle_declares_its_decoded_size_with_a_one_byte_payload() -> Result<(), Error> {
        let count: u32 = 131_072;
        let block = BlockHeader::rle(true, count)?;
        assert_eq!(block.stored_len(), 1);
        assert_eq!(block.decoded_len(), count);

        let encoded = block.encode();
        assert_eq!(encoded.len(), BLOCK_HEADER_BYTES);
        let (decoded, used) = BlockHeader::decode(encoded.as_bytes())?;
        assert_eq!(used, BLOCK_HEADER_BYTES);
        assert_eq!(decoded, block);
        assert_eq!(decoded.kind, BlockType::Rle);

        let mut out = vec![0_u8; 131_072];
        let written = decoded.expand(&[0x5A], &mut out)?;
        assert_eq!(written, 131_072);
        assert!(out.iter().all(|byte| *byte == 0x5A));
        Ok(())
    }

    #[test]
    fn an_rle_region_round_trips_through_the_frame() -> Result<(), Error> {
        let header = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Independent,
            IntegrityMode::PerRegion,
        );
        let block = BlockHeader::rle(true, 4_096)?;
        let mut body = block.encode().as_bytes().to_vec();
        body.push(0x7E);
        let physical = u64::try_from(body.len()).map_err(|_| Error::InvalidParameter)?;
        let region = RegionHeader::new(4_096, physical, header.integrity)?;

        let mut bytes = header.encode().as_bytes().to_vec();
        bytes.extend_from_slice(Record::Region(region).encode().as_bytes());
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(Record::Terminator.encode().as_bytes());

        let (_, content) = decode_frame(&bytes)?;
        assert_eq!(content.len(), 4_096);
        assert!(content.iter().all(|byte| *byte == 0x7E));
        assert_eq!(bytes.len(), 16 + 25 + 5 + 1);
        Ok(())
    }

    #[test]
    fn a_compressed_block_is_an_unsupported_feature() {
        let word: u32 = 0b0000_0100 | 0b1000_0000;
        let bytes = [
            byte_of(u64::from(word), 0),
            byte_of(u64::from(word), 1),
            byte_of(u64::from(word), 2),
            byte_of(u64::from(word), 3),
        ];
        assert_eq!(
            BlockHeader::decode(&bytes),
            Err(Error::UnsupportedFeature(Feature::CompressedBlock))
        );
    }

    #[test]
    fn a_reserved_block_type_is_corrupt_data() {
        let word: u32 = 0b0000_0110 | 0b1000_0000;
        let bytes = [
            byte_of(u64::from(word), 0),
            byte_of(u64::from(word), 1),
            byte_of(u64::from(word), 2),
            byte_of(u64::from(word), 3),
        ];
        assert_eq!(
            BlockHeader::decode(&bytes),
            Err(Error::CorruptData(Corruption::BlockType))
        );
    }

    #[test]
    fn a_block_that_decodes_to_nothing_is_corrupt_data() {
        assert_eq!(
            BlockHeader::decode(&[0, 0, 0, 0]),
            Err(Error::CorruptData(Corruption::BlockSize))
        );
        assert_eq!(BlockHeader::raw(true, 0), Err(Error::InvalidParameter));
        assert_eq!(
            BlockHeader::raw(true, MAX_BLOCK_BYTES.saturating_add(1)),
            Err(Error::InvalidParameter)
        );
    }

    #[test]
    fn an_undefined_record_tag_is_corrupt_data() {
        for tag in 2..=u8::MAX {
            assert_eq!(
                Record::decode(&[tag], IntegrityMode::Absent),
                Err(Error::CorruptData(Corruption::RecordTag)),
                "tag {tag}"
            );
        }
    }

    #[test]
    fn a_region_that_decodes_to_nothing_is_corrupt_data() {
        let mut bytes = vec![TAG_REGION];
        bytes.resize(
            RECORD_TAG_BYTES.saturating_add(REGION_HEADER_FIXED_BYTES),
            0,
        );
        if let Some(slot) = bytes.get_mut(9) {
            *slot = 0x10;
        }
        assert_eq!(
            Record::decode(&bytes, IntegrityMode::Absent),
            Err(Error::CorruptData(Corruption::RegionLogicalSize))
        );
        assert_eq!(
            RegionHeader::new(0, 64, IntegrityMode::Absent),
            Err(Error::InvalidParameter)
        );
        assert_eq!(
            RegionHeader::new(64, 4, IntegrityMode::Absent),
            Err(Error::InvalidParameter)
        );
    }

    #[test]
    fn a_region_too_small_to_hold_a_block_is_corrupt_data() {
        let mut bytes = vec![TAG_REGION];
        bytes.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0]);
        bytes.extend_from_slice(&[4, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            Record::decode(&bytes, IntegrityMode::Absent),
            Err(Error::CorruptData(Corruption::RegionPhysicalSize))
        );
    }

    #[test]
    fn a_stream_cut_at_every_byte_offset_fails() -> Result<(), Error> {
        let mut header = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Independent,
            IntegrityMode::PerRegion,
        );
        header.content_length = Some(2_000);
        header.dictionary_id = Some(9);
        let data = sample(2_000);
        let bytes = build_raw_frame(&header, &data, 768, 200)?;
        assert!(decode_frame(&bytes).is_ok());

        for length in 0..bytes.len() {
            let cut = bytes.get(..length).ok_or(Error::InvalidParameter)?;
            assert!(
                decode_frame(cut).is_err(),
                "a stream cut at {length} decoded as a whole frame"
            );
        }
        Ok(())
    }

    #[test]
    fn a_frame_with_a_content_length_decodes_to_the_same_bytes_as_one_without() -> Result<(), Error>
    {
        let data = sample(1_500);
        let plain = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Independent,
            IntegrityMode::Absent,
        );
        let mut declared = plain;
        declared.content_length = Some(1_500);

        let (_, without) = decode_frame(&build_raw_frame(&plain, &data, 512, 128)?)?;
        let (_, with) = decode_frame(&build_raw_frame(&declared, &data, 512, 128)?)?;
        assert_eq!(without, data);
        assert_eq!(with, data);
        assert_eq!(
            declared.encoded_len(),
            plain.encoded_len().saturating_add(8)
        );
        Ok(())
    }

    #[test]
    fn a_declared_length_is_checked_before_anything_is_sized_from_it() -> Result<(), Error> {
        let block = BlockHeader::raw(true, MAX_BLOCK_BYTES)?;
        let encoded = block.encode();
        let (decoded, used) = BlockHeader::decode(encoded.as_bytes())?;
        assert_eq!(used, BLOCK_HEADER_BYTES);
        assert_eq!(decoded.decoded_len(), MAX_BLOCK_BYTES);
        assert_eq!(
            decoded.payload(&[]),
            Err(Error::TruncatedInput {
                needed: usize::try_from(MAX_BLOCK_BYTES).map_err(|_| Error::InvalidParameter)?,
            })
        );
        let mut small = [0_u8; 8];
        assert_eq!(
            decoded.expand(&[], &mut small),
            Err(Error::CorruptData(Corruption::BlockSize))
        );

        let header = FrameHeader::new(
            ResourceClass::Small,
            RegionIndependence::Independent,
            IntegrityMode::Absent,
        );
        let region = RegionHeader::new(u64::MAX, u64::MAX, IntegrityMode::Absent)?;
        let mut bytes = header.encode().as_bytes().to_vec();
        bytes.extend_from_slice(Record::Region(region).encode().as_bytes());
        bytes.extend_from_slice(block.encode().as_bytes());
        assert_eq!(
            decode_frame(&bytes),
            Err(Error::LimitExceeded {
                declared: u64::from(MAX_BLOCK_BYTES),
                allowed: u64::from(BLOCK_LIMIT),
            })
        );
        Ok(())
    }

    #[test]
    fn a_region_header_carries_its_integrity_field_only_when_the_frame_declares_one()
    -> Result<(), Error> {
        let absent = RegionHeader::new(64, 64, IntegrityMode::Absent)?;
        let present = RegionHeader::new(64, 64, IntegrityMode::PerRegion)?;
        assert_eq!(absent.encoded_len(), REGION_HEADER_FIXED_BYTES);
        assert_eq!(
            present.encoded_len(),
            REGION_HEADER_FIXED_BYTES.saturating_add(INTEGRITY_FIELD_BYTES)
        );
        assert!(absent.integrity.is_none());
        assert_eq!(present.integrity, Some(0));

        let encoded = present.encode();
        let (decoded, used) = RegionHeader::decode(encoded.as_bytes(), IntegrityMode::PerRegion)?;
        assert_eq!(decoded, present);
        assert_eq!(used, present.encoded_len());
        Ok(())
    }

    #[test]
    fn every_structure_serializes_to_the_width_it_declares() -> Result<(), Error> {
        for header in every_legal_header() {
            assert_eq!(header.encode().len(), header.encoded_len());
        }
        for integrity in INTEGRITY {
            let region = RegionHeader::new(64, 64, integrity)?;
            assert_eq!(region.encode().len(), region.encoded_len());
            let record = Record::Region(region);
            assert_eq!(record.encode().len(), record.encoded_len());
        }
        assert_eq!(
            Record::Terminator.encode().len(),
            Record::Terminator.encoded_len()
        );
        let block = BlockHeader::raw(true, 1)?;
        assert_eq!(block.encode().len(), BLOCK_HEADER_BYTES);
        Ok(())
    }

    #[test]
    fn a_short_record_asks_for_the_whole_record_including_its_tag() -> Result<(), Error> {
        for integrity in INTEGRITY {
            let region = RegionHeader::new(64, 64, integrity)?;
            let record = Record::Region(region);
            let encoded = record.encode();
            for length in 0..encoded.len() {
                let short = encoded.as_bytes().get(..length).unwrap_or(&[]);
                let result = Record::decode(short, integrity);
                assert!(
                    matches!(result, Err(Error::TruncatedInput { needed }) if needed > length),
                    "length {length} gave {result:?}"
                );
            }
            let one_short = encoded
                .as_bytes()
                .get(..record.encoded_len().saturating_sub(1))
                .unwrap_or(&[]);
            assert_eq!(
                Record::decode(one_short, integrity),
                Err(Error::TruncatedInput {
                    needed: record.encoded_len()
                })
            );
        }
        Ok(())
    }

    #[test]
    fn the_fixed_frame_header_is_the_width_the_format_states() {
        let header = FrameHeader::new(
            ResourceClass::Minimal,
            RegionIndependence::Dependent,
            IntegrityMode::Absent,
        );
        assert_eq!(header.encoded_len(), FRAME_HEADER_FIXED_BYTES);
    }
}
