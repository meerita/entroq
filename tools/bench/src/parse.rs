//! Owns reading a recorded benchmark result: what a document must carry to be read at all,
//! and the typed rows a later stage plots from.
//!
//! A parser that skips a field it does not recognize drops a metric, and a dropped metric
//! reads as an absent one. So this parser understands a document whole or rejects it. Every
//! object is checked against the exact set of keys it may carry: a declared key that is
//! missing fails, and a key nobody declared fails. The count of keys it checked is reported,
//! so "nothing was skipped" is a number rather than a claim.
//!
//! The document read here is the one a measurement writes. The schema identifier lives in
//! this module because a host that never linked the competitor laboratory still reads the
//! results a host that did linked wrote.
//!
//! This module does not own what is measured, how a document is written, or what a frontier
//! does with a row.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::{Error, Result};

/// The schema a result document declares, and the only one this parser reads.
///
/// A change to the document's shape changes this, so a parser meets a document it does not
/// understand as a mismatch rather than as a field it silently ignores.
pub const SCHEMA: &str = "entroq.bench.result/2";

const DOCUMENT: &[&str] = &[
    "schema",
    "produced_at",
    "segment",
    "tier",
    "tier_is_publication",
    "tier_licence",
    "codec",
    "size_class",
    "operating_point_group",
    "entroq",
    "environment",
    "laboratory",
    "inputs",
    "measurements",
    "unavailable_metrics",
    "limits",
];

const ENTROQ: &[&str] = &["measured", "reason"];

const ENVIRONMENT: &[&str] = &[
    "os",
    "os_version",
    "arch",
    "cpu_model",
    "cpu_features_compiled",
    "cpu_feature_method",
    "cores",
    "memory",
    "frequency_policy",
    "rustc",
    "cargo",
    "build_profile",
    "entroq_revision",
    "harness_version",
    "encoder_version",
    "statistics_enabled",
    "counter_backend",
];

const COUNTER: &[&str] = &["name", "granted", "reason"];

const LABORATORY: &[&str] = &[
    "codec",
    "pinned_version",
    "upstream_commit",
    "reported_version",
    "version_agrees",
    "libraries",
    "other_codecs_linked",
    "provenance",
];

const LIBRARY: &[&str] = &[
    "codec",
    "version",
    "library",
    "linked_from",
    "digest",
    "matches_manifest",
    "note",
];

const INPUTS: &[&str] = &[
    "budget_bytes_per_class",
    "selected_bytes",
    "entries",
    "excluded",
];

const INPUT_ENTRY: &[&str] = &[
    "name", "group", "content", "class", "bytes", "digest", "license",
];

const EXCLUDED_ENTRY: &[&str] = &["name", "class", "bytes", "reason"];

const MEASUREMENT: &[&str] = &[
    "codec",
    "display_name",
    "version",
    "operating_point",
    "format",
    "integrity",
    "entry",
    "group",
    "class",
    "input_bytes",
    "input_digest",
    "threads",
    "round_trip_verified",
    "encode_samples",
    "decode_samples",
    "metrics",
];

const SAMPLING: &[&str] = &[
    "samples",
    "repetitions_per_sample",
    "min_ns",
    "median_ns",
    "p95_ns",
    "max_ns",
    "spread",
    "spread_definition",
    "accepted_spread",
    "spread_accepted",
    "spread_note",
];

const UNAVAILABLE: &[&str] = &["metric", "reason"];

const MEASURED_METRIC: &[&str] = &["measured", "value", "unit", "method"];

const ABSENT_METRIC: &[&str] = &["measured", "reason"];

/// Every metric a result document carries, whatever the host could produce a number for.
///
/// A document that omits one of these has dropped a metric, and a document that carries a
/// name this list does not is a document this parser cannot claim to have read. Both are
/// rejected.
pub const METRICS: &[&str] = &[
    "compressed_bytes",
    "compression_ratio",
    "encode_throughput",
    "decode_throughput",
    "peak_rss",
    "codec_owned_bytes",
    "decoder_owned_bytes",
    "allocations",
    "encode_first_output_latency",
    "encode_streaming_latency",
    "parallel_scaling",
    "encode_cycles_per_byte",
    "decode_cycles_per_byte",
    "instructions_per_byte",
    "random_range_latency",
    "range_amplification",
];

/// One metric as a document recorded it: a number with its unit and the call that produced
/// it, or an absence with its reason. There is no third state, and a reason is never a zero.
pub enum Reading {
    Measured {
        value: f64,
        unit: String,
        method: String,
    },
    Unavailable {
        reason: String,
    },
}

impl Reading {
    /// The number this reading carries, or none when the document recorded a reason.
    pub const fn value(&self) -> Option<f64> {
        match self {
            Self::Measured { value, .. } => Some(*value),
            Self::Unavailable { .. } => None,
        }
    }
}

/// One measured row: one competitor, at one operating point, over one corpus entry.
pub struct Row {
    pub codec: String,
    pub display_name: String,
    pub version: String,
    pub operating_point: String,
    /// What the measured bytes were protected by, as the result stated it. A fairness axis:
    /// a codec that checksums less is not faster for free.
    pub integrity: String,
    pub entry: String,
    pub group: String,
    pub class: String,
    pub input_bytes: u64,
    pub input_digest: String,
    pub threads: u64,
    pub round_trip_verified: bool,
    /// The spread the encode samples reported, or none when the row took no encode sample.
    pub encode_spread: Option<f64>,
    /// The spread the decode samples reported, or none when the row took no decode sample.
    pub decode_spread: Option<f64>,
    readings: Vec<(String, Reading)>,
}

impl Row {
    /// What this row recorded for one metric, or none when the metric is not one a result
    /// document carries.
    pub fn reading(&self, metric: &str) -> Option<&Reading> {
        self.readings
            .iter()
            .find(|(name, _)| name == metric)
            .map(|(_, reading)| reading)
    }

    /// How many of this row's metrics carry a number.
    pub fn measured(&self) -> usize {
        self.readings
            .iter()
            .filter(|(_, reading)| reading.value().is_some())
            .count()
    }

    pub const fn readings(&self) -> usize {
        self.readings.len()
    }
}

/// The host a document was produced on, at the depth a comparison has to agree about.
#[derive(Clone)]
pub struct Host {
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub cpu_model: String,
    pub cores: String,
    pub memory: String,
    pub rustc: String,
    pub build_profile: String,
    pub harness_version: String,
    pub counter_backend: String,
    pub counter_granted: bool,
}

/// One result document, read whole.
pub struct Document {
    pub path: PathBuf,
    pub produced_at: String,
    pub segment: String,
    pub tier: String,
    pub tier_is_publication: bool,
    pub tier_licence: String,
    pub codec: String,
    pub size_class: Option<String>,
    /// The operating point group this segment covered, or none when it covered every point
    /// its tier names.
    pub operating_point_group: Option<String>,
    pub entroq_measured: bool,
    pub host: Host,
    pub rows: Vec<Row>,
    pub limits: Vec<String>,
    /// How many object keys were checked against a declared name to read this document.
    pub fields: usize,
}

impl Document {
    pub fn measured(&self) -> usize {
        self.rows.iter().map(Row::measured).sum()
    }

    pub fn readings(&self) -> usize {
        self.rows.iter().map(Row::readings).sum()
    }
}

/// Reads one result document.
///
/// # Errors
///
/// Fails when the file cannot be read, when it is not JSON, when it declares another schema,
/// or when any object in it carries a key this parser does not declare or omits one it does.
pub fn read(path: &Path) -> Result<Document> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::at("read", path, e))?;
    let value: Value = serde_json::from_str(&text).map_err(|e| {
        Error::report(
            format!("{}", path.display()),
            format!("is not a JSON document: {e}"),
        )
    })?;
    document(path, &value)
}

/// Reads one result document that is already in memory.
///
/// `path` names where the document came from, so a rejection says which document it was.
///
/// # Errors
///
/// Fails when the value declares another schema, or when any object in it carries a key this
/// parser does not declare or omits one it does.
pub fn document(path: &Path, value: &Value) -> Result<Document> {
    let mut reader = Reader { path, fields: 0 };
    reader.document(value)
}

/// Walks a document against the shape this parser declares, counting every key it checks.
struct Reader<'a> {
    path: &'a Path,
    fields: usize,
}

impl Reader<'_> {
    fn fail(&self, at: &str, message: &str) -> Error {
        Error::report(
            format!("{} at `{at}`", self.path.display()),
            String::from(message),
        )
    }

    /// Checks one object against the exact set of keys it may carry, and counts them.
    fn object<'v>(
        &mut self,
        value: &'v Value,
        at: &str,
        declared: &[&str],
    ) -> Result<&'v Map<String, Value>> {
        let object = value
            .as_object()
            .ok_or_else(|| self.fail(at, "is not an object"))?;
        for name in declared {
            if !object.contains_key(*name) {
                return Err(self.fail(
                    at,
                    &format!(
                        "carries no `{name}`. A result read in part is a result with a \
                         dropped metric, so it is rejected whole."
                    ),
                ));
            }
        }
        for name in object.keys() {
            if !declared.contains(&name.as_str()) {
                return Err(self.fail(
                    at,
                    &format!(
                        "carries `{name}`, which this parser does not understand. A field it \
                         skipped would be a metric it dropped, so it is rejected whole."
                    ),
                ));
            }
        }
        self.fields = self.fields.saturating_add(object.len());
        Ok(object)
    }

    fn value<'v>(&self, object: &'v Map<String, Value>, at: &str, name: &str) -> Result<&'v Value> {
        object
            .get(name)
            .ok_or_else(|| self.fail(at, &format!("carries no `{name}`")))
    }

    fn text(&self, object: &Map<String, Value>, at: &str, name: &str) -> Result<String> {
        self.value(object, at, name)?
            .as_str()
            .map(String::from)
            .ok_or_else(|| self.fail(at, &format!("`{name}` is not a string")))
    }

    /// Checks a field the writer leaves null when it has nothing to state.
    fn text_or_null(&self, object: &Map<String, Value>, at: &str, name: &str) -> Result<()> {
        let value = self.value(object, at, name)?;
        if value.is_string() || value.is_null() {
            return Ok(());
        }
        Err(self.fail(at, &format!("`{name}` is neither a string nor null")))
    }

    fn optional_text(
        &self,
        object: &Map<String, Value>,
        at: &str,
        name: &str,
    ) -> Result<Option<String>> {
        let value = self.value(object, at, name)?;
        if value.is_null() {
            return Ok(None);
        }
        value
            .as_str()
            .map(|text| Some(String::from(text)))
            .ok_or_else(|| self.fail(at, &format!("`{name}` is neither a string nor null")))
    }

    fn count(&self, object: &Map<String, Value>, at: &str, name: &str) -> Result<u64> {
        self.value(object, at, name)?
            .as_u64()
            .ok_or_else(|| self.fail(at, &format!("`{name}` is not a whole number")))
    }

    fn flag(&self, object: &Map<String, Value>, at: &str, name: &str) -> Result<bool> {
        self.value(object, at, name)?
            .as_bool()
            .ok_or_else(|| self.fail(at, &format!("`{name}` is not a boolean")))
    }

    fn number(&self, object: &Map<String, Value>, at: &str, name: &str) -> Result<f64> {
        self.value(object, at, name)?
            .as_f64()
            .ok_or_else(|| self.fail(at, &format!("`{name}` is not a number")))
    }

    fn array<'v>(
        &self,
        object: &'v Map<String, Value>,
        at: &str,
        name: &str,
    ) -> Result<&'v Vec<Value>> {
        self.value(object, at, name)?
            .as_array()
            .ok_or_else(|| self.fail(at, &format!("`{name}` is not an array")))
    }

    fn boolean_or_null(&self, object: &Map<String, Value>, at: &str, name: &str) -> Result<()> {
        let value = self.value(object, at, name)?;
        if value.is_boolean() || value.is_null() {
            return Ok(());
        }
        Err(self.fail(at, &format!("`{name}` is neither a boolean nor null")))
    }

    fn document(&mut self, value: &Value) -> Result<Document> {
        const AT: &str = "the document";
        let root = self.object(value, AT, DOCUMENT)?;
        let schema = self.text(root, AT, "schema")?;
        if schema != SCHEMA {
            return Err(self.fail(
                AT,
                &format!("declares schema `{schema}`, and this parser reads `{SCHEMA}`"),
            ));
        }

        let entroq = self.object(self.value(root, AT, "entroq")?, "entroq", ENTROQ)?;
        let entroq_measured = self.flag(entroq, "entroq", "measured")?;
        let _ = self.text(entroq, "entroq", "reason")?;

        let host = self.host(root)?;
        self.laboratory(root)?;
        self.inputs(root)?;
        let rows = self.measurements(root)?;
        self.unavailable(root)?;
        let limits = self.limits(root)?;

        Ok(Document {
            path: self.path.to_path_buf(),
            produced_at: self.text(root, AT, "produced_at")?,
            segment: self.text(root, AT, "segment")?,
            tier: self.text(root, AT, "tier")?,
            tier_is_publication: self.flag(root, AT, "tier_is_publication")?,
            tier_licence: self.text(root, AT, "tier_licence")?,
            codec: self.text(root, AT, "codec")?,
            size_class: self.optional_text(root, AT, "size_class")?,
            operating_point_group: self.optional_text(root, AT, "operating_point_group")?,
            entroq_measured,
            host,
            rows,
            limits,
            fields: self.fields,
        })
    }

    fn limits(&self, root: &Map<String, Value>) -> Result<Vec<String>> {
        self.array(root, "the document", "limits")?
            .iter()
            .map(|limit| {
                limit
                    .as_str()
                    .map(String::from)
                    .ok_or_else(|| self.fail("limits", "carries something that is not a string"))
            })
            .collect()
    }

    fn host(&mut self, root: &Map<String, Value>) -> Result<Host> {
        const AT: &str = "environment";
        const COUNTER_AT: &str = "environment.counter_backend";
        let environment = self.object(self.value(root, "the document", AT)?, AT, ENVIRONMENT)?;
        for name in [
            "cpu_features_compiled",
            "cpu_feature_method",
            "frequency_policy",
            "cargo",
            "entroq_revision",
        ] {
            let _ = self.text(environment, AT, name)?;
        }
        self.text_or_null(environment, AT, "encoder_version")?;
        let _ = self.flag(environment, AT, "statistics_enabled")?;

        let counter = self.object(
            self.value(environment, AT, "counter_backend")?,
            COUNTER_AT,
            COUNTER,
        )?;
        self.text_or_null(counter, COUNTER_AT, "reason")?;

        Ok(Host {
            os: self.text(environment, AT, "os")?,
            os_version: self.text(environment, AT, "os_version")?,
            arch: self.text(environment, AT, "arch")?,
            cpu_model: self.text(environment, AT, "cpu_model")?,
            cores: self.text(environment, AT, "cores")?,
            memory: self.text(environment, AT, "memory")?,
            rustc: self.text(environment, AT, "rustc")?,
            build_profile: self.text(environment, AT, "build_profile")?,
            harness_version: self.text(environment, AT, "harness_version")?,
            counter_backend: self.text(counter, COUNTER_AT, "name")?,
            counter_granted: self.flag(counter, COUNTER_AT, "granted")?,
        })
    }

    fn laboratory(&mut self, root: &Map<String, Value>) -> Result<()> {
        const AT: &str = "laboratory";
        const LIBRARY_AT: &str = "laboratory.libraries";
        let laboratory = self.object(self.value(root, "the document", AT)?, AT, LABORATORY)?;
        for name in ["codec", "pinned_version", "upstream_commit", "provenance"] {
            let _ = self.text(laboratory, AT, name)?;
        }
        self.text_or_null(laboratory, AT, "reported_version")?;
        self.boolean_or_null(laboratory, AT, "version_agrees")?;
        for other in self.array(laboratory, AT, "other_codecs_linked")? {
            if !other.is_string() {
                return Err(self.fail(
                    AT,
                    "`other_codecs_linked` holds something that is not a string",
                ));
            }
        }
        for entry in self.array(laboratory, AT, "libraries")? {
            let library = self.object(entry, LIBRARY_AT, LIBRARY)?;
            for name in [
                "codec",
                "version",
                "library",
                "linked_from",
                "digest",
                "note",
            ] {
                let _ = self.text(library, LIBRARY_AT, name)?;
            }
            let _ = self.flag(library, LIBRARY_AT, "matches_manifest")?;
        }
        Ok(())
    }

    fn inputs(&mut self, root: &Map<String, Value>) -> Result<()> {
        const AT: &str = "inputs";
        const ENTRY_AT: &str = "inputs.entries";
        const EXCLUDED_AT: &str = "inputs.excluded";
        let inputs = self.object(self.value(root, "the document", AT)?, AT, INPUTS)?;
        let _ = self.count(inputs, AT, "budget_bytes_per_class")?;
        let _ = self.count(inputs, AT, "selected_bytes")?;
        for value in self.array(inputs, AT, "entries")? {
            let entry = self.object(value, ENTRY_AT, INPUT_ENTRY)?;
            for name in ["name", "group", "content", "class", "digest", "license"] {
                let _ = self.text(entry, ENTRY_AT, name)?;
            }
            let _ = self.count(entry, ENTRY_AT, "bytes")?;
        }
        for value in self.array(inputs, AT, "excluded")? {
            let entry = self.object(value, EXCLUDED_AT, EXCLUDED_ENTRY)?;
            for name in ["name", "class", "reason"] {
                let _ = self.text(entry, EXCLUDED_AT, name)?;
            }
            let _ = self.count(entry, EXCLUDED_AT, "bytes")?;
        }
        Ok(())
    }

    fn unavailable(&mut self, root: &Map<String, Value>) -> Result<()> {
        const AT: &str = "unavailable_metrics";
        for value in self.array(root, "the document", AT)? {
            let absence = self.object(value, AT, UNAVAILABLE)?;
            let metric = self.text(absence, AT, "metric")?;
            let _ = self.text(absence, AT, "reason")?;
            if !METRICS.contains(&metric.as_str()) {
                return Err(self.fail(
                    AT,
                    &format!("names `{metric}`, which is not a metric a result carries"),
                ));
            }
        }
        Ok(())
    }

    fn measurements(&mut self, root: &Map<String, Value>) -> Result<Vec<Row>> {
        let mut rows = Vec::new();
        for value in self.array(root, "the document", "measurements")? {
            rows.push(self.row(value)?);
        }
        Ok(rows)
    }

    fn row(&mut self, value: &Value) -> Result<Row> {
        const AT: &str = "measurements";
        let measurement = self.object(value, AT, MEASUREMENT)?;
        let _ = self.text(measurement, AT, "format")?;
        Ok(Row {
            codec: self.text(measurement, AT, "codec")?,
            display_name: self.text(measurement, AT, "display_name")?,
            version: self.text(measurement, AT, "version")?,
            operating_point: self.text(measurement, AT, "operating_point")?,
            integrity: self.text(measurement, AT, "integrity")?,
            entry: self.text(measurement, AT, "entry")?,
            group: self.text(measurement, AT, "group")?,
            class: self.text(measurement, AT, "class")?,
            input_bytes: self.count(measurement, AT, "input_bytes")?,
            input_digest: self.text(measurement, AT, "input_digest")?,
            threads: self.count(measurement, AT, "threads")?,
            round_trip_verified: self.flag(measurement, AT, "round_trip_verified")?,
            encode_spread: self.sampling(measurement, "encode_samples")?,
            decode_spread: self.sampling(measurement, "decode_samples")?,
            readings: self.readings(measurement)?,
        })
    }

    /// Reads one sampling block and returns the spread it reported, or none when the row
    /// took no sample in that direction.
    fn sampling(&mut self, measurement: &Map<String, Value>, name: &str) -> Result<Option<f64>> {
        const AT: &str = "measurements.samples";
        let value = self.value(measurement, "measurements", name)?;
        if value.is_null() {
            return Ok(None);
        }
        let sampling = self.object(value, AT, SAMPLING)?;
        for field in [
            "samples",
            "repetitions_per_sample",
            "min_ns",
            "median_ns",
            "p95_ns",
            "max_ns",
        ] {
            let _ = self.count(sampling, AT, field)?;
        }
        let _ = self.text(sampling, AT, "spread_definition")?;
        self.text_or_null(sampling, AT, "spread_note")?;
        self.boolean_or_null(sampling, AT, "spread_accepted")?;
        let accepted = self.value(sampling, AT, "accepted_spread")?;
        if !accepted.is_number() && !accepted.is_null() {
            return Err(self.fail(AT, "`accepted_spread` is neither a number nor null"));
        }
        Ok(Some(self.number(sampling, AT, "spread")?))
    }

    fn readings(&mut self, measurement: &Map<String, Value>) -> Result<Vec<(String, Reading)>> {
        const AT: &str = "measurements.metrics";
        let metrics = self.object(
            self.value(measurement, "measurements", "metrics")?,
            AT,
            METRICS,
        )?;
        let mut readings = Vec::with_capacity(METRICS.len());
        for name in METRICS {
            readings.push((String::from(*name), self.reading(metrics, name)?));
        }
        Ok(readings)
    }

    fn reading(&mut self, metrics: &Map<String, Value>, name: &str) -> Result<Reading> {
        let at = format!("measurements.metrics.{name}");
        let value = self.value(metrics, "measurements.metrics", name)?;
        let measured = value
            .as_object()
            .and_then(|metric| metric.get("measured"))
            .and_then(Value::as_bool)
            .ok_or_else(|| self.fail(&at, "carries no `measured` flag"))?;
        if measured {
            let metric = self.object(value, &at, MEASURED_METRIC)?;
            return Ok(Reading::Measured {
                value: self.number(metric, &at, "value")?,
                unit: self.text(metric, &at, "unit")?,
                method: self.text(metric, &at, "method")?,
            });
        }
        let metric = self.object(value, &at, ABSENT_METRIC)?;
        Ok(Reading::Unavailable {
            reason: self.text(metric, &at, "reason")?,
        })
    }
}

#[cfg(test)]
pub mod fixture {
    use serde_json::{Value, json};

    use super::{METRICS, SCHEMA};

    /// One measured metric, as a document carries it.
    pub fn measured(value: f64, unit: &str) -> Value {
        json!({ "measured": true, "value": value, "unit": unit, "method": "a recorded call" })
    }

    /// One metric no number was produced for, as a document carries it.
    pub fn absent(reason: &str) -> Value {
        json!({ "measured": false, "reason": reason })
    }

    /// Every metric a document carries, unavailable, with the named ones replaced.
    pub fn metrics(named: &[(&str, Value)]) -> Value {
        let mut object = serde_json::Map::new();
        for name in METRICS {
            let reading = named.iter().find(|(metric, _)| metric == name).map_or_else(
                || absent("this fixture states no number for it"),
                |(_, reading)| reading.clone(),
            );
            let _ = object.insert(String::from(*name), reading);
        }
        Value::Object(object)
    }

    pub fn samples(spread: f64) -> Value {
        json!({
            "samples": 5,
            "repetitions_per_sample": 1,
            "min_ns": 100,
            "median_ns": 110,
            "p95_ns": 120,
            "max_ns": 120,
            "spread": spread,
            "spread_definition": "the slowest sample minus the fastest, over the median",
            "accepted_spread": 0.35,
            "spread_accepted": true,
            "spread_note": Value::Null,
        })
    }

    /// One measured row over the entry `entry`, carrying the metrics it is given.
    pub fn row(codec: &str, point: &str, entry: &str, metrics: &Value) -> Value {
        json!({
            "codec": codec,
            "display_name": codec,
            "version": "v1",
            "operating_point": point,
            "format": "the fixture format",
            "integrity": "the fixture protects nothing",
            "entry": entry,
            "group": "project",
            "class": "small",
            "input_bytes": 4096,
            "input_digest": "sha256:fixture",
            "threads": 1,
            "round_trip_verified": true,
            "encode_samples": samples(0.0),
            "decode_samples": samples(0.0),
            "metrics": metrics,
        })
    }

    /// A complete document at one tier, carrying the rows it is given.
    pub fn document(tier: &str, rows: &[Value]) -> Value {
        json!({
            "schema": SCHEMA,
            "produced_at": "2026-09-12T00:00:00Z",
            "segment": "bench-fixture",
            "tier": tier,
            "tier_is_publication": tier == "publication",
            "tier_licence": "a fixture states no licence",
            "codec": "fixture",
            "size_class": Value::Null,
            "operating_point_group": Value::Null,
            "entroq": { "measured": false, "reason": "no codec path exists" },
            "environment": environment(),
            "laboratory": laboratory(),
            "inputs": inputs(),
            "measurements": rows,
            "unavailable_metrics": [],
            "limits": ["a fixture licenses nothing"],
        })
    }

    fn environment() -> Value {
        json!({
            "os": "the fixture host",
            "os_version": "0",
            "arch": "aarch64",
            "cpu_model": "a fixture CPU",
            "cpu_features_compiled": "none",
            "cpu_feature_method": "the fixture states them",
            "cores": "1",
            "memory": "1 byte",
            "frequency_policy": "unavailable",
            "rustc": "rustc fixture",
            "cargo": "cargo fixture",
            "build_profile": "release",
            "entroq_revision": "0000000",
            "harness_version": "0.0.0",
            "encoder_version": Value::Null,
            "statistics_enabled": false,
            "counter_backend": {
                "name": "perf_event_open",
                "granted": false,
                "reason": "the fixture host denies it",
            },
        })
    }

    fn laboratory() -> Value {
        json!({
            "codec": "fixture",
            "pinned_version": "v1",
            "upstream_commit": "0000000",
            "reported_version": "1",
            "version_agrees": true,
            "libraries": [{
                "codec": "fixture",
                "version": "v1",
                "library": "lib/libfixture.a",
                "linked_from": "/fixture/lib/libfixture.a",
                "digest": "sha256:fixture",
                "matches_manifest": true,
                "note": "a fixture library",
            }],
            "other_codecs_linked": [],
            "provenance": "a fixture links nothing",
        })
    }

    fn inputs() -> Value {
        json!({
            "budget_bytes_per_class": 4096,
            "selected_bytes": 4096,
            "entries": [{
                "name": "fixture-entry",
                "group": "project",
                "content": "json",
                "class": "small",
                "bytes": 4096,
                "digest": "sha256:fixture",
                "license": "MIT OR Apache-2.0",
            }],
            "excluded": [],
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::{Value, json};

    use super::{METRICS, Reading, SCHEMA, document, fixture};

    fn one() -> Value {
        fixture::document(
            "dev",
            &[fixture::row(
                "lz4",
                "fast-1",
                "fixture-entry",
                &fixture::metrics(&[
                    ("compression_ratio", fixture::measured(2.5, "ratio")),
                    (
                        "encode_throughput",
                        fixture::measured(1.0e9, "bytes per second"),
                    ),
                ]),
            )],
        )
    }

    fn read(value: &Value) -> super::Result<super::Document> {
        document(Path::new("fixture.json"), value)
    }

    fn rejected(value: &Value) -> String {
        read(value).err().map(|e| e.to_string()).unwrap_or_default()
    }

    #[test]
    fn a_complete_document_is_read_whole() {
        let parsed = read(&one());
        let counted = parsed.as_ref().ok().map(|document| document.fields);
        assert!(parsed.is_ok(), "{}", rejected(&one()));
        assert!(counted.is_some_and(|fields| fields > 60), "{counted:?}");
    }

    #[test]
    fn every_reading_of_every_row_is_read() {
        let parsed = read(&one());
        let readings = parsed.as_ref().ok().map(super::Document::readings);
        assert_eq!(readings, Some(METRICS.len()));
        assert_eq!(parsed.as_ref().ok().map(super::Document::measured), Some(2));
    }

    #[test]
    fn a_reading_carries_its_number_or_its_reason_and_never_both() {
        let parsed = read(&one());
        let row = parsed
            .ok()
            .and_then(|document| document.rows.into_iter().next());
        let ratio = row
            .as_ref()
            .and_then(|row| row.reading("compression_ratio"))
            .and_then(Reading::value);
        assert_eq!(ratio, Some(2.5));
        let counter = row
            .as_ref()
            .and_then(|row| row.reading("instructions_per_byte"))
            .and_then(Reading::value);
        assert_eq!(counter, None);
    }

    #[test]
    fn a_field_nobody_declared_is_rejected_rather_than_skipped() {
        let mut value = one();
        if let Some(object) = value.as_object_mut() {
            let _ = object.insert(String::from("throughput_index"), json!(4));
        }
        let message = rejected(&value);
        assert!(message.contains("throughput_index"), "{message}");
        assert!(message.contains("does not understand"), "{message}");
    }

    #[test]
    fn a_field_a_document_owes_is_rejected_when_it_is_missing() {
        let mut value = one();
        if let Some(object) = value.as_object_mut() {
            let _ = object.remove("limits");
        }
        let message = rejected(&value);
        assert!(message.contains("limits"), "{message}");
    }

    #[test]
    fn a_nested_field_nobody_declared_is_rejected_too() {
        let mut value = one();
        let nested = value
            .get_mut("environment")
            .and_then(|environment| environment.get_mut("counter_backend"))
            .and_then(Value::as_object_mut);
        if let Some(object) = nested {
            let _ = object.insert(String::from("cycles"), json!(0));
        }
        let message = rejected(&value);
        assert!(message.contains("cycles"), "{message}");
        assert!(message.contains("counter_backend"), "{message}");
    }

    #[test]
    fn a_metric_nobody_declared_is_rejected_rather_than_read_past() {
        let mut value = one();
        let metrics = value
            .get_mut("measurements")
            .and_then(|rows| rows.get_mut(0))
            .and_then(|row| row.get_mut("metrics"))
            .and_then(Value::as_object_mut);
        if let Some(object) = metrics {
            let _ = object.insert(
                String::from("branch_misses"),
                fixture::measured(1.0, "count"),
            );
        }
        let message = rejected(&value);
        assert!(message.contains("branch_misses"), "{message}");
    }

    #[test]
    fn a_dropped_metric_is_rejected_rather_than_read_as_absent() {
        let mut value = one();
        let metrics = value
            .get_mut("measurements")
            .and_then(|rows| rows.get_mut(0))
            .and_then(|row| row.get_mut("metrics"))
            .and_then(Value::as_object_mut);
        if let Some(object) = metrics {
            let _ = object.remove("decode_throughput");
        }
        let message = rejected(&value);
        assert!(message.contains("decode_throughput"), "{message}");
        assert!(message.contains("dropped metric"), "{message}");
    }

    #[test]
    fn a_measured_metric_that_carries_a_reason_instead_of_a_method_is_rejected() {
        let mut value = one();
        let metrics = value
            .get_mut("measurements")
            .and_then(|rows| rows.get_mut(0))
            .and_then(|row| row.get_mut("metrics"))
            .and_then(Value::as_object_mut);
        if let Some(object) = metrics {
            let _ = object.insert(
                String::from("compression_ratio"),
                json!({ "measured": true, "value": 2.0, "unit": "ratio", "reason": "none" }),
            );
        }
        let message = rejected(&value);
        assert!(message.contains("compression_ratio"), "{message}");
    }

    #[test]
    fn another_schema_is_rejected_by_name() {
        let mut value = one();
        if let Some(object) = value.as_object_mut() {
            let _ = object.insert(String::from("schema"), json!("entroq.bench.result/1"));
        }
        let message = rejected(&value);
        assert!(message.contains("entroq.bench.result/1"), "{message}");
        assert!(message.contains(SCHEMA), "{message}");
    }

    #[test]
    fn an_unavailable_metric_a_document_names_must_be_a_metric_a_result_carries() {
        let mut value = one();
        if let Some(object) = value.as_object_mut() {
            let _ = object.insert(
                String::from("unavailable_metrics"),
                json!([{ "metric": "cache_misses", "reason": "nobody measured it" }]),
            );
        }
        let message = rejected(&value);
        assert!(message.contains("cache_misses"), "{message}");
    }

    #[test]
    fn a_row_that_took_no_sample_reports_no_spread_rather_than_zero() {
        let mut value = one();
        let row = value
            .get_mut("measurements")
            .and_then(|rows| rows.get_mut(0))
            .and_then(Value::as_object_mut);
        if let Some(object) = row {
            let _ = object.insert(String::from("decode_samples"), Value::Null);
        }
        let parsed = read(&value);
        let spread = parsed
            .ok()
            .and_then(|document| document.rows.into_iter().next())
            .map(|row| row.decode_spread);
        assert_eq!(spread, Some(None));
    }

    #[test]
    fn the_declared_metric_set_names_each_metric_once() {
        for (index, name) in METRICS.iter().enumerate() {
            assert!(
                !METRICS
                    .iter()
                    .skip(index.saturating_add(1))
                    .any(|other| other == name),
                "{name} is declared twice"
            );
        }
        assert!(METRICS.contains(&"encode_cycles_per_byte"));
        assert!(METRICS.contains(&"decode_cycles_per_byte"));
        assert!(METRICS.contains(&"instructions_per_byte"));
    }
}
