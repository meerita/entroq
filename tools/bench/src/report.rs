//! Owns what the report commands read and what they emit: which files a result root holds,
//! the parse report, and the frontier report.
//!
//! Both documents are machine-readable, and both name every file they were built from. A
//! plotted point carries the result file it came from, so the path from a recorded
//! measurement to a marked point is in the document rather than in somebody's memory.
//!
//! This module does not own how a result is read or how a frontier is computed.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::pareto::{self, AXES, Absent, Chart, Plot, Plotted, Subject};
use crate::parse::{self, Document};

/// The name a segment writes its result document under.
const RESULT_FILE: &str = "result.json";

pub const PARSE_SCHEMA: &str = "entroq.bench.parse/1";
pub const PARETO_SCHEMA: &str = "entroq.bench.pareto/1";

/// Reads every result under the given roots.
///
/// A root that is a file is read as one result whatever it is named. A root that is a
/// directory is searched for every file named `result.json`, so a run record root yields the
/// results of every campaign under it.
///
/// # Errors
///
/// Fails when a root does not exist, when a directory cannot be read, when no result is
/// found, or when any result is not a document the parser fully understands.
pub fn read(roots: &[PathBuf]) -> Result<Vec<Document>> {
    let paths = discover(roots)?;
    if paths.is_empty() {
        return Err(Error::report(
            describe(roots),
            String::from("holds no result document, so there is nothing to read"),
        ));
    }
    paths.iter().map(|path| parse::read(path)).collect()
}

/// The parse report: every result read, and how much of each one was checked.
pub fn parsed(documents: &[Document], produced_at: &str) -> Value {
    json!({
        "schema": PARSE_SCHEMA,
        "produced_at": produced_at,
        "reads": parse::SCHEMA,
        "results": documents.iter().map(source).collect::<Vec<_>>(),
        "totals": totals(documents),
        "limits": [
            "Every result named here was read whole. A document that carried a field this \
             parser does not declare, or that omitted one it does, was rejected rather than \
             read in part.",
            "This report states what was read. It states no measurement and no comparison.",
        ],
    })
}

/// The frontier report: every chart, every axis, and every point another point dominates.
pub fn frontier(documents: &[Document], charts: &[Chart], produced_at: &str) -> Value {
    json!({
        "schema": PARETO_SCHEMA,
        "produced_at": produced_at,
        "reads": parse::SCHEMA,
        "sources": documents.iter().map(source).collect::<Vec<_>>(),
        "axes": AXES
            .iter()
            .map(|axis| json!({
                "axis": axis.name,
                "title": axis.title,
                "x": { "metric": pareto::RATIO, "better": "higher" },
                "y": { "metric": axis.metric, "better": axis.better.name() },
            }))
            .collect::<Vec<_>>(),
        "charts": charts.iter().map(chart).collect::<Vec<_>>(),
        "limits": limits(documents, charts),
    })
}

fn source(document: &Document) -> Value {
    json!({
        "path": document.path.display().to_string(),
        "segment": document.segment,
        "tier": document.tier,
        "tier_is_publication": document.tier_is_publication,
        "codec": document.codec,
        "size_class": document.size_class,
        "operating_point_group": document.operating_point_group,
        "produced_at": document.produced_at,
        "entroq_measured": document.entroq_measured,
        "measurements": document.rows.len(),
        "readings": document.readings(),
        "readings_measured": document.measured(),
        "fields_checked": document.fields,
    })
}

fn totals(documents: &[Document]) -> Value {
    json!({
        "results": documents.len(),
        "measurements": documents.iter().map(|d| d.rows.len()).sum::<usize>(),
        "readings": documents.iter().map(Document::readings).sum::<usize>(),
        "readings_measured": documents.iter().map(Document::measured).sum::<usize>(),
        "fields_checked": documents.iter().map(|d| d.fields).sum::<usize>(),
        "fields_skipped": 0,
    })
}

fn chart(chart: &Chart) -> Value {
    json!({
        "subject": subject(&chart.subject),
        "axes": chart.plots.iter().map(plot).collect::<Vec<_>>(),
    })
}

fn subject(subject: &Subject) -> Value {
    let host = &subject.host;
    json!({
        "tier": subject.tier,
        "tier_is_publication": subject.tier_is_publication,
        "tier_licence": subject.tier_licence,
        "entry": subject.entry,
        "group": subject.group,
        "size_class": subject.class,
        "input_bytes": subject.input_bytes,
        "input_digest": subject.input_digest,
        "threads": subject.threads,
        "host": {
            "os": host.os,
            "os_version": host.os_version,
            "arch": host.arch,
            "cpu_model": host.cpu_model,
            "cores": host.cores,
            "memory": host.memory,
            "rustc": host.rustc,
            "build_profile": host.build_profile,
            "harness_version": host.harness_version,
            "counter_backend": host.counter_backend,
            "counter_granted": host.counter_granted,
        },
    })
}

fn plot(plot: &Plot) -> Value {
    json!({
        "axis": plot.axis.name,
        "title": plot.axis.title,
        "y": { "metric": plot.axis.metric, "better": plot.axis.better.name() },
        "has_data": plot.has_data(),
        "no_data_reason": plot.no_data(),
        "points": plot.points.iter().map(point).collect::<Vec<_>>(),
        "frontier": plot
            .points
            .iter()
            .filter(|point| !point.dominated())
            .map(Plotted::label)
            .collect::<Vec<_>>(),
        "not_plotted": plot.absent.iter().map(absent).collect::<Vec<_>>(),
    })
}

fn point(point: &Plotted) -> Value {
    json!({
        "label": point.label(),
        "codec": point.codec,
        "version": point.version,
        "operating_point": point.operating_point,
        "tier": point.tier,
        "source": point.source,
        "ratio": point.ratio,
        "value": point.value,
        "unit": point.unit,
        "method": point.method,
        "dominated": point.dominated(),
        "dominated_by": point.dominated_by,
        "inside_reported_spread": point.inside_reported_spread,
    })
}

fn absent(absent: &Absent) -> Value {
    json!({
        "label": absent.label,
        "metric": absent.metric,
        "reason": absent.reason,
    })
}

/// What this report may be read as: what the frontier means here, plus every limit the
/// results it was built from already stated. A limit a measurement recorded does not stop
/// applying when its numbers are plotted.
fn limits(documents: &[Document], charts: &[Chart]) -> Value {
    let mut limits = vec![
        String::from(
            "One chart holds one tier, one host, one corpus entry, and one thread count. \
             Nothing here compares two of any of them, and every point states the tier it \
             came from.",
        ),
        String::from(
            "A point is marked dominated when another point on the same chart is equal or \
             better in compression ratio and in the axis metric, and strictly better in at \
             least one of them. A point equal in both is not dominated.",
        ),
        String::from(
            "A dominated point is not a rejected one. It is a point that another point \
             beats on this axis, and it can still be the right choice for a property this \
             axis does not measure, such as bounded memory, streaming, random access, or \
             corruption isolation.",
        ),
        String::from(
            "A verdict marked inside_reported_spread rests on a margin no larger than the \
             spread the samples behind it reported. It is a difference this data cannot \
             separate from noise.",
        ),
        String::from(
            "A row whose result recorded no number for an axis metric is not plotted on that \
             axis. It is named under not_plotted with the reason the result gave. An axis \
             whose points are a subset of the codecs measured is the frontier of that \
             subset.",
        ),
        String::from(
            "Every number here was read from a recorded result. Nothing is derived, \
             converted, or retyped.",
        ),
    ];
    for line in integrity(documents) {
        limits.push(line);
    }
    for chart in charts {
        if !limits.contains(&chart.subject.tier_licence) {
            limits.push(chart.subject.tier_licence.clone());
        }
    }
    for document in documents {
        for limit in &document.limits {
            if !limits.contains(limit) {
                limits.push(limit.clone());
            }
        }
    }
    Value::from(limits)
}

/// What each competitor's measured bytes were protected by, one line each.
///
/// A comparison of throughput across codecs is only fair when the work behind each number is
/// equivalent, and a checksum is work. So the frontier states the integrity setting every
/// point on it was produced under, rather than leaving a reader to assume they agree.
fn integrity(documents: &[Document]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for row in documents.iter().flat_map(|document| &document.rows) {
        let line = format!(
            "Integrity setting for {}, and the thread count behind every number from it: {} \
             Threads: {}.",
            row.display_name, row.integrity, row.threads
        );
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    lines
}

/// Writes a document to standard output.
///
/// # Errors
///
/// Fails when the document cannot be serialized.
pub fn emit(document: &Value) -> Result<()> {
    let text = serde_json::to_string_pretty(document)
        .map_err(|e| Error::report("the report document", e.to_string()))?;
    println!("{text}");
    Ok(())
}

/// The lines a person reading the report sees. They go to standard error, so the document on
/// standard output stays a document.
pub fn log_parse(documents: &[Document]) {
    eprintln!(
        "read {} result {}",
        documents.len(),
        if documents.len() == 1 {
            "document"
        } else {
            "documents"
        }
    );
    for document in documents {
        eprintln!(
            "  {}  {} at the {} tier: {} measurements, {} readings, {} fields checked",
            document.path.display(),
            document.codec,
            document.tier,
            document.rows.len(),
            document.readings(),
            document.fields,
        );
    }
}

/// The frontier a person reads, one line per point, with the tier beside each one.
pub fn log_frontier(charts: &[Chart]) {
    eprintln!("{} charts", charts.len());
    for chart in charts {
        let subject = &chart.subject;
        eprintln!(
            "{} at the {} tier, {} bytes, {} thread(s), on {} {}",
            subject.entry,
            subject.tier,
            subject.input_bytes,
            subject.threads,
            subject.host.cpu_model,
            subject.host.arch,
        );
        for plot in &chart.plots {
            if let Some(reason) = plot.no_data() {
                eprintln!("  {}: no data. {reason}", plot.axis.title);
                continue;
            }
            eprintln!("  {}:", plot.axis.title);
            for point in &plot.points {
                eprintln!(
                    "    {:<24} [{}] ratio {:.4}  {} {}  {}",
                    point.label(),
                    point.tier,
                    point.ratio,
                    point.value,
                    point.unit,
                    verdict(point),
                );
            }
            for absent in &plot.absent {
                eprintln!("    {:<24} not plotted: {}", absent.label, absent.reason);
            }
        }
    }
}

fn verdict(point: &Plotted) -> String {
    if !point.dominated() {
        return String::from("on the frontier");
    }
    let by = point.dominated_by.join(", ");
    if point.inside_reported_spread {
        return format!("dominated by {by}, inside the reported spread");
    }
    format!("dominated by {by}")
}

fn describe(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every result file under the given roots, in a stable order.
fn discover(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for root in roots {
        let kind = std::fs::metadata(root).map_err(|e| Error::at("read", root, e))?;
        if kind.is_dir() {
            walk(root, &mut found)?;
        } else {
            found.push(root.clone());
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

fn walk(root: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let listing =
            std::fs::read_dir(&directory).map_err(|e| Error::at("read", &directory, e))?;
        for entry in listing {
            let entry = entry.map_err(|e| Error::at("read", &directory, e))?;
            // `file_type` does not follow a symbolic link, so a link that points at an
            // ancestor is a file here and the walk cannot loop through it.
            let kind = entry
                .file_type()
                .map_err(|e| Error::at("read", &entry.path(), e))?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if entry.file_name() == RESULT_FILE {
                found.push(entry.path());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::Value;

    use super::{PARETO_SCHEMA, PARSE_SCHEMA, discover, frontier, parsed, read};
    use crate::pareto;
    use crate::parse::{self, fixture};

    fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("entroq-bench-report-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    /// Writes one result document into `<root>/segments/<segment>/attempt-01/result.json`.
    fn write(root: &Path, segment: &str, document: &Value) -> Option<PathBuf> {
        let directory = root.join("segments").join(segment).join("attempt-01");
        std::fs::create_dir_all(&directory).ok()?;
        let path = directory.join("result.json");
        let text = serde_json::to_string(document).ok()?;
        std::fs::write(&path, text).ok()?;
        Some(path)
    }

    fn one(codec: &str, ratio: f64) -> Value {
        fixture::document(
            "dev",
            &[fixture::row(
                codec,
                "default",
                "fixture-entry",
                &fixture::metrics(&[
                    (pareto::RATIO, fixture::measured(ratio, "ratio")),
                    (
                        "encode_throughput",
                        fixture::measured(100.0, "bytes per second"),
                    ),
                ]),
            )],
        )
    }

    #[test]
    fn a_directory_yields_every_result_under_it_in_a_stable_order() {
        let root = root("discover");
        assert!(write(&root, "bench-zstd", &one("zstd", 3.0)).is_some());
        assert!(write(&root, "bench-lz4", &one("lz4", 2.0)).is_some());
        // A file the walk must not read as a result.
        assert!(std::fs::write(root.join("segments").join("summary.md"), "not a result").is_ok());
        let found = discover(std::slice::from_ref(&root)).unwrap_or_default();
        let names: Vec<String> = found
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        assert_eq!(found.len(), 2, "{names:?}");
        assert!(names.iter().all(|name| name.ends_with("result.json")));
        assert!(
            names.first().is_some_and(|name| name.contains("bench-lz4")),
            "{names:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_root_that_holds_no_result_is_reported_rather_than_read_as_empty() {
        let root = root("empty");
        assert!(std::fs::create_dir_all(&root).is_ok());
        let failure = read(std::slice::from_ref(&root));
        let message = failure.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("nothing to read"), "{message}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_root_that_does_not_exist_is_reported() {
        let missing = std::env::temp_dir().join("entroq-bench-report-absent");
        let _ = std::fs::remove_dir_all(&missing);
        assert!(read(&[missing]).is_err());
    }

    #[test]
    fn the_parse_report_states_what_was_read_and_that_nothing_was_skipped() {
        let documents = vec![document("lz4", 2.0), document("zstd", 3.0)];
        let report = parsed(&documents, "2026-09-12T00:00:00Z");
        assert_eq!(
            report.get("schema").and_then(Value::as_str),
            Some(PARSE_SCHEMA)
        );
        let totals = report.get("totals");
        assert_eq!(
            totals
                .and_then(|t| t.get("results"))
                .and_then(Value::as_u64),
            Some(2)
        );
        assert_eq!(
            totals
                .and_then(|t| t.get("fields_skipped"))
                .and_then(Value::as_u64),
            Some(0)
        );
        let checked = totals
            .and_then(|t| t.get("fields_checked"))
            .and_then(Value::as_u64)
            .unwrap_or_default();
        assert!(checked > 100, "{checked} fields checked");
    }

    fn document(codec: &str, ratio: f64) -> parse::Document {
        let value = one(codec, ratio);
        let read = parse::document(Path::new("fixture.json"), &value);
        assert!(read.is_ok(), "the fixture must parse");
        read.unwrap_or_else(|_| unreachable!("checked above"))
    }

    #[test]
    fn every_plotted_point_names_its_tier_and_the_file_it_came_from() {
        let documents = vec![document("lz4", 2.0), document("zstd", 3.0)];
        let charts = pareto::charts(&documents).unwrap_or_default();
        let report = frontier(&documents, &charts, "2026-09-12T00:00:00Z");
        assert_eq!(
            report.get("schema").and_then(Value::as_str),
            Some(PARETO_SCHEMA)
        );
        let points = report
            .get("charts")
            .and_then(Value::as_array)
            .and_then(|charts| charts.first())
            .and_then(|chart| chart.get("axes"))
            .and_then(Value::as_array)
            .and_then(|axes| axes.first())
            .and_then(|axis| axis.get("points"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(points.len(), 2);
        for point in &points {
            assert_eq!(point.get("tier").and_then(Value::as_str), Some("dev"));
            assert_eq!(
                point.get("source").and_then(Value::as_str),
                Some("fixture.json")
            );
        }
    }

    #[test]
    fn the_frontier_report_declares_all_five_axes_whatever_the_results_carried() {
        let documents = vec![document("lz4", 2.0)];
        let charts = pareto::charts(&documents).unwrap_or_default();
        let report = frontier(&documents, &charts, "2026-09-12T00:00:00Z");
        let declared = report
            .get("axes")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();
        assert_eq!(declared, 5);
        let axes = report
            .get("charts")
            .and_then(Value::as_array)
            .and_then(|charts| charts.first())
            .and_then(|chart| chart.get("axes"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(axes.len(), 5);
        let empty: Vec<&str> = axes
            .iter()
            .filter(|axis| axis.get("has_data").and_then(Value::as_bool) == Some(false))
            .filter_map(|axis| axis.get("axis").and_then(Value::as_str))
            .collect();
        assert!(empty.contains(&"ratio-against-encode-cpu"), "{empty:?}");
        assert!(empty.contains(&"ratio-against-decode-cpu"), "{empty:?}");
        for axis in &axes {
            if axis.get("has_data").and_then(Value::as_bool) == Some(false) {
                assert!(
                    axis.get("no_data_reason").and_then(Value::as_str).is_some(),
                    "an empty axis states no reason"
                );
            }
        }
    }

    #[test]
    fn the_report_carries_every_limit_the_results_it_read_already_stated() {
        let documents = vec![document("lz4", 2.0)];
        let charts = pareto::charts(&documents).unwrap_or_default();
        let report = frontier(&documents, &charts, "2026-09-12T00:00:00Z");
        let limits: Vec<String> = report
            .get("limits")
            .and_then(Value::as_array)
            .map(|limits| {
                limits
                    .iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            limits
                .iter()
                .any(|limit| limit.contains("a fixture licenses nothing")),
            "{limits:?}"
        );
        assert!(
            limits
                .iter()
                .any(|limit| limit.contains("one tier, one host")),
            "{limits:?}"
        );
    }
}
