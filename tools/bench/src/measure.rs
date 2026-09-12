//! Owns the measurement: which competitor is driven over which entries, what is timed, and
//! what each metric comes out to.
//!
//! Every number here is produced in this process, through the competitor's own library. A
//! metric that a library, a host, or a codec path cannot produce is recorded as unavailable
//! with its reason, and the reason names what would have to change.
//!
//! Three costs are paid once per size class rather than once per entry: the streamed pass,
//! the cold pass that counts allocations, and the threaded pass. Each of them repeats the
//! whole compression, and each reports a property of the competitor rather than of the
//! entry, so paying them per entry would multiply the segment cost for nothing. The entry
//! they are measured on is the largest one the class selected, and every other row says so
//! instead of leaving the field blank.
//!
//! This module does not own the tier policy, the result document, or the laboratory.

use std::time::Instant;

use crate::catalog::Codec;
use crate::clock::{self, Samples};
use crate::competitor::{Method, Session};
use crate::corpus;
use crate::environment::{self, Environment};
use crate::error::{Error, Result};
use crate::lab;
use crate::laboratory::{self, Check};
use crate::metric::{Metric, Set};
use crate::plan::{self, Request, STREAM_CHUNK, Selection, Tier};
use crate::registry::{ENTRIES, Entry, SizeClass};
use crate::{alloc, counters};

/// What one segment already knows before it measures a row.
struct Context<'a> {
    request: &'a Request,
    selection: &'a Selection,
    environment: &'a Environment,
}

/// One measured row: one competitor, at one operating point, over one entry.
pub struct Measurement {
    pub codec: &'static str,
    pub display_name: &'static str,
    pub version: &'static str,
    pub operating_point: String,
    pub format: &'static str,
    pub integrity: &'static str,
    pub entry: &'static str,
    pub group: &'static str,
    pub class: &'static str,
    pub input_bytes: u64,
    pub input_digest: &'static str,
    pub threads: u32,
    pub round_trip_verified: bool,
    pub encode: Option<Sampling>,
    pub decode: Option<Sampling>,
    pub metrics: Set,
}

/// What a set of samples came to, and whether the tier accepts its spread.
pub struct Sampling {
    pub samples: usize,
    pub repetitions: u32,
    pub min_ns: u64,
    pub median_ns: u64,
    pub p95_ns: u64,
    pub max_ns: u64,
    pub spread: f64,
    pub accepted_spread: Option<f64>,
    pub spread_accepted: Option<bool>,
}

/// Everything one segment produced.
pub struct Outcome {
    pub environment: Environment,
    pub laboratory: Vec<Check>,
    pub linked_version: Option<String>,
    pub selection: Selection,
    pub measurements: Vec<Measurement>,
}

/// Runs one segment.
///
/// # Errors
///
/// Fails when the laboratory is not linked, a corpus entry is not the one the registry
/// describes, or a competitor library refuses the work it was given.
pub fn run(request: &Request) -> Result<Outcome> {
    if !laboratory::is_linked() {
        return Err(Error::measure(
            "the benchmark harness",
            laboratory::NOT_LINKED,
        ));
    }
    let lab_layout = lab::Layout::resolve(request.lab.clone())?;
    let corpus_layout = corpus::Layout::resolve(request.corpus.clone())?;
    let laboratory = laboratory::confirm(&laboratory::linked(), &lab_layout)?;
    let environment = environment::capture();

    let classes: Vec<SizeClass> = request
        .class
        .map_or_else(|| request.tier.classes().to_vec(), |class| vec![class]);
    let selection = plan::select(request.tier, &classes, ENTRIES);
    let points = plan::points(request);

    let mut linked_version = None;
    let mut measurements = Vec::new();
    for entry in &selection.selected {
        let data = corpus::load(entry, &corpus_layout)?;
        for point in &points {
            let session = Session::open(request.codec, point)?;
            if linked_version.is_none() {
                linked_version = session.linked_version();
            }
            let context = Context {
                request,
                selection: &selection,
                environment: &environment,
            };
            measurements.push(measure(&context, entry, point, &data, session)?);
        }
    }

    Ok(Outcome {
        environment,
        laboratory,
        linked_version,
        selection,
        measurements,
    })
}

/// Whether this entry is the one its class pays the per-class costs on.
///
/// The largest selected entry of a class is the one whose streamed pass produces the most
/// chunks and whose cold pass exercises the most of the codec's working set.
fn is_representative(entry: &Entry, selection: &Selection) -> bool {
    selection
        .selected
        .iter()
        .filter(|other| other.class() == entry.class())
        .max_by_key(|other| (other.bytes, other.name))
        .is_some_and(|largest| largest.name == entry.name)
}

#[allow(clippy::too_many_lines)]
fn measure(
    context: &Context<'_>,
    entry: &'static Entry,
    point: &str,
    data: &[u8],
    mut session: Session,
) -> Result<Measurement> {
    let request = context.request;
    let environment = context.environment;
    let representative = is_representative(entry, context.selection);
    let tier = request.tier;
    let notes = session.notes();
    let mut metrics = Set::new();

    let bound = session.compress_bound(data.len()).max(64);
    let mut compressed = vec![0_u8; bound];
    let mut restored = vec![0_u8; data.len().max(1)];

    // One warm pass before timing: it pays the first-touch cost of both buffers and gives
    // the batch calibration a cost to size itself from.
    let (first, first_ns) = clock::time(|| session.compress(data, &mut compressed));
    let compressed_bytes = first?;

    let encode_repetitions = batch(entry.class(), first_ns);
    let encode = sample(tier, encode_repetitions, || {
        session.compress(data, &mut compressed)
    })?;

    let payload = compressed
        .get(..compressed_bytes)
        .ok_or_else(|| Error::measure("the compressed output", "is shorter than reported"))?
        .to_vec();
    // A decode is cheaper than an encode at every operating point, so it calibrates its
    // own batch. Reusing the encode's repetition count leaves a tiny-class decode below the
    // clock's resolution, which is the error batching exists to remove.
    let (decoded, decode_ns) = clock::time(|| session.decompress(&payload, &mut restored));
    let produced = decoded?;
    let round_trip = produced == data.len() && restored.get(..produced) == Some(data);
    if !round_trip {
        return Err(Error::measure(
            format!(
                "the {} round trip over {}",
                request.codec.display, entry.name
            ),
            "did not return the input it was given, so no number from it means anything",
        ));
    }
    let decode_repetitions = batch(entry.class(), decode_ns);
    let decode = sample(tier, decode_repetitions, || {
        session.decompress(&payload, &mut restored)
    })?;

    metrics.add(
        "compressed_bytes",
        Metric::count(
            u64::try_from(compressed_bytes).unwrap_or(u64::MAX),
            "bytes",
            "the length the library reported for the compressed output",
        ),
    );
    metrics.add("compression_ratio", ratio(entry.bytes, compressed_bytes));
    metrics.add(
        "encode_throughput",
        throughput(
            &encode,
            entry.bytes,
            "input bytes over the median encode sample",
        ),
    );
    metrics.add(
        "decode_throughput",
        throughput(
            &decode,
            entry.bytes,
            "input bytes over the median decode sample",
        ),
    );
    metrics.add(
        "peak_rss",
        environment::peak_rss_bytes().map_or_else(
            || Metric::absent("this host refused getrusage"),
            |bytes| Metric::count(bytes, "bytes", environment::PEAK_RSS_METHOD),
        ),
    );

    // The cold pass: a fresh session, one compression, and release, all inside one allocator
    // interval. It is what an allocation count and a working-set figure are about.
    let cold = if representative {
        Some(cold_pass(request.codec, point, data, &mut compressed)?)
    } else {
        None
    };
    let deferred = format!(
        "measured once per size class, on {}, which is the largest entry the {} class \
         selected",
        representative_name(entry, context.selection),
        entry.class().name()
    );

    metrics.add(
        "codec_owned_bytes",
        codec_owned(
            session.encoder_state_bytes(),
            &notes.state_bytes,
            cold.as_ref(),
            &deferred,
        ),
    );
    metrics.add(
        "decoder_owned_bytes",
        decoder_owned(session.decoder_state_bytes(), &notes.state_bytes),
    );
    metrics.add(
        "allocations",
        allocations(&notes.allocations, cold.as_ref(), &deferred),
    );

    let streaming = if representative && notes.streaming.is_available() {
        Some(stream_pass(&mut session, data, bound)?)
    } else {
        None
    };
    let (first_output, streaming_latency) = streaming_metrics(
        streaming.as_ref(),
        &notes.streaming,
        representative,
        &deferred,
    );
    metrics.add("encode_first_output_latency", first_output);
    metrics.add("encode_streaming_latency", streaming_latency);

    metrics.add(
        "parallel_scaling",
        parallel(
            &session,
            &notes.threads,
            data,
            &mut compressed,
            representative,
            &deferred,
        ),
    );

    let counter = counters::unavailable_reason(&environment.counter);
    for name in [
        "encode_cycles_per_byte",
        "decode_cycles_per_byte",
        "instructions_per_byte",
    ] {
        metrics.add(
            name,
            if environment.counter.availability.is_granted() {
                Metric::absent(
                    "this host grants the counter and this revision reads no counter around \
                     a measurement yet",
                )
            } else {
                Metric::absent(counter.clone())
            },
        );
    }

    for name in ["random_range_latency", "range_amplification"] {
        metrics.add(
            name,
            Metric::absent(
                "no codec path in this repository produces a range read, and no competitor \
                 measured here publishes one either. The metric waits for the Entroq index \
                 and range decode.",
            ),
        );
    }

    Ok(Measurement {
        codec: request.codec.name,
        display_name: request.codec.display,
        version: request.codec.version,
        operating_point: String::from(point),
        format: notes.format,
        integrity: notes.integrity,
        entry: entry.name,
        group: entry.group.name(),
        class: entry.class().name(),
        input_bytes: entry.bytes,
        input_digest: entry.digest,
        threads: environment::MEASUREMENT_THREADS,
        round_trip_verified: round_trip,
        encode: sampling(tier, &encode),
        decode: sampling(tier, &decode),
        metrics,
    })
}

fn representative_name(entry: &Entry, selection: &Selection) -> String {
    selection
        .selected
        .iter()
        .filter(|other| other.class() == entry.class())
        .max_by_key(|other| (other.bytes, other.name))
        .map_or_else(
            || String::from("no entry"),
            |largest| String::from(largest.name),
        )
}

/// What the cold pass measured.
struct Cold {
    allocations: u64,
    allocated_bytes: u64,
    peak_bytes: u64,
}

fn cold_pass(codec: &Codec, point: &str, data: &[u8], into: &mut [u8]) -> Result<Cold> {
    let interval = alloc::open();
    let outcome = {
        // The session opens, compresses, and releases inside the interval, so the count is
        // what one compression costs from cold rather than what a warm context costs.
        match Session::open(codec, point) {
            Ok(mut session) => session.compress(data, into),
            Err(failure) => Err(failure),
        }
    };
    let usage = alloc::close(&interval);
    let _ = outcome?;
    Ok(Cold {
        allocations: usage.allocations,
        allocated_bytes: usage.bytes,
        peak_bytes: usage.peak_bytes,
    })
}

/// Every chunk of one streamed compression.
struct Streamed {
    first_output_ns: u64,
    /// The input bytes fed before the first output byte appeared.
    first_output_input_bytes: usize,
    latencies_ns: Vec<u64>,
}

fn stream_pass(session: &mut Session, data: &[u8], bound: usize) -> Result<Streamed> {
    // A streamed compression ends a block per chunk, so it produces more bytes than a single
    // call over the same input. The room is the single-call bound plus a per-chunk margin.
    let chunks = data.len().div_ceil(STREAM_CHUNK).max(1);
    let room = bound
        .saturating_add(chunks.saturating_mul(64))
        .saturating_add(4096);
    let mut into = vec![0_u8; room];
    let produced = session.stream(data, STREAM_CHUNK, &mut into)?;

    let first = produced
        .iter()
        .scan((0_u64, 0_usize), |carried, chunk| {
            carried.0 = carried.0.saturating_add(chunk.elapsed_ns);
            carried.1 = carried.1.saturating_add(chunk.input_bytes);
            Some((carried.0, carried.1, chunk.output_bytes))
        })
        .find(|(_, _, output)| *output > 0);
    Ok(Streamed {
        first_output_ns: first.map_or(0, |(elapsed, _, _)| elapsed),
        first_output_input_bytes: first.map_or(0, |(_, fed, _)| fed),
        latencies_ns: produced.iter().map(|chunk| chunk.elapsed_ns).collect(),
    })
}

fn streaming_metrics(
    streamed: Option<&Streamed>,
    method: &Method,
    representative: bool,
    deferred: &str,
) -> (Metric, Metric) {
    let Some(streamed) = streamed else {
        let reason = if method.is_available() && !representative {
            String::from(deferred)
        } else {
            String::from(method.text())
        };
        return (Metric::absent(reason.clone()), Metric::absent(reason));
    };
    let mut sorted = streamed.latencies_ns.clone();
    sorted.sort_unstable();
    let median = sorted
        .get(sorted.len().saturating_sub(1).div_euclid(2))
        .copied()
        .unwrap_or(0);
    (
        Metric::count(
            streamed.first_output_ns,
            "nanoseconds",
            format!(
                "{}, timed from the first chunk fed to the first chunk that produced \
                 output, after {} input bytes",
                method.text(),
                streamed.first_output_input_bytes
            ),
        ),
        Metric::count(
            median,
            "nanoseconds",
            format!(
                "{}, the median time from feeding one {STREAM_CHUNK} byte chunk to its \
                 output being available, over {} chunks",
                method.text(),
                streamed.latencies_ns.len()
            ),
        ),
    )
}

fn parallel(
    session: &Session,
    method: &Method,
    data: &[u8],
    into: &mut [u8],
    representative: bool,
    deferred: &str,
) -> Metric {
    if !method.is_available() {
        return Metric::absent(method.text());
    }
    if !representative {
        return Metric::absent(String::from(deferred));
    }
    let Ok(available) = std::thread::available_parallelism() else {
        return Metric::absent("this host does not report how many threads it offers");
    };
    let workers = u32::try_from(available.get()).unwrap_or(1).clamp(1, 4);
    if workers < 2 {
        return Metric::absent("this host offers one thread, so there is nothing to scale");
    }
    let (single, single_ns) = clock::time(|| session.threaded(1, data, into));
    if !matches!(single, Some(Ok(_))) {
        return Metric::absent("the library refused a single worker thread");
    }
    let (many, many_ns) = clock::time(|| session.threaded(workers, data, into));
    if !matches!(many, Some(Ok(_))) {
        return Metric::absent(format!(
            "the library refused {workers} worker threads, which a build compiled without \
             multithreading does"
        ));
    }
    if many_ns == 0 {
        return Metric::absent("the threaded compression was below the clock's resolution");
    }
    #[allow(clippy::cast_precision_loss)]
    let scaling = single_ns as f64 / many_ns as f64;
    Metric::rate(
        scaling,
        "speedup",
        format!(
            "{}, one worker against {workers} workers, one sample each, over this entry",
            method.text()
        ),
    )
}

fn codec_owned(
    published: Option<u64>,
    method: &Method,
    cold: Option<&Cold>,
    deferred: &str,
) -> Metric {
    if let Some(bytes) = published {
        return Metric::count(bytes, "bytes", method.text());
    }
    if !method.is_available() {
        return Metric::absent(method.text());
    }
    cold.map_or_else(
        || Metric::absent(String::from(deferred)),
        |cold| {
            Metric::count(
                cold.peak_bytes,
                "bytes",
                format!(
                    "{}, as the largest amount outstanding during one cold compression",
                    method.text()
                ),
            )
        },
    )
}

/// The bytes the decoder holds, when the project publishes a call that reports them.
fn decoder_owned(published: Option<u64>, method: &Method) -> Metric {
    published.map_or_else(
        || {
            Metric::absent(format!(
                "{}. No decoder state size is published for this competitor.",
                method.text()
            ))
        },
        |bytes| Metric::count(bytes, "bytes", method.text()),
    )
}

fn allocations(method: &Method, cold: Option<&Cold>, deferred: &str) -> Metric {
    if !method.is_available() {
        return Metric::absent(method.text());
    }
    cold.map_or_else(
        || Metric::absent(String::from(deferred)),
        |cold| {
            Metric::count(
                cold.allocations,
                "allocations",
                format!(
                    "{}, counted over one cold compression that opened the context, \
                     compressed once, and released it, carrying {} bytes in total",
                    method.text(),
                    cold.allocated_bytes
                ),
            )
        },
    )
}

fn ratio(input_bytes: u64, compressed_bytes: usize) -> Metric {
    let compressed = u64::try_from(compressed_bytes).unwrap_or(u64::MAX);
    if compressed == 0 {
        return Metric::absent("the library produced no output, so there is no ratio");
    }
    #[allow(clippy::cast_precision_loss)]
    let value = input_bytes as f64 / compressed as f64;
    Metric::rate(
        value,
        "input bytes per compressed byte",
        "the registered entry size over the compressed length",
    )
}

fn throughput(samples: &Samples, input_bytes: u64, method: &'static str) -> Metric {
    samples
        .statistics()
        .and_then(|statistics| statistics.throughput(input_bytes))
        .map_or_else(
            || Metric::absent("no sample was taken"),
            |rate| Metric::rate(rate, "bytes per second", method),
        )
}

/// Whether a size class needs its measurement batched, and how many repetitions it takes.
///
/// Every clock on the development host quantizes at tens of nanoseconds, and a compression
/// of a tiny or small entry lands within tens of ticks. The medium class upward is already
/// far above the quantum, so it is timed once per sample.
fn batch(class: SizeClass, cost_ns: u64) -> u32 {
    match class {
        SizeClass::Tiny | SizeClass::Small => clock::repetitions(cost_ns),
        SizeClass::Medium | SizeClass::Large | SizeClass::Huge => 1,
    }
}

/// Takes samples until the tier's count is reached or its budget is spent, never fewer
/// than one.
fn sample<F>(tier: Tier, repetitions: u32, mut work: F) -> Result<Samples>
where
    F: FnMut() -> Result<usize>,
{
    let mut samples = Samples::new(repetitions);
    let started = Instant::now();
    loop {
        let mut outcome: Result<usize> = Ok(0);
        let ((), batch_ns) = clock::time(|| {
            for _ in 0..repetitions {
                outcome = work();
                if outcome.is_err() {
                    break;
                }
            }
        });
        let _ = outcome?;
        samples.push_batch(batch_ns);
        if samples.count() >= tier.samples() || started.elapsed() >= tier.sample_budget() {
            return Ok(samples);
        }
    }
}

fn sampling(tier: Tier, samples: &Samples) -> Option<Sampling> {
    let statistics = samples.statistics()?;
    let accepted = tier.accepted_spread();
    Some(Sampling {
        samples: statistics.samples,
        repetitions: statistics.repetitions,
        min_ns: statistics.min_ns,
        median_ns: statistics.median_ns,
        p95_ns: statistics.p95_ns,
        max_ns: statistics.max_ns,
        spread: statistics.spread,
        accepted_spread: accepted,
        // One sample has no spread to judge, and a zero would read as perfect stability.
        spread_accepted: accepted
            .filter(|_| statistics.samples > 1)
            .map(|limit| statistics.spread <= limit),
    })
}

#[cfg(test)]
mod tests {
    use super::{batch, is_representative, ratio, sample};
    use crate::plan::{self, Tier};
    use crate::registry::{ENTRIES, SizeClass, find};

    #[test]
    fn a_small_input_is_batched_and_a_medium_one_is_not() {
        assert!(batch(SizeClass::Tiny, 500) > 1);
        assert!(batch(SizeClass::Small, 500) > 1);
        assert_eq!(batch(SizeClass::Medium, 500), 1);
        assert_eq!(batch(SizeClass::Large, 500), 1);
    }

    #[test]
    fn a_class_has_exactly_one_representative_entry() {
        let selection = plan::select(Tier::Gate, Tier::Gate.classes(), ENTRIES);
        for class in Tier::Gate.classes() {
            let representatives = selection
                .selected
                .iter()
                .filter(|entry| entry.class() == *class)
                .filter(|entry| is_representative(entry, &selection))
                .count();
            assert_eq!(representatives, 1, "{}", class.name());
        }
    }

    #[test]
    fn the_representative_of_a_class_is_its_largest_selected_entry() {
        let selection = plan::select(Tier::Dev, Tier::Dev.classes(), ENTRIES);
        let largest = selection
            .selected
            .iter()
            .filter(|entry| entry.class() == SizeClass::Medium)
            .max_by_key(|entry| (entry.bytes, entry.name));
        assert!(largest.is_some_and(|entry| is_representative(entry, &selection)));
    }

    #[test]
    fn a_ratio_is_the_input_over_the_output() {
        let metric = ratio(1000, 250);
        let json = metric.to_json();
        assert_eq!(
            json.get("value").and_then(serde_json::Value::as_f64),
            Some(4.0)
        );
    }

    #[test]
    fn a_compression_that_produced_nothing_has_no_ratio() {
        assert!(!ratio(1000, 0).is_measured());
    }

    #[test]
    fn a_measurement_always_takes_at_least_one_sample() {
        let samples = sample(Tier::Smoke, 1, || Ok(1));
        assert!(samples.is_ok_and(|samples| samples.count() >= 1));
    }

    #[test]
    fn a_measurement_stops_at_the_tier_sample_count() {
        let samples = sample(Tier::Smoke, 1, || Ok(1));
        assert!(samples.is_ok_and(|samples| samples.count() <= Tier::Smoke.samples()));
    }

    #[test]
    fn a_failing_operation_is_reported_rather_than_sampled() {
        let failed = sample(Tier::Smoke, 1, || {
            Err(crate::error::Error::measure("a library", "refused"))
        });
        assert!(failed.is_err());
    }

    #[test]
    fn an_entry_outside_the_selection_is_not_a_representative() {
        let selection = plan::select(Tier::Smoke, Tier::Smoke.classes(), ENTRIES);
        let medium = find("project-json-medium");
        assert!(medium.is_some_and(|entry| !is_representative(entry, &selection)));
    }
}
