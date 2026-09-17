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

use codec::format::{DecoderPolicy, FrameHeader, IntegrityMode, RegionIndependence, ResourceClass};
use codec::stream::{Decoder, Encoder, Progress, StreamState};

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
/// The peak resident set at the largest size may exceed the peak at the smallest by no more
/// than the margin, no point may exceed the ceiling, and the allocation count and the peak
/// outstanding bytes must be identical at every size. A path that materialized the whole
/// input or the whole output would move the resident set by a gibibyte, which is two hundred
/// and fifty times the margin.
const RSS_MARGIN_BYTES: u64 = 4_194_304;
const RSS_CEILING_BYTES: u64 = 33_554_432;

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

/// Whether a curve holds the criterion, and what it was judged on.
pub struct Verdict {
    pub flat: bool,
    pub findings: Vec<String>,
}

/// Judges a curve against the criterion stated above.
#[must_use]
pub fn judge(points: &[Point]) -> Verdict {
    let mut findings = Vec::new();
    let Some(first) = points.first() else {
        findings.push(String::from("the curve holds no point"));
        return Verdict {
            flat: false,
            findings,
        };
    };

    for point in points {
        if point.allocations != first.allocations {
            findings.push(format!(
                "at {} bytes the run served {} allocations, against {} at {} bytes",
                point.logical_bytes, point.allocations, first.allocations, first.logical_bytes
            ));
        }
        if point.peak_outstanding_bytes != first.peak_outstanding_bytes {
            findings.push(format!(
                "at {} bytes the peak outstanding was {} bytes, against {} at {} bytes",
                point.logical_bytes,
                point.peak_outstanding_bytes,
                first.peak_outstanding_bytes,
                first.logical_bytes
            ));
        }
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
        if let Some(rss) = point.peak_rss_bytes {
            if rss > RSS_CEILING_BYTES {
                findings.push(format!(
                    "at {} bytes the peak resident set was {rss} bytes, above the {RSS_CEILING_BYTES} ceiling",
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

    Verdict {
        flat: findings.is_empty(),
        findings,
    }
}

/// Runs one child per size and reports the curve.
///
/// # Errors
///
/// Fails when a child cannot be started, or does not report a point.
pub fn run(program: &Path, sizes: &[u64]) -> Result<(Vec<Point>, Verdict)> {
    let mut points = Vec::new();
    for size in sizes {
        points.push(child(program, *size)?);
    }
    let verdict = judge(&points);
    Ok((points, verdict))
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
pub fn table(points: &[Point], verdict: &Verdict) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# The memory growth curve\n");
    let _ = writeln!(out, "Platform: {}", host::platform());
    let _ = writeln!(out, "Resident set: {}", host::PEAK_RSS_METHOD);
    let _ = writeln!(
        out,
        "Harness buffers: {HARNESS_BUFFERS} of {HARNESS_BUFFER_BYTES} bytes, fixed before the \
         run and inside the measured interval"
    );
    let _ = writeln!(
        out,
        "Criterion: the peak resident set at the largest size exceeds the smallest by at most \
         {RSS_MARGIN_BYTES} bytes, no point exceeds {RSS_CEILING_BYTES} bytes, and the \
         allocation count, the peak outstanding bytes, and the declared bound are identical \
         at every size.\n"
    );
    let _ = writeln!(
        out,
        "| logical MiB | stream bytes | encoder bound | decoder bound | allocations | \
         allocated bytes | peak outstanding | peak RSS | ms |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|");
    for point in points {
        let rss = point
            .peak_rss_bytes
            .map_or_else(|| String::from("absent"), |bytes| bytes.to_string());
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {rss} | {} |",
            point.logical_bytes.wrapping_shr(20),
            point.stream_bytes,
            point.encoder_bytes,
            point.decoder_bytes,
            point.allocations,
            point.allocated_bytes,
            point.peak_outstanding_bytes,
            point.elapsed_ms,
        );
    }
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

#[cfg(test)]
mod tests {
    use super::{Point, judge, line, parse, table};

    fn point(logical: u64, rss: u64) -> Point {
        Point {
            logical_bytes: logical,
            stream_bytes: logical.saturating_add(64),
            encoder_bytes: 1_048_612,
            decoder_bytes: 36,
            allocations: 5,
            allocated_bytes: 1_310_756,
            peak_outstanding_bytes: 1_310_756,
            peak_rss_bytes: Some(rss),
            elapsed_ms: 12,
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
        let verdict = judge(&points);
        assert!(verdict.flat, "{:?}", verdict.findings);
        assert!(table(&points, &verdict).contains("The curve is flat"));
    }

    #[test]
    fn a_resident_set_that_follows_the_input_is_not_flat() {
        let points = [
            point(1_048_576, 4_000_000),
            point(1_073_741_824, 1_080_000_000),
        ];
        let verdict = judge(&points);
        assert!(!verdict.flat);
        assert!(verdict.findings.iter().any(|f| f.contains("ceiling")));
    }

    #[test]
    fn an_allocation_count_that_moves_with_the_input_is_not_flat() {
        let mut grown = point(1_073_741_824, 4_100_000);
        grown.allocations = 20_000;
        let points = [point(1_048_576, 4_000_000), grown];
        assert!(!judge(&points).flat);
    }

    #[test]
    fn a_peak_outstanding_that_moves_with_the_input_is_not_flat() {
        let mut grown = point(1_073_741_824, 4_100_000);
        grown.peak_outstanding_bytes = 1_073_741_824;
        let points = [point(1_048_576, 4_000_000), grown];
        assert!(!judge(&points).flat);
    }

    #[test]
    fn a_host_that_reports_no_resident_set_is_a_finding_rather_than_a_flat_curve() {
        let mut blind = point(1_048_576, 0);
        blind.peak_rss_bytes = None;
        assert!(!judge(&[blind]).flat);
    }

    #[test]
    fn an_empty_curve_is_not_flat() {
        assert!(!judge(&[]).flat);
    }
}
