//! Owns the memory growth curve: what the streaming pair holds as the logical input grows by
//! three orders of magnitude, and whether that figure moves.
//!
//! Boundedness is proved with a curve rather than with one large input. A single run on a
//! gibibyte says the run finished; a flat curve from one mebibyte to one gibibyte says the
//! cost does not follow the input, which is the property the format claims.
//!
//! Each point runs in its own child process. The resident-set high-water mark is process wide
//! and monotonic, so six points measured in one process would report the largest of them six
//! times and prove nothing about the small ones.
//!
//! This module does not own the codec's bound. It measures what the bound costs.

use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use codec::entropy::MAX_BLOCK_TABLE_BYTES;
use codec::entropy::huffman::{LENGTH_LIMIT, table_bytes_for};
use codec::format::{DecoderPolicy, FrameHeader, IntegrityMode, RegionIndependence, ResourceClass};
use codec::sequence::WINDOW;
use codec::stream::{DEFAULT_BLOCK_BYTES, Decoder, Encoder, Progress, StreamState};

/// The decode table shape the literal alphabet is read through.
///
/// Stated beside the bound because the shape decides the figure a description declares. It is
/// provisional: the format fixes the declared figure, not the way a decoder reaches it.
const HUFFMAN_SHAPE: &str = "huffman-single-level, 2 << max_length bytes";

use crate::alloc;
use crate::content::Shape;
use crate::error::{Error, Result};
use crate::host;

/// The logical input sizes the curve covers, in bytes.
///
/// A decade and a half of input, doubling by fours, so a cost that grew with the input would
/// show as a slope rather than as a step that one size could hide.
pub const SIZES: [u64; 6] = [
    1_048_576,
    4_194_304,
    16_777_216,
    67_108_864,
    268_435_456,
    1_073_741_824,
];

/// The buffers the harness holds while it streams, in bytes.
///
/// Every one is fixed before the run and none is proportional to the logical input. They are
/// part of the measured interval and are stated rather than subtracted.
const HARNESS_BUFFER_BYTES: usize = 65_536;
const HARNESS_BUFFERS: usize = 4;

/// The criterion, stated before the run.
///
/// Five clauses, and the tool decides the verdict on all five.
///
/// ```text
/// 1  the declared bound of each direction is identical at every size, and is stated with
///    the window, the block length and the Huffman shape beside it
/// 2  the allocations one block costs are at most the ceiling, and the figure does not grow
///    with the input: the marginal cost between consecutive sizes stays inside the spread
/// 3  the peak outstanding bytes stay inside the declared bound plus the slack, and their
///    own spread across the whole range stays inside its margin
/// 4  the peak resident set at the largest size exceeds the smallest by at most the margin,
///    and no size exceeds the declared bound plus the resident slack
/// 5  a block whose declared peak table memory exceeds local policy is refused before the
///    allocation counter moves
/// ```
///
/// Clauses 2 and 3 are the bounded form, not an equality. Coding and reading one block
/// allocate in proportion to that block and free it before the call returns, which is the
/// path this revision ships; the equality is the removal of those allocations, and the
/// performance pass owns it. What a curve settles here is that neither figure follows the
/// input: a path that materialized the whole input or the whole output would move the
/// resident set by a gibibyte, which is two hundred and fifty times the margin, and a path
/// whose per-block cost grew with the input would move the marginal figure.
const RSS_MARGIN_BYTES: u64 = 4_194_304;
const RSS_SLACK_BYTES: u64 = 33_554_432;
const PEAK_OUTSTANDING_SLACK_BYTES: u64 = 1_048_576;
const PEAK_OUTSTANDING_MARGIN_BYTES: u64 = 65_536;
const ALLOCATIONS_PER_BLOCK_CEILING: u64 = 128;
const ALLOCATIONS_PER_BLOCK_SPREAD: u64 = 8;

/// What one point of the curve measured.
#[derive(Clone, Copy, Debug, Default)]
pub struct Point {
    pub logical_bytes: u64,
    pub stream_bytes: u64,
    pub encoder_bytes: u64,
    pub decoder_bytes: u64,
    pub allocations: u64,
    pub allocated_bytes: u64,
    pub peak_outstanding_bytes: u64,
    pub peak_rss_bytes: Option<u64>,
    pub elapsed_ms: u64,
}

/// Streams `logical_bytes` of generated content through both machines and reports what the
/// process held while it did.
///
/// Nothing here is proportional to the logical size. The content is produced in place, checked
/// in place, and never stored, so the only memory the run holds is the codec's declared state
/// and the harness buffers named above.
///
/// # Errors
///
/// Fails when the codec refuses a request, which the caller's parameters cannot cause.
pub fn point(logical_bytes: u64) -> Result<Point> {
    let shape = Shape::Mixed;
    let header = FrameHeader::new(
        ResourceClass::Small,
        RegionIndependence::Independent,
        IntegrityMode::PerRegion,
    );
    let policy = DecoderPolicy::with_max_history_bytes(ResourceClass::Huge.history_bytes());

    let opened = alloc::counts();
    alloc::restart_peak();
    let started = Instant::now();

    let mut encoder = Encoder::new(header)?;
    let mut decoder = Decoder::new(policy);
    let mut input = vec![0_u8; HARNESS_BUFFER_BYTES];
    let mut coded = vec![0_u8; HARNESS_BUFFER_BYTES];
    let mut plain = vec![0_u8; HARNESS_BUFFER_BYTES];
    let mut want = vec![0_u8; HARNESS_BUFFER_BYTES];

    let mut run = Run {
        shape,
        checked: 0,
        stream_bytes: 0,
    };
    let mut fed = 0_u64;
    while fed < logical_bytes {
        let span = usize::try_from(logical_bytes.saturating_sub(fed))
            .unwrap_or(HARNESS_BUFFER_BYTES)
            .min(HARNESS_BUFFER_BYTES);
        let chunk = input
            .get_mut(..span)
            .ok_or(codec::format::Error::InvalidParameter)?;
        shape.fill(fed, chunk);
        let mut sent = 0_usize;
        while sent < span {
            let rest = input
                .get(sent..span)
                .ok_or(codec::format::Error::InvalidParameter)?;
            let progress = encoder.encode(rest, &mut coded)?;
            sent = sent.saturating_add(progress.consumed);
            run.drain(&mut decoder, &coded, progress, &mut plain, &mut want)?;
        }
        fed = fed.saturating_add(u64::try_from(span).unwrap_or(0));
    }
    loop {
        let progress = encoder.finish(&mut coded)?;
        run.drain(&mut decoder, &coded, progress, &mut plain, &mut want)?;
        if progress.state == StreamState::Finished {
            break;
        }
    }
    run.drain(
        &mut decoder,
        &coded,
        Progress {
            consumed: 0,
            produced: 0,
            state: StreamState::NeedsInput,
        },
        &mut plain,
        &mut want,
    )?;
    decoder.finish()?;

    let elapsed = started.elapsed();
    let interval = opened.until(alloc::counts());
    let encoder_bytes = u64::try_from(encoder.steady_state_bytes()).unwrap_or(0);
    let decoder_bytes = u64::try_from(decoder.steady_state_bytes()).unwrap_or(0);

    if run.checked != logical_bytes {
        return Err(Error::child(format!(
            "the decoder produced {} bytes for {logical_bytes} of input",
            run.checked
        )));
    }
    Ok(Point {
        logical_bytes,
        stream_bytes: run.stream_bytes,
        encoder_bytes,
        decoder_bytes,
        allocations: interval.allocations,
        allocated_bytes: interval.allocated_bytes,
        peak_outstanding_bytes: interval.peak_outstanding_bytes,
        peak_rss_bytes: host::peak_rss_bytes(),
        elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
    })
}

/// The decoder side of one point, which checks what the encoder produced as it arrives.
struct Run {
    shape: Shape,
    checked: u64,
    stream_bytes: u64,
}

impl Run {
    fn drain(
        &mut self,
        decoder: &mut Decoder,
        coded: &[u8],
        progress: Progress,
        plain: &mut [u8],
        want: &mut [u8],
    ) -> Result<()> {
        self.stream_bytes = self
            .stream_bytes
            .saturating_add(u64::try_from(progress.produced).unwrap_or(0));
        let mut rest = coded
            .get(..progress.produced)
            .ok_or(codec::format::Error::InvalidParameter)?;
        loop {
            let moved = decoder.decode(rest, plain)?;
            rest = rest
                .get(moved.consumed..)
                .ok_or(codec::format::Error::InvalidParameter)?;
            if moved.produced > 0 {
                let got = plain
                    .get(..moved.produced)
                    .ok_or(codec::format::Error::InvalidParameter)?;
                let expected = want
                    .get_mut(..moved.produced)
                    .ok_or(codec::format::Error::InvalidParameter)?;
                self.shape.fill(self.checked, expected);
                if got != expected {
                    return Err(Error::child(format!(
                        "the stream did not round trip at byte {}",
                        self.checked
                    )));
                }
                self.checked = self
                    .checked
                    .saturating_add(u64::try_from(moved.produced).unwrap_or(0));
            }
            if rest.is_empty() && moved.produced == 0 {
                return Ok(());
            }
        }
    }
}

/// The content the table refusal is measured on, in bytes.
///
/// Small enough to be one region and one block, so the peak table memory a decoder holds is
/// the figure that one block declares. A policy one byte below it refuses that block and
/// nothing before it.
const REFUSAL_BYTES: usize = 8_192;

/// What the table-memory refusal measured.
///
/// A bound a decoder enforces after it has committed memory is not a bound. The figure that
/// makes the position measurable is the allocation counter across the refusing call: a block
/// refused at the declaration leaves it where it was.
#[derive(Clone, Copy, Debug, Default)]
pub struct Refusal {
    /// The peak table memory the block declares, read under a policy that admits it.
    pub declared: u64,
    /// The table memory the policy under test admits, which is one byte below the
    /// declaration.
    pub allowed: u64,
    /// Whether the decoder refused with `LimitExceeded`.
    pub refused: bool,
    /// The allocations the refusing call served.
    pub allocations: u64,
    /// The decode tables the refusing decoder built.
    pub tables_built: u64,
}

impl Refusal {
    /// What this measurement found against the criterion, and nothing when it held.
    fn findings(&self) -> Vec<String> {
        let mut findings = Vec::new();
        if self.declared == 0 {
            findings.push(String::from(
                "the stream the refusal is measured on declares no table memory, so no policy \
                 could refuse it and the criterion was not exercised",
            ));
        }
        if !self.refused {
            findings.push(format!(
                "a block declaring {} table bytes was not refused by a policy admitting {}",
                self.declared, self.allowed
            ));
        }
        if self.allocations > 0 {
            findings.push(format!(
                "the refusing call served {} allocations, so the refusal did not precede them",
                self.allocations
            ));
        }
        if self.tables_built > 0 {
            findings.push(format!(
                "the refusing decoder built {} decode tables, so it committed before it \
                 refused",
                self.tables_built
            ));
        }
        findings
    }
}

/// The policy that admits whatever a frame this tool writes declares.
const fn permissive() -> DecoderPolicy {
    DecoderPolicy::with_max_history_bytes(ResourceClass::Huge.history_bytes())
}

/// Encodes `content` as one frame, in one call each way.
fn whole(content: &[u8]) -> Result<Vec<u8>> {
    let header = FrameHeader::new(
        ResourceClass::Small,
        RegionIndependence::Independent,
        IntegrityMode::Absent,
    );
    let mut encoder = Encoder::new(header)?;
    let mut stream = vec![0_u8; content.len().saturating_mul(2).saturating_add(4_096)];
    let mut at = 0_usize;
    let mut fed = 0_usize;
    while fed < content.len() {
        let rest = content
            .get(fed..)
            .ok_or(codec::format::Error::InvalidParameter)?;
        let room = stream
            .get_mut(at..)
            .ok_or(codec::format::Error::InvalidParameter)?;
        let progress = encoder.encode(rest, room)?;
        fed = fed.saturating_add(progress.consumed);
        at = at.saturating_add(progress.produced);
    }
    loop {
        let room = stream
            .get_mut(at..)
            .ok_or(codec::format::Error::InvalidParameter)?;
        let progress = encoder.finish(room)?;
        at = at.saturating_add(progress.produced);
        if progress.state == StreamState::Finished {
            break;
        }
    }
    stream.truncate(at);
    Ok(stream)
}

/// Measures whether a block above the table ceiling is refused before anything is allocated.
///
/// The allocation counter is process wide, so the figure is what the process served during the
/// refusing call and not what that call served. The command runs alone in its own process, so
/// the two are the same there. Under a test harness that runs tests on several threads they
/// are not, and the test beside this function asserts only what a concurrent thread cannot
/// move.
///
/// # Errors
///
/// Fails when the stream cannot be built, or when a decoder that should accept it does not.
pub fn refusal() -> Result<Refusal> {
    let mut content = vec![0_u8; REFUSAL_BYTES];
    Shape::TextLike.fill(0, &mut content);
    let stream = whole(&content)?;
    let mut out = vec![0_u8; REFUSAL_BYTES.saturating_add(4_096)];

    // What the block declares. It is read rather than assumed, so the policy under test sits
    // one byte below a figure this revision's encoder actually wrote.
    let mut admitting = Decoder::new(permissive());
    drain(&mut admitting, &stream, &mut out)?;
    let declared = admitting.peak_table_bytes();

    // The policy admits no table memory at all, so the block is refused against the figure it
    // declares in its prologue rather than against the figure its descriptions build to. That
    // is step 3 of the read order, which is the position this clause is about: a policy that
    // refused one step later would already have parsed four descriptions, and parsing one
    // allocates even though it builds no table.
    let allowed = 0;
    let mut refusing = Decoder::new(permissive().with_max_table_bytes(allowed));
    let opened = alloc::counts();
    let outcome = refusing.decode(&stream, &mut out);
    let interval = opened.until(alloc::counts());
    Ok(Refusal {
        declared,
        allowed,
        refused: matches!(outcome, Err(codec::format::Error::LimitExceeded { .. })),
        allocations: interval.allocations,
        tables_built: refusing.tables_built(),
    })
}

/// Reads a whole stream, which a decoder that admits it reaches the end of.
fn drain(decoder: &mut Decoder, stream: &[u8], out: &mut [u8]) -> Result<()> {
    let mut at = 0_usize;
    loop {
        let rest = stream.get(at..).unwrap_or_default();
        let progress = decoder.decode(rest, out)?;
        at = at.saturating_add(progress.consumed);
        if progress.state == StreamState::Finished {
            break;
        }
        if progress.consumed == 0 && progress.produced == 0 {
            break;
        }
    }
    decoder.finish()?;
    Ok(())
}

/// Whether a curve holds the criterion, and what it was judged on.
pub struct Verdict {
    pub flat: bool,
    pub findings: Vec<String>,
}

/// The blocks a run of `logical_bytes` cuts its input into.
///
/// The per-block figures are marginal costs, so they need the count the cost is spread over.
/// The layout is the encoder's default, which is what `point` drives.
const fn blocks(logical_bytes: u64) -> u64 {
    logical_bytes.div_ceil(DEFAULT_BLOCK_BYTES as u64)
}

/// The allocations one block cost, between two sizes.
///
/// Marginal rather than average, so the allocations a run makes once at construction are not
/// charged to the blocks of the smallest size and read as a slope.
fn marginal_allocations(from: &Point, to: &Point) -> Option<u64> {
    let span = blocks(to.logical_bytes).checked_sub(blocks(from.logical_bytes))?;
    let spent = to.allocations.checked_sub(from.allocations)?;
    spent.checked_div(span)
}

/// Judges a curve against the criterion stated above.
#[must_use]
pub fn judge(points: &[Point], refusal: &Refusal) -> Verdict {
    let mut findings = Vec::new();
    let Some(first) = points.first() else {
        findings.push(String::from("the curve holds no point"));
        return Verdict {
            flat: false,
            findings,
        };
    };
    let bound = first.encoder_bytes.saturating_add(first.decoder_bytes);

    for point in points {
        if point.encoder_bytes != first.encoder_bytes || point.decoder_bytes != first.decoder_bytes
        {
            findings.push(format!(
                "at {} bytes the declared bound was {} and {}, against {} and {}",
                point.logical_bytes,
                point.encoder_bytes,
                point.decoder_bytes,
                first.encoder_bytes,
                first.decoder_bytes
            ));
        }
        let outstanding_ceiling = bound.saturating_add(PEAK_OUTSTANDING_SLACK_BYTES);
        if point.peak_outstanding_bytes > outstanding_ceiling {
            findings.push(format!(
                "at {} bytes the peak outstanding was {} bytes, above the {outstanding_ceiling} \
                 the declared bound and its slack allow",
                point.logical_bytes, point.peak_outstanding_bytes
            ));
        }
        if let Some(rss) = point.peak_rss_bytes {
            let rss_ceiling = bound.saturating_add(RSS_SLACK_BYTES);
            if rss > rss_ceiling {
                findings.push(format!(
                    "at {} bytes the peak resident set was {rss} bytes, above the \
                     {rss_ceiling} the declared bound and its slack allow",
                    point.logical_bytes
                ));
            }
            if let Some(base) = first.peak_rss_bytes
                && rss.saturating_sub(base) > RSS_MARGIN_BYTES
            {
                findings.push(format!(
                    "at {} bytes the peak resident set was {rss} bytes, {} above the {base} measured at {} bytes",
                    point.logical_bytes,
                    rss.saturating_sub(base),
                    first.logical_bytes
                ));
            }
        } else {
            findings.push(format!(
                "at {} bytes the host refused to report a resident set",
                point.logical_bytes
            ));
        }
    }

    let spread = outstanding_spread(points);
    if spread > PEAK_OUTSTANDING_MARGIN_BYTES {
        findings.push(format!(
            "the peak outstanding moved by {spread} bytes across the range, above the \
             {PEAK_OUTSTANDING_MARGIN_BYTES} margin"
        ));
    }

    let marginals = marginals(points);
    for (at, cost) in &marginals {
        if *cost > ALLOCATIONS_PER_BLOCK_CEILING {
            findings.push(format!(
                "reaching {at} bytes cost {cost} allocations per block, above the \
                 {ALLOCATIONS_PER_BLOCK_CEILING} ceiling"
            ));
        }
    }
    let costs: Vec<u64> = marginals.iter().map(|(_, cost)| *cost).collect();
    let widest = costs
        .iter()
        .max()
        .copied()
        .unwrap_or(0)
        .saturating_sub(costs.iter().min().copied().unwrap_or(0));
    if widest > ALLOCATIONS_PER_BLOCK_SPREAD {
        findings.push(format!(
            "the allocations one block cost moved by {widest} across the range, above the \
             {ALLOCATIONS_PER_BLOCK_SPREAD} spread"
        ));
    }

    findings.extend(refusal.findings());

    Verdict {
        flat: findings.is_empty(),
        findings,
    }
}

/// The distance from the smallest peak-outstanding figure to the largest.
fn outstanding_spread(points: &[Point]) -> u64 {
    let widest = points
        .iter()
        .map(|point| point.peak_outstanding_bytes)
        .max()
        .unwrap_or(0);
    let narrowest = points
        .iter()
        .map(|point| point.peak_outstanding_bytes)
        .min()
        .unwrap_or(0);
    widest.saturating_sub(narrowest)
}

/// The allocations one block cost, for each step of the curve.
///
/// A curve of one point has no step, so its per-block figure is the average instead. The
/// ceiling still binds it; the spread has nothing to compare.
fn marginals(points: &[Point]) -> Vec<(u64, u64)> {
    let mut costs = Vec::new();
    for pair in points.windows(2) {
        if let (Some(from), Some(to)) = (pair.first(), pair.get(1))
            && let Some(cost) = marginal_allocations(from, to)
        {
            costs.push((to.logical_bytes, cost));
        }
    }
    if costs.is_empty()
        && let Some(only) = points.first()
        && let Some(cost) = only
            .allocations
            .checked_div(blocks(only.logical_bytes).max(1))
    {
        costs.push((only.logical_bytes, cost));
    }
    costs
}

/// Runs one child per size, checks the table refusal, and reports the curve.
///
/// The refusal is measured in this process rather than in a child. It is a property of one
/// call and not of a run, so it needs no process of its own, and the counter it turns on is
/// the same one every child reads.
///
/// # Errors
///
/// Fails when a child cannot be started, does not report a point, or when the stream the
/// refusal is measured on cannot be built.
pub fn run(program: &Path, sizes: &[u64]) -> Result<(Vec<Point>, Refusal, Verdict)> {
    let mut points = Vec::new();
    for size in sizes {
        points.push(child(program, *size)?);
    }
    let refusal = refusal()?;
    let verdict = judge(&points, &refusal);
    Ok((points, refusal, verdict))
}

fn child(program: &Path, bytes: u64) -> Result<Point> {
    let output = Command::new(program)
        .args(["curve-point", "--bytes", &bytes.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| Error::at("run", program, e))?;
    if !output.status.success() {
        return Err(Error::child(format!(
            "the point at {bytes} bytes exited {}",
            output
                .status
                .code()
                .map_or_else(|| String::from("on a signal"), |code| code.to_string())
        )));
    }
    let line = String::from_utf8_lossy(&output.stdout);
    parse(&line).ok_or_else(|| {
        Error::child(format!(
            "the point at {bytes} bytes reported `{}`, which is not a point",
            line.trim()
        ))
    })
}

/// The one line a child prints, in the shape the parent reads it.
#[must_use]
pub fn line(point: &Point) -> String {
    let rss = point
        .peak_rss_bytes
        .map_or_else(|| String::from("absent"), |bytes| bytes.to_string());
    format!(
        "logical_bytes={} stream_bytes={} encoder_bytes={} decoder_bytes={} \
         allocations={} allocated_bytes={} peak_outstanding_bytes={} \
         peak_rss_bytes={rss} elapsed_ms={}\n",
        point.logical_bytes,
        point.stream_bytes,
        point.encoder_bytes,
        point.decoder_bytes,
        point.allocations,
        point.allocated_bytes,
        point.peak_outstanding_bytes,
        point.elapsed_ms,
    )
}

fn parse(line: &str) -> Option<Point> {
    let mut point = Point::default();
    let mut seen = 0_u32;
    for field in line.split_whitespace() {
        let (key, value) = field.split_once('=')?;
        seen = seen.saturating_add(1);
        match key {
            "logical_bytes" => point.logical_bytes = value.parse().ok()?,
            "stream_bytes" => point.stream_bytes = value.parse().ok()?,
            "encoder_bytes" => point.encoder_bytes = value.parse().ok()?,
            "decoder_bytes" => point.decoder_bytes = value.parse().ok()?,
            "allocations" => point.allocations = value.parse().ok()?,
            "allocated_bytes" => point.allocated_bytes = value.parse().ok()?,
            "peak_outstanding_bytes" => point.peak_outstanding_bytes = value.parse().ok()?,
            "peak_rss_bytes" => {
                point.peak_rss_bytes = if value == "absent" {
                    None
                } else {
                    Some(value.parse().ok()?)
                };
            }
            "elapsed_ms" => point.elapsed_ms = value.parse().ok()?,
            _ => return None,
        }
    }
    (seen == 9).then_some(point)
}

/// The curve as a table, with the criterion it was judged against.
#[must_use]
pub fn table(points: &[Point], refusal: &Refusal, verdict: &Verdict) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# The memory growth curve\n");
    let _ = writeln!(out, "Platform: {}", host::platform());
    let _ = writeln!(out, "Resident set: {}", host::PEAK_RSS_METHOD);
    let _ = writeln!(
        out,
        "Harness buffers: {HARNESS_BUFFERS} of {HARNESS_BUFFER_BYTES} bytes, fixed before the \
         run and inside the measured interval"
    );
    out.push_str(&bound(points));
    out.push_str(&criterion());
    out.push_str(&rows(points));
    out.push_str(&refused(refusal));

    let _ = writeln!(out);
    if verdict.flat {
        let _ = writeln!(out, "The curve is flat. Every criterion above holds.");
    } else {
        let _ = writeln!(out, "The curve is not flat:");
        for finding in &verdict.findings {
            let _ = writeln!(out, "* {finding}");
        }
    }
    out
}

/// The declared bound of each direction, with what decides it beside it.
///
/// A bound with no configuration on the row is a number nobody can reproduce, so the window,
/// the block length and the decode table shape sit on it.
fn bound(points: &[Point]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\n## The declared bound, per direction\n");
    let _ = writeln!(
        out,
        "| direction | declared bytes | window | block length | Huffman shape |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|");
    let first = points.first().copied().unwrap_or_default();
    for (direction, declared) in [
        ("encode", first.encoder_bytes),
        ("decode", first.decoder_bytes),
    ] {
        let _ = writeln!(
            out,
            "| {direction} | {declared} | {WINDOW} | {DEFAULT_BLOCK_BYTES} | {HUFFMAN_SHAPE} |"
        );
    }
    let _ = writeln!(
        out,
        "\nThe shape's table is {} bytes at the {} length limit, and one block declares at \
         most {MAX_BLOCK_TABLE_BYTES} bytes over every table it decodes under.",
        table_bytes_for(LENGTH_LIMIT),
        LENGTH_LIMIT
    );
    out
}

/// The criterion, written into the report before the numbers it judges.
fn criterion() -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\n## The criterion, stated before the run\n");
    let _ = writeln!(
        out,
        "```text\n\
         1  the declared bound of each direction is identical at every size, and is stated\n\
         \x20  above with the window, the block length and the Huffman shape beside it\n\
         2  the allocations one block costs are at most {ALLOCATIONS_PER_BLOCK_CEILING}, and the\n\
         \x20  marginal figure moves by at most {ALLOCATIONS_PER_BLOCK_SPREAD} across the range\n\
         3  the peak outstanding bytes stay within the declared bound plus\n\
         \x20  {PEAK_OUTSTANDING_SLACK_BYTES} bytes, and their own spread stays within\n\
         \x20  {PEAK_OUTSTANDING_MARGIN_BYTES} bytes\n\
         4  the peak resident set at the largest size exceeds the smallest by at most\n\
         \x20  {RSS_MARGIN_BYTES} bytes, and no size exceeds the declared bound plus\n\
         \x20  {RSS_SLACK_BYTES} bytes\n\
         5  a block whose declared peak table memory exceeds local policy is refused before\n\
         \x20  the allocation counter moves\n\
         ```"
    );
    let _ = writeln!(
        out,
        "\nClauses 2 and 3 are bounded figures and not equalities. Coding and reading one \
         block allocate in proportion to that block and free it before the call returns, \
         which is the path this revision ships."
    );
    out
}

/// One row per size, with the allocations one block cost between this size and the last.
fn rows(points: &[Point]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\n## The curve\n");
    let _ = writeln!(
        out,
        "| logical MiB | blocks | stream bytes | encoder bound | decoder bound | allocations | \
         per block | allocated bytes | peak outstanding | peak RSS | ms |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|---|---|");
    let mut previous: Option<&Point> = None;
    for point in points {
        let rss = point
            .peak_rss_bytes
            .map_or_else(|| String::from("absent"), |bytes| bytes.to_string());
        let per_block = previous
            .and_then(|from| marginal_allocations(from, point))
            .map_or_else(|| String::from("-"), |cost| cost.to_string());
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {per_block} | {} | {} | {rss} | {} |",
            point.logical_bytes.wrapping_shr(20),
            blocks(point.logical_bytes),
            point.stream_bytes,
            point.encoder_bytes,
            point.decoder_bytes,
            point.allocations,
            point.allocated_bytes,
            point.peak_outstanding_bytes,
            point.elapsed_ms,
        );
        previous = Some(point);
    }
    out
}

/// What the table-memory refusal found, in the shape a reader checks clause 5 against.
fn refused(refusal: &Refusal) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\n## The table refusal\n");
    let _ = writeln!(
        out,
        "A block holding {} table bytes, read under a policy admitting {}: {}, with {} \
         allocations served and {} decode tables built by the refusing decoder.",
        refusal.declared,
        refusal.allowed,
        if refusal.refused {
            "refused as LimitExceeded"
        } else {
            "not refused"
        },
        refusal.allocations,
        refusal.tables_built,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::{Point, Refusal, blocks, judge, line, parse, table};

    /// A point of a curve whose per-block cost is one allocation.
    ///
    /// The allocation count is a function of the block count, so a pair of these points has a
    /// marginal figure the criterion can judge rather than a constant the criterion could
    /// not distinguish from a path that allocates nothing.
    fn point(logical: u64, rss: u64) -> Point {
        Point {
            logical_bytes: logical,
            stream_bytes: logical.saturating_add(64),
            encoder_bytes: 1_048_612,
            decoder_bytes: 36,
            allocations: blocks(logical).saturating_add(5),
            allocated_bytes: 1_310_756,
            peak_outstanding_bytes: 1_310_756,
            peak_rss_bytes: Some(rss),
            elapsed_ms: 12,
        }
    }

    /// A refusal that held, so a curve test judges the curve and not this clause.
    const fn held() -> Refusal {
        Refusal {
            declared: 4_096,
            allowed: 4_095,
            refused: true,
            allocations: 0,
            tables_built: 0,
        }
    }

    #[test]
    fn a_point_survives_the_line_the_child_prints_it_on() {
        let original = point(1_048_576, 4_000_000);
        let read = parse(&line(&original));
        let read = read.unwrap_or_default();
        assert_eq!(read.logical_bytes, original.logical_bytes);
        assert_eq!(read.allocations, original.allocations);
        assert_eq!(read.peak_outstanding_bytes, original.peak_outstanding_bytes);
        assert_eq!(read.peak_rss_bytes, original.peak_rss_bytes);
    }

    #[test]
    fn a_host_that_refuses_a_resident_set_survives_the_line() {
        let mut original = point(1_048_576, 0);
        original.peak_rss_bytes = None;
        assert_eq!(
            parse(&line(&original)).unwrap_or_default().peak_rss_bytes,
            None
        );
    }

    #[test]
    fn a_line_with_a_field_the_parser_does_not_know_is_not_a_point() {
        assert!(parse("logical_bytes=1 depth=2").is_none());
        assert!(parse("").is_none());
        assert!(parse("logical_bytes=1").is_none());
    }

    #[test]
    fn a_flat_curve_holds_the_criterion() {
        let points = [point(1_048_576, 4_000_000), point(1_073_741_824, 4_200_000)];
        let verdict = judge(&points, &held());
        assert!(verdict.flat, "{:?}", verdict.findings);
        let report = table(&points, &held(), &verdict);
        assert!(report.contains("The curve is flat"), "{report}");
        assert!(report.contains("refused as LimitExceeded"), "{report}");
    }

    #[test]
    fn the_report_states_the_declared_bound_of_each_direction() {
        let points = [point(1_048_576, 4_000_000)];
        let report = table(&points, &held(), &judge(&points, &held()));
        assert!(report.contains("| encode | 1048612 |"), "{report}");
        assert!(report.contains("| decode | 36 |"), "{report}");
        assert!(report.contains("huffman-single-level"), "{report}");
    }

    #[test]
    fn a_resident_set_that_follows_the_input_is_not_flat() {
        let points = [
            point(1_048_576, 4_000_000),
            point(1_073_741_824, 1_080_000_000),
        ];
        let verdict = judge(&points, &held());
        assert!(!verdict.flat);
        assert!(verdict.findings.iter().any(|f| f.contains("resident set")));
    }

    #[test]
    fn a_per_block_cost_above_the_ceiling_is_not_flat() {
        let mut grown = point(1_073_741_824, 4_100_000);
        grown.allocations = blocks(grown.logical_bytes).saturating_mul(1_000);
        let points = [point(1_048_576, 4_000_000), grown];
        let verdict = judge(&points, &held());
        assert!(!verdict.flat);
        assert!(
            verdict.findings.iter().any(|f| f.contains("per block")),
            "{:?}",
            verdict.findings
        );
    }

    #[test]
    fn a_per_block_cost_that_grows_with_the_input_is_not_flat() {
        // Three points whose per-block cost rises from 1 to 40 across the range. Each is
        // under the ceiling, so only the spread clause catches it.
        let mut middle = point(16_777_216, 4_050_000);
        middle.allocations = blocks(middle.logical_bytes).saturating_add(5);
        let mut last = point(1_073_741_824, 4_100_000);
        last.allocations = middle
            .allocations
            .saturating_add(blocks(last.logical_bytes).saturating_mul(40));
        let points = [point(1_048_576, 4_000_000), middle, last];
        let verdict = judge(&points, &held());
        assert!(!verdict.flat);
        assert!(
            verdict.findings.iter().any(|f| f.contains("spread")),
            "{:?}",
            verdict.findings
        );
    }

    #[test]
    fn a_peak_outstanding_that_follows_the_input_is_not_flat() {
        let mut grown = point(1_073_741_824, 4_100_000);
        grown.peak_outstanding_bytes = 1_073_741_824;
        let points = [point(1_048_576, 4_000_000), grown];
        assert!(!judge(&points, &held()).flat);
    }

    #[test]
    fn a_peak_outstanding_that_drifts_beyond_its_margin_is_not_flat() {
        let mut drifted = point(1_073_741_824, 4_100_000);
        drifted.peak_outstanding_bytes = drifted.peak_outstanding_bytes.saturating_add(65_537);
        let points = [point(1_048_576, 4_000_000), drifted];
        let verdict = judge(&points, &held());
        assert!(!verdict.flat);
        assert!(
            verdict
                .findings
                .iter()
                .any(|f| f.contains("peak outstanding moved")),
            "{:?}",
            verdict.findings
        );
    }

    #[test]
    fn a_host_that_reports_no_resident_set_is_a_finding_rather_than_a_flat_curve() {
        let mut blind = point(1_048_576, 0);
        blind.peak_rss_bytes = None;
        assert!(!judge(&[blind], &held()).flat);
    }

    #[test]
    fn an_empty_curve_is_not_flat() {
        assert!(!judge(&[], &held()).flat);
    }

    #[test]
    fn a_block_above_policy_that_is_not_refused_is_a_finding() {
        let mut admitted = held();
        admitted.refused = false;
        let points = [point(1_048_576, 4_000_000)];
        let verdict = judge(&points, &admitted);
        assert!(!verdict.flat);
        assert!(
            verdict.findings.iter().any(|f| f.contains("not refused")),
            "{:?}",
            verdict.findings
        );
    }

    #[test]
    fn a_refusal_that_allocated_first_is_a_finding() {
        let mut late = held();
        late.allocations = 1;
        let points = [point(1_048_576, 4_000_000)];
        let verdict = judge(&points, &late);
        assert!(!verdict.flat);
        assert!(
            verdict
                .findings
                .iter()
                .any(|f| f.contains("did not precede")),
            "{:?}",
            verdict.findings
        );
    }

    #[test]
    fn a_refusal_that_built_a_table_first_is_a_finding() {
        let mut committed = held();
        committed.tables_built = 1;
        let points = [point(1_048_576, 4_000_000)];
        assert!(!judge(&points, &committed).flat);
    }

    #[test]
    fn a_stream_that_declares_no_table_memory_does_not_exercise_the_clause() {
        let empty = Refusal::default();
        let points = [point(1_048_576, 4_000_000)];
        let verdict = judge(&points, &empty);
        assert!(!verdict.flat);
        assert!(
            verdict
                .findings
                .iter()
                .any(|f| f.contains("was not exercised")),
            "{:?}",
            verdict.findings
        );
    }

    #[test]
    fn the_refusal_this_revision_writes_is_refused_without_committing() {
        let Ok(measured) = super::refusal() else {
            unreachable!("the tool builds the stream it measures the refusal on")
        };
        assert!(measured.declared > 0, "{measured:?}");
        assert!(measured.refused, "{measured:?}");
        // The allocation count is a process-wide counter and this harness runs tests on
        // several threads, so it is the command that judges the position. What holds here
        // whatever else the process is doing is that the refusing decoder committed no table.
        assert_eq!(measured.tables_built, 0, "{measured:?}");
    }
}
