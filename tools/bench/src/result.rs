//! Owns the result a segment emits: one machine-readable document, and the human-readable
//! log beside it.
//!
//! The document is what a parser reads. Nobody retypes a number out of a terminal, so the
//! document carries the environment, the inputs, every metric with its method, the variance,
//! and the entries the tier's budget left out. The log is for a person watching a segment
//! run; it carries no number that the document does not.
//!
//! The schema identifier this document declares belongs to the parser that reads it, because
//! a host that never linked the laboratory still reads a recorded result.
//!
//! The document always goes to standard output. When the runner names a directory for the
//! segment's evidence, it is written there as well, beside the raw output the runner
//! captures.
//!
//! This module does not own what is measured or what a tier licenses.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::counters;
use crate::error::{Error, Result};
use crate::measure::{Measurement, Outcome, Sampling};
use crate::parse::SCHEMA;
use crate::plan::Request;
use crate::subject::Subject;

/// The file a segment's document is written to inside its evidence directory.
const FILE: &str = "result.json";

/// The variable the runner sets to the directory a segment's evidence goes to.
pub const SEGMENT_DIR: &str = "ENTROQ_SEGMENT_DIR";

/// What the Entroq column of a result means, for a segment that measured the codec.
const ENTROQ_MEASURED: &str = "This segment measured Entroq, driven in-process through the \
crate of this workspace at the revision the environment records. It crossed the same \
boundary a competitor crosses: one call, one process, no subprocess and no command line \
tool.";

/// What the Entroq column of a result means, for a segment that measured a competitor.
///
/// It is stated rather than left blank, and no zero, no placeholder, and no absent row stands
/// in for a number nobody measured here.
const ENTROQ_ELSEWHERE: &str = "This segment measured a competitor, so it carries no Entroq \
row. The harness links the Entroq codec and measures it in its own segment. No zero, no \
placeholder, and no default stands in for a number this segment did not produce.";

/// The Entroq column of one segment's result.
fn entroq_column(request: &Request) -> (bool, &'static str) {
    if request.subject == Subject::Entroq {
        (true, ENTROQ_MEASURED)
    } else {
        (false, ENTROQ_ELSEWHERE)
    }
}

/// Builds the document for one segment.
pub fn document(request: &Request, outcome: &Outcome, produced_at: &str) -> Value {
    let (measured, reason) = entroq_column(request);
    json!({
        "schema": SCHEMA,
        "produced_at": produced_at,
        "segment": request.segment,
        "tier": request.tier.name(),
        "tier_is_publication": request.tier.is_publication(),
        "tier_licence": request.tier.licence(),
        "codec": request.subject.name(),
        "size_class": request.class.map_or(Value::Null, |class| Value::from(class.name())),
        "operating_point_group": request
            .points
            .map_or(Value::Null, |group| Value::from(group.name)),
        "entroq": {
            "measured": measured,
            "reason": reason,
        },
        "environment": environment_json(outcome),
        "laboratory": laboratory_json(request, outcome),
        "inputs": inputs_json(outcome),
        "measurements": outcome
            .measurements
            .iter()
            .map(measurement_json)
            .collect::<Vec<_>>(),
        "unavailable_metrics": unavailable_json(outcome),
        "limits": limits(request, outcome),
    })
}

fn environment_json(outcome: &Outcome) -> Value {
    let environment = &outcome.environment;
    json!({
        "os": environment.os,
        "os_version": environment.os_version,
        "arch": environment.arch,
        "cpu_model": environment.cpu_model,
        "cpu_features_compiled": environment.cpu_features,
        "cpu_feature_method": "the features this binary was compiled for. The harness \
                               dispatches on none, so the compiled set is the executed set.",
        "cores": environment.cores,
        "memory": environment.memory,
        "frequency_policy": environment.frequency_policy,
        "rustc": environment.rustc,
        "cargo": environment.cargo,
        "build_profile": environment.build_profile,
        "entroq_revision": environment.revision,
        "harness_version": environment.harness_version,
        "encoder_version": codec::VERSION,
        "statistics_enabled": false,
        "counter_backend": {
            "name": environment.counter.name,
            "granted": environment.counter.availability.is_granted(),
            "reason": environment
                .counter
                .availability
                .reason()
                .map_or(Value::Null, Value::from),
        },
    })
}

fn laboratory_json(request: &Request, outcome: &Outcome) -> Value {
    let name = request.subject.name();
    let libraries = outcome
        .laboratory
        .iter()
        .filter(|check| check.codec == name)
        .map(|check| {
            json!({
                "codec": check.codec,
                "version": check.version,
                "library": check.library,
                "linked_from": check.linked_from,
                "digest": check.digest,
                "matches_manifest": check.matches_manifest,
                "note": check.note,
            })
        })
        .collect::<Vec<_>>();
    // Entroq has no upstream pin and no archive. Its provenance is the repository revision,
    // which is what a rebuild of this binary reproduces it from, so the field that names a
    // competitor's commit names that revision and the provenance says so.
    let commit = request
        .subject
        .competitor()
        .map_or(outcome.environment.revision.as_str(), |codec| codec.commit);
    json!({
        "codec": name,
        "pinned_version": request.subject.version(),
        "upstream_commit": commit,
        "reported_version": outcome.linked_version.clone().map_or(Value::Null, Value::from),
        "version_agrees": outcome
            .linked_version
            .as_ref()
            .map_or(Value::Null, |reported| {
                Value::from(request.subject.version().contains(reported.as_str()))
            }),
        "libraries": libraries,
        "other_codecs_linked": other_codecs(request, outcome),
        "provenance": provenance(request),
    })
}

/// How the bytes this segment measured were obtained.
const fn provenance(request: &Request) -> &'static str {
    if request.subject.competitor().is_none() {
        return "Entroq is a crate of this workspace and no archive was linked for it, so the \
                commit above is the repository revision this binary was built from and the \
                library list is empty. The version above is the one the codec crate declares \
                for itself, which the session reports back at run time.";
    }
    "Every archive the linker was given is named by absolute path above, and its digest is \
     the digest the laboratory manifest records. The linker was given no search path other \
     than the laboratory's own, so a copy of the same project installed elsewhere on this \
     host was not reachable. A competitor whose library publishes its version reports it \
     above, checked against the version the catalog pins."
}

/// The competitors this binary also linked, named once each.
///
/// A result states them so a reader can see that one binary carries the whole laboratory,
/// and that a segment measures one of them by choice rather than by what was available.
fn other_codecs(request: &Request, outcome: &Outcome) -> Vec<Value> {
    let mut named: Vec<String> = Vec::new();
    for check in &outcome.laboratory {
        if check.codec == request.subject.name() {
            continue;
        }
        let entry = format!("{} {}", check.codec, check.version);
        if !named.contains(&entry) {
            named.push(entry);
        }
    }
    named.into_iter().map(Value::from).collect()
}

fn inputs_json(outcome: &Outcome) -> Value {
    let selection = &outcome.selection;
    json!({
        "budget_bytes_per_class": selection.budget_per_class,
        "selected_bytes": selection.bytes,
        "entries": selection
            .selected
            .iter()
            .map(|entry| json!({
                "name": entry.name,
                "group": entry.group.name(),
                "content": entry.content,
                "class": entry.class().name(),
                "bytes": entry.bytes,
                "digest": entry.digest,
                "license": entry.license.name,
            }))
            .collect::<Vec<_>>(),
        "excluded": selection
            .excluded
            .iter()
            .map(|entry| json!({
                "name": entry.name,
                "class": entry.class().name(),
                "bytes": entry.bytes,
                "reason": "the tier's input budget for this size class did not reach it",
            }))
            .collect::<Vec<_>>(),
    })
}

fn measurement_json(measurement: &Measurement) -> Value {
    json!({
        "codec": measurement.codec,
        "display_name": measurement.display_name,
        "version": measurement.version,
        "operating_point": measurement.operating_point,
        "format": measurement.format,
        "integrity": measurement.integrity,
        "entry": measurement.entry,
        "group": measurement.group,
        "class": measurement.class,
        "input_bytes": measurement.input_bytes,
        "input_digest": measurement.input_digest,
        "threads": measurement.threads,
        "round_trip_verified": measurement.round_trip_verified,
        "encode_samples": measurement.encode.as_ref().map_or(Value::Null, sampling_json),
        "decode_samples": measurement.decode.as_ref().map_or(Value::Null, sampling_json),
        "metrics": measurement.metrics.to_json(),
    })
}

fn sampling_json(sampling: &Sampling) -> Value {
    json!({
        "samples": sampling.samples,
        "repetitions_per_sample": sampling.repetitions,
        "min_ns": sampling.min_ns,
        "median_ns": sampling.median_ns,
        "p95_ns": sampling.p95_ns,
        "max_ns": sampling.max_ns,
        "spread": sampling.spread,
        "spread_definition": "the slowest sample minus the fastest, over the median",
        "accepted_spread": sampling.accepted_spread,
        "spread_accepted": sampling.spread_accepted,
        "spread_note": if sampling.samples > 1 {
            Value::Null
        } else {
            Value::from(
                "one sample was taken, because the measurement budget of this tier did not \
                 buy a second. A single sample has no spread, and none is reported.",
            )
        },
    })
}

/// Every metric no row in this segment produced a number for, with the reason.
fn unavailable_json(outcome: &Outcome) -> Value {
    let mut reported: Vec<(&str, &str)> = Vec::new();
    for measurement in &outcome.measurements {
        for (name, reason) in measurement.metrics.unavailable() {
            if !reported
                .iter()
                .any(|(seen, why)| *seen == name && *why == reason)
            {
                reported.push((name, reason));
            }
        }
    }
    reported
        .into_iter()
        .map(|(metric, reason)| json!({ "metric": metric, "reason": reason }))
        .collect::<Vec<_>>()
        .into()
}

fn limits(request: &Request, outcome: &Outcome) -> Value {
    let counter = counters::unavailable_reason(&outcome.environment.counter);
    let mut limits = vec![
        String::from(request.tier.licence()),
        String::from(entroq_column(request).1),
        String::from(
            "Every number here was produced on one host and one architecture. Nothing here \
             compares two environments.",
        ),
        String::from(
            "The streamed pass, the cold allocation pass, and the threaded pass are \
             measured once per size class, on the largest entry that class selected. Every \
             other row names the entry they were measured on instead of leaving the field \
             blank.",
        ),
    ];
    if !counter.is_empty() {
        limits.push(counter);
    }
    if !outcome.selection.excluded.is_empty() {
        limits.push(format!(
            "The tier's input budget did not reach {} registered {}: {}. This segment covers \
             part of its size classes, not all of them.",
            outcome.selection.excluded.len(),
            if outcome.selection.excluded.len() == 1 {
                "entry"
            } else {
                "entries"
            },
            outcome
                .selection
                .excluded
                .iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Value::from(limits)
}

/// Writes the document to standard output, and to the segment's evidence directory when the
/// runner named one.
///
/// # Errors
///
/// Fails when the document cannot be serialized or the evidence file cannot be written.
pub fn emit(document: &Value) -> Result<Option<PathBuf>> {
    let text = serde_json::to_string_pretty(document)
        .map_err(|e| Error::measure("the result document", e.to_string()))?;
    println!("{text}");
    let Some(dir) = std::env::var_os(SEGMENT_DIR) else {
        return Ok(None);
    };
    let path = Path::new(&dir).join(FILE);
    std::fs::write(&path, format!("{text}\n")).map_err(|e| Error::at("write", &path, e))?;
    Ok(Some(path))
}

/// The lines a person watching the segment sees. They go to standard error, so the document
/// on standard output stays a document.
pub fn log(request: &Request, outcome: &Outcome, written: Option<&Path>) {
    eprintln!(
        "{} at the {} tier: {} measurements over {} entries, {} bytes",
        request.subject.display(),
        request.tier.name(),
        outcome.measurements.len(),
        outcome.selection.selected.len(),
        outcome.selection.bytes,
    );
    for check in outcome
        .laboratory
        .iter()
        .filter(|check| check.codec == request.subject.name())
    {
        eprintln!(
            "  linked {} {}  {}  manifest {}",
            check.codec,
            check.library,
            check.digest,
            if check.matches_manifest {
                "agrees"
            } else {
                "DISAGREES"
            },
        );
    }
    if !outcome.environment.counter.availability.is_granted() {
        eprintln!(
            "  counter backend {}: unavailable, so the three counter metrics report no number",
            outcome.environment.counter.name
        );
    }
    let measured: usize = outcome
        .measurements
        .iter()
        .map(|m| m.metrics.measured())
        .sum();
    let total: usize = outcome.measurements.iter().map(|m| m.metrics.len()).sum();
    eprintln!("  {measured} of {total} metric readings carry a number; the rest carry a reason");
    match written {
        Some(path) => eprintln!("  result written to {}", path.display()),
        None => eprintln!("  result on standard output only: no segment directory was named"),
    }
}

#[cfg(test)]
mod tests {
    use super::{ENTROQ_ELSEWHERE, ENTROQ_MEASURED, SCHEMA, SEGMENT_DIR};

    #[test]
    fn the_schema_is_versioned_so_a_parser_can_match_on_it() {
        assert!(SCHEMA.starts_with("entroq.bench.result/"));
    }

    #[test]
    fn the_entroq_column_says_what_it_holds_either_way() {
        assert!(ENTROQ_MEASURED.contains("measured Entroq"));
        assert!(ENTROQ_MEASURED.contains("same boundary"));
        assert!(ENTROQ_ELSEWHERE.contains("no Entroq row"));
        assert!(ENTROQ_ELSEWHERE.contains("No zero"));
    }

    #[test]
    fn the_segment_directory_is_named_by_one_variable() {
        assert_eq!(SEGMENT_DIR, "ENTROQ_SEGMENT_DIR");
    }
}
