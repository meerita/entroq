//! Owns the run record: its directory, its manifest, its append-only journal, its raw
//! segment output, and the summary a sealed campaign leaves behind.
//!
//! The journal is append-only here, not by convention. It is opened for append and never for
//! write, truncate, or removal, and no function in this module rewrites or deletes a line. A
//! failed attempt and a later passing attempt are two lines, and both stay.
//!
//! A record lives under the run root, which is outside the repository. This module never
//! writes inside the repository.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::tier::{InputBudget, SEGMENT_BUDGET, Segment, Suite, Tier};
use crate::workspace::{Environment, InputSet, Revision};

const MANIFEST: &str = "manifest.json";
const JOURNAL: &str = "journal.jsonl";
const SEGMENTS: &str = "segments";
const SUMMARY: &str = "summary.md";

/// What one segment attempt did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
    Timeout,
    Cached,
}

impl Status {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Timeout => "timeout",
            Self::Cached => "cached",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "pass" => Some(Self::Pass),
            "fail" => Some(Self::Fail),
            "timeout" => Some(Self::Timeout),
            "cached" => Some(Self::Cached),
            _ => None,
        }
    }

    /// Whether the segment came out of this attempt satisfied.
    ///
    /// A cached attempt reuses a pass that was measured at the same revision and the same
    /// inputs, so it counts as satisfied and states what it reused.
    pub const fn is_satisfied(self) -> bool {
        matches!(self, Self::Pass | Self::Cached)
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One journal line: one attempt at one segment.
pub struct Entry {
    pub segment: String,
    pub attempt: u32,
    pub started: String,
    pub duration_s: f64,
    pub status: Status,
    pub revision: String,
    pub input_digest: String,
    pub evidence: String,
    pub note: Option<String>,
}

impl Entry {
    fn to_json(&self) -> Value {
        let mut value = json!({
            "segment": self.segment,
            "attempt": self.attempt,
            "started": self.started,
            "duration_s": self.duration_s,
            "status": self.status.as_str(),
            "revision": self.revision,
            "input_digest": self.input_digest,
            "evidence": self.evidence,
        });
        if let (Some(note), Some(object)) = (self.note.as_ref(), value.as_object_mut()) {
            let _ = object.insert(String::from("note"), Value::String(note.clone()));
        }
        value
    }

    fn from_json(value: &Value, line: usize) -> Result<Self> {
        let at =
            |field: &str| Error::record(format!("journal line {line}"), format!("has no {field}"));
        let text = |field: &str| -> Result<String> {
            value
                .get(field)
                .and_then(Value::as_str)
                .map(String::from)
                .ok_or_else(|| at(field))
        };
        let status_name = text("status")?;
        let status = Status::parse(&status_name).ok_or_else(|| {
            Error::record(
                format!("journal line {line}"),
                format!("has unknown status `{status_name}`"),
            )
        })?;
        Ok(Self {
            segment: text("segment")?,
            attempt: value
                .get("attempt")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| at("attempt"))?,
            started: text("started")?,
            duration_s: value
                .get("duration_s")
                .and_then(Value::as_f64)
                .ok_or_else(|| at("duration_s"))?,
            status,
            revision: text("revision")?,
            input_digest: text("input_digest")?,
            evidence: text("evidence")?,
            note: value.get("note").and_then(Value::as_str).map(String::from),
        })
    }
}

/// What a campaign was asked to do, written before its first segment runs.
pub struct Manifest<'a> {
    pub campaign: &'a str,
    pub tier: Tier,
    pub suite: Suite,
    pub topic: &'a str,
    pub created: &'a str,
    pub revision: &'a Revision,
    pub coverage: &'a str,
    pub segments: &'a [Segment],
    pub inputs: &'a InputSet,
    pub environment: &'a Environment,
}

impl Manifest<'_> {
    fn to_json(&self) -> Value {
        let segments = self
            .segments
            .iter()
            .map(|segment| {
                json!({
                    "id": segment.id,
                    "description": segment.description,
                    "steps": segment
                        .steps
                        .iter()
                        .map(|step| {
                            let mut argv = vec![Value::from(step.program)];
                            argv.extend(step.args.iter().map(|arg| Value::from(*arg)));
                            Value::Array(argv)
                        })
                        .collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>();
        let input_budget = match self.tier.input_budget() {
            InputBudget::Bytes(bytes) => Value::from(bytes),
            InputBudget::FullCorpora => Value::from("full corpora"),
        };
        json!({
            "campaign": self.campaign,
            "tier": self.tier.name(),
            "suite": self.suite.name(),
            "topic": self.topic,
            "created": self.created,
            "revision": {
                "commit": self.revision.commit,
                "short": self.revision.short(),
                "dirty": self.revision.dirty,
            },
            "encoder_version": Value::Null,
            "coverage": self.coverage,
            "segments": segments,
            "input_set": {
                "description": self.inputs.description,
                "file_count": self.inputs.file_count,
                "byte_count": self.inputs.byte_count,
                "digest": self.inputs.digest,
            },
            "budget": {
                "segment_seconds": SEGMENT_BUDGET.as_secs(),
                "campaign_seconds": self.tier.campaign_budget().map(|d| d.as_secs()),
                "input": input_budget,
            },
            "environment": {
                "os": self.environment.os,
                "arch": self.environment.arch,
                "host": self.environment.host,
                "rustc": self.environment.rustc,
                "cargo": self.environment.cargo,
                "runner": self.environment.runner,
            },
            "authorization": Value::Null,
        })
    }
}

/// What one fuzz invocation was asked to do, written before its segment runs.
///
/// A fuzz campaign is one segment over one target, so its manifest states the target, the
/// corpus the segment continues from, and what earlier segments already spent on that
/// target. Coverage comes from many bounded segments across many days, so the accumulated
/// figure is the one a reader needs.
pub struct FuzzManifest<'a> {
    pub campaign: &'a str,
    pub tier: Tier,
    pub topic: &'a str,
    pub created: &'a str,
    pub revision: &'a Revision,
    pub target: &'a str,
    pub covers: &'a str,
    pub coverage: &'a str,
    pub segment: &'a str,
    pub description: &'a str,
    pub steps: &'a [Vec<String>],
    pub corpus: &'a Path,
    pub artifacts: &'a Path,
    pub corpus_files: usize,
    pub prior_segments: usize,
    pub prior_seconds: f64,
    pub fuzz_seconds: u64,
    pub inputs: &'a InputSet,
    pub environment: &'a Environment,
}

impl FuzzManifest<'_> {
    fn to_json(&self) -> Value {
        json!({
            "campaign": self.campaign,
            "tier": self.tier.name(),
            "suite": "fuzz",
            "topic": self.topic,
            "created": self.created,
            "revision": {
                "commit": self.revision.commit,
                "short": self.revision.short(),
                "dirty": self.revision.dirty,
            },
            "encoder_version": Value::Null,
            "coverage": self.coverage,
            "target": {
                "name": self.target,
                "covers": self.covers,
            },
            "segments": [{
                "id": self.segment,
                "description": self.description,
                "steps": self.steps.iter().map(|argv| {
                    Value::Array(argv.iter().map(|arg| Value::from(arg.as_str())).collect())
                }).collect::<Vec<_>>(),
            }],
            "corpus": {
                "directory": self.corpus.display().to_string(),
                "artifact_directory": self.artifacts.display().to_string(),
                "files_before": self.corpus_files,
            },
            "accumulated_before": {
                "segments": self.prior_segments,
                "seconds": self.prior_seconds,
            },
            "input_set": {
                "description": self.inputs.description,
                "file_count": self.inputs.file_count,
                "byte_count": self.inputs.byte_count,
                "digest": self.inputs.digest,
            },
            "budget": {
                "segment_seconds": SEGMENT_BUDGET.as_secs(),
                "campaign_seconds": Value::Null,
                "fuzz_seconds": self.fuzz_seconds,
                "input": "the accumulated corpus, plus what libFuzzer derives from it",
            },
            "environment": {
                "os": self.environment.os,
                "arch": self.environment.arch,
                "host": self.environment.host,
                "rustc": self.environment.rustc,
                "cargo": self.environment.cargo,
                "runner": self.environment.runner,
            },
            "authorization": Value::Null,
        })
    }
}

/// One campaign's record directory.
pub struct Record {
    dir: PathBuf,
    campaign: String,
}

impl Record {
    /// Starts a new campaign record under `<runs>/<tier>/<date>-<NN>-<topic>/`.
    ///
    /// # Errors
    ///
    /// Fails when the run root cannot be created or a record already holds the chosen name.
    pub fn create(runs: &Path, tier: Tier, date: &str, topic: &str) -> Result<Self> {
        let tier_dir = runs.join(tier.name());
        fs::create_dir_all(&tier_dir).map_err(|e| Error::at("create", &tier_dir, e))?;
        let sequence = next_sequence(&tier_dir, date)?;
        let campaign = format!("{date}-{sequence:02}-{topic}");
        let dir = tier_dir.join(&campaign);
        fs::create_dir(&dir).map_err(|e| Error::at("create", &dir, e))?;
        Ok(Self { dir, campaign })
    }

    /// Every campaign record for a topic at a tier, oldest first.
    ///
    /// A tier directory holds every campaign that ran at that tier, and more than one suite
    /// can run at one tier, so the topic is what selects the campaigns of one activity.
    ///
    /// # Errors
    ///
    /// Fails when the tier directory cannot be read.
    pub fn all(runs: &Path, tier: Tier, topic: &str) -> Result<Vec<Self>> {
        let tier_dir = runs.join(tier.name());
        if !tier_dir.is_dir() {
            return Ok(Vec::new());
        }
        let suffix = format!("-{topic}");
        let mut names = Vec::new();
        for entry in fs::read_dir(&tier_dir).map_err(|e| Error::at("read", &tier_dir, e))? {
            let entry = entry.map_err(|e| Error::at("read", &tier_dir, e))?;
            if entry.path().is_dir()
                && let Some(name) = entry.file_name().to_str()
                && name.ends_with(&suffix)
            {
                names.push(String::from(name));
            }
        }
        names.sort();
        Ok(names
            .into_iter()
            .map(|campaign| Self {
                dir: tier_dir.join(&campaign),
                campaign,
            })
            .collect())
    }

    /// Opens the most recent campaign record for a topic at a tier.
    ///
    /// # Errors
    ///
    /// Fails when the tier directory cannot be read.
    pub fn latest(runs: &Path, tier: Tier, topic: &str) -> Result<Option<Self>> {
        Ok(Self::all(runs, tier, topic)?.pop())
    }

    pub fn campaign(&self) -> &str {
        &self.campaign
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes the manifest. A campaign writes it once, before its first segment runs.
    ///
    /// # Errors
    ///
    /// Fails when the manifest cannot be written.
    pub fn write_manifest(&self, manifest: &Manifest<'_>) -> Result<()> {
        self.write_json(&manifest.to_json())
    }

    /// Writes the manifest of a fuzz campaign.
    ///
    /// # Errors
    ///
    /// Fails when the manifest cannot be written.
    pub fn write_fuzz_manifest(&self, manifest: &FuzzManifest<'_>) -> Result<()> {
        self.write_json(&manifest.to_json())
    }

    fn write_json(&self, value: &Value) -> Result<()> {
        let path = self.dir.join(MANIFEST);
        let text = serde_json::to_string_pretty(value)
            .map_err(|e| Error::record(MANIFEST, e.to_string()))?;
        fs::write(&path, format!("{text}\n")).map_err(|e| Error::at("write", &path, e))
    }

    /// Appends one line to the journal, when the segment ends.
    ///
    /// This is the only function that writes to the journal, and it opens the file for
    /// append only.
    ///
    /// # Errors
    ///
    /// Fails when the journal cannot be opened or extended.
    pub fn append(&self, entry: &Entry) -> Result<()> {
        let path = self.dir.join(JOURNAL);
        let line = serde_json::to_string(&entry.to_json())
            .map_err(|e| Error::record(JOURNAL, e.to_string()))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| Error::at("open", &path, e))?;
        file.write_all(format!("{line}\n").as_bytes())
            .map_err(|e| Error::at("append to", &path, e))
    }

    /// Reads every journal line, oldest first.
    ///
    /// # Errors
    ///
    /// Fails when the journal exists but a line does not parse.
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let path = self.dir.join(JOURNAL);
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(&path).map_err(|e| Error::at("read", &path, e))?;
        let mut entries = Vec::new();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let number = index.saturating_add(1);
            let value: Value = serde_json::from_str(line)
                .map_err(|e| Error::record(format!("journal line {number}"), e.to_string()))?;
            entries.push(Entry::from_json(&value, number)?);
        }
        Ok(entries)
    }

    /// The directory one attempt writes its raw output to, relative to the record.
    pub fn attempt_path(segment: &str, attempt: u32) -> String {
        format!("{SEGMENTS}/{segment}/attempt-{attempt:02}")
    }

    pub fn resolve(&self, relative: &str) -> PathBuf {
        self.dir.join(relative)
    }

    /// Writes the summary. A campaign writes it when it seals.
    ///
    /// # Errors
    ///
    /// Fails when the summary cannot be written.
    pub fn write_summary(&self, text: &str) -> Result<()> {
        let path = self.dir.join(SUMMARY);
        fs::write(&path, text).map_err(|e| Error::at("write", &path, e))
    }
}

fn next_sequence(tier_dir: &Path, date: &str) -> Result<u32> {
    let prefix = format!("{date}-");
    let mut highest = 0;
    for entry in fs::read_dir(tier_dir).map_err(|e| Error::at("read", tier_dir, e))? {
        let entry = entry.map_err(|e| Error::at("read", tier_dir, e))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some(sequence) = rest.split('-').next().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        highest = highest.max(sequence);
    }
    Ok(highest.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::{Entry, Record, Status};
    use crate::tier::Tier;
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("entroq-run-record-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(std::fs::create_dir_all(&dir).is_ok());
        dir
    }

    fn entry(segment: &str, attempt: u32, status: Status, revision: &str) -> Entry {
        Entry {
            segment: String::from(segment),
            attempt,
            started: String::from("2026-09-11T20:00:00Z"),
            duration_s: 1.5,
            status,
            revision: String::from(revision),
            input_digest: String::from("sha256:abc"),
            evidence: Record::attempt_path(segment, attempt),
            note: None,
        }
    }

    fn create(runs: &Path, date: &str) -> Record {
        create_topic(runs, date, "workspace-gate")
    }

    fn create_topic(runs: &Path, date: &str, topic: &str) -> Record {
        let record = Record::create(runs, Tier::Gate, date, topic);
        assert!(record.is_ok());
        record.unwrap_or_else(|_| Record {
            dir: PathBuf::new(),
            campaign: String::new(),
        })
    }

    #[test]
    fn a_record_is_named_by_date_sequence_and_topic() {
        let runs = scratch("naming");
        let record = create(&runs, "2026-09-11");
        assert_eq!(record.campaign(), "2026-09-11-01-workspace-gate");
    }

    #[test]
    fn a_second_campaign_on_one_date_takes_the_next_sequence() {
        let runs = scratch("sequence");
        let _first = create(&runs, "2026-09-11");
        let second = create(&runs, "2026-09-11");
        assert_eq!(second.campaign(), "2026-09-11-02-workspace-gate");
    }

    #[test]
    fn a_new_date_restarts_the_sequence() {
        let runs = scratch("new-date");
        let _first = create(&runs, "2026-09-11");
        let next = create(&runs, "2026-09-12");
        assert_eq!(next.campaign(), "2026-09-12-01-workspace-gate");
    }

    #[test]
    fn the_latest_record_is_the_most_recent_campaign() {
        let runs = scratch("latest");
        let _first = create(&runs, "2026-09-11");
        let _second = create(&runs, "2026-09-11");
        let third = create(&runs, "2026-09-12");
        let latest = Record::latest(&runs, Tier::Gate, "workspace-gate");
        assert!(latest.is_ok());
        let name = latest.ok().flatten().map(|r| String::from(r.campaign()));
        assert_eq!(name.as_deref(), Some(third.campaign()));
    }

    #[test]
    fn no_record_means_no_latest_campaign() {
        let runs = scratch("empty");
        let latest = Record::latest(&runs, Tier::Gate, "workspace-gate");
        assert!(matches!(latest, Ok(None)));
    }

    #[test]
    fn a_resume_finds_the_campaign_of_its_own_topic_not_the_newest_of_the_tier() {
        let runs = scratch("topic");
        let workspace = create_topic(&runs, "2026-09-11", "workspace-gate");
        let lab = create_topic(&runs, "2026-09-12", "lab-build");
        let found = Record::latest(&runs, Tier::Gate, "workspace-gate")
            .ok()
            .flatten()
            .map(|record| String::from(record.campaign()));
        assert_eq!(found.as_deref(), Some(workspace.campaign()));
        let found = Record::latest(&runs, Tier::Gate, "lab-build")
            .ok()
            .flatten()
            .map(|record| String::from(record.campaign()));
        assert_eq!(found.as_deref(), Some(lab.campaign()));
    }

    #[test]
    fn every_campaign_of_a_topic_is_listed_oldest_first() {
        let runs = scratch("all");
        let first = create_topic(&runs, "2026-09-11", "fuzz-mechanism");
        let second = create_topic(&runs, "2026-09-12", "fuzz-mechanism");
        let _other = create_topic(&runs, "2026-09-12", "workspace-gate");
        let found = Record::all(&runs, Tier::Gate, "fuzz-mechanism").unwrap_or_default();
        let names: Vec<&str> = found.iter().map(Record::campaign).collect();
        assert_eq!(names, vec![first.campaign(), second.campaign()]);
    }

    #[test]
    fn a_topic_with_no_campaign_has_nothing_to_resume() {
        let runs = scratch("topic-absent");
        let _existing = create_topic(&runs, "2026-09-11", "workspace-gate");
        let found = Record::latest(&runs, Tier::Gate, "lab-build");
        assert!(matches!(found, Ok(None)));
    }

    #[test]
    fn the_journal_starts_empty_and_grows_by_appending() {
        let runs = scratch("journal");
        let record = create(&runs, "2026-09-11");
        assert!(matches!(record.entries().as_deref(), Ok([])));
        assert!(
            record
                .append(&entry("workspace-fmt", 1, Status::Fail, "aaa"))
                .is_ok()
        );
        assert!(
            record
                .append(&entry("workspace-fmt", 2, Status::Pass, "aaa"))
                .is_ok()
        );
        let entries = record.entries();
        assert!(entries.is_ok());
        let entries = entries.unwrap_or_default();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries.first().map(|e| e.status), Some(Status::Fail));
        assert_eq!(entries.get(1).map(|e| e.status), Some(Status::Pass));
    }

    #[test]
    fn a_journal_line_round_trips_every_field() {
        let runs = scratch("round-trip");
        let record = create(&runs, "2026-09-11");
        let mut written = entry("workspace-test", 3, Status::Timeout, "beef");
        written.note = Some(String::from("`cargo test` exceeded the segment budget"));
        assert!(record.append(&written).is_ok());
        let entries = record.entries().unwrap_or_default();
        let read = entries.first();
        assert_eq!(read.map(|e| e.segment.as_str()), Some("workspace-test"));
        assert_eq!(read.map(|e| e.attempt), Some(3));
        assert_eq!(read.map(|e| e.status), Some(Status::Timeout));
        assert_eq!(read.map(|e| e.revision.as_str()), Some("beef"));
        assert_eq!(read.map(|e| e.input_digest.as_str()), Some("sha256:abc"));
        assert_eq!(
            read.map(|e| e.evidence.as_str()),
            Some("segments/workspace-test/attempt-03")
        );
        assert_eq!(
            read.and_then(|e| e.note.as_deref()),
            written.note.as_deref()
        );
    }

    #[test]
    fn a_passing_line_carries_no_note() {
        let runs = scratch("no-note");
        let record = create(&runs, "2026-09-11");
        assert!(
            record
                .append(&entry("workspace-fmt", 1, Status::Pass, "aaa"))
                .is_ok()
        );
        let entries = record.entries().unwrap_or_default();
        assert_eq!(entries.first().and_then(|e| e.note.as_deref()), None);
    }

    #[test]
    fn a_malformed_journal_line_is_reported_not_skipped() {
        let runs = scratch("malformed");
        let record = create(&runs, "2026-09-11");
        assert!(
            record
                .append(&entry("workspace-fmt", 1, Status::Pass, "aaa"))
                .is_ok()
        );
        let path = record.dir().join("journal.jsonl");
        let appended = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, b"{\"segment\":\"x\"}\n"));
        assert!(appended.is_ok());
        assert!(record.entries().is_err());
    }

    #[test]
    fn a_summary_lands_beside_the_journal() {
        let runs = scratch("seal");
        let record = create(&runs, "2026-09-11");
        let summary = record.dir().join("summary.md");
        assert!(!summary.is_file());
        assert!(record.write_summary("# sealed\n").is_ok());
        assert!(summary.is_file());
    }

    #[test]
    fn only_a_pass_or_a_reuse_satisfies_a_segment() {
        assert!(Status::Pass.is_satisfied());
        assert!(Status::Cached.is_satisfied());
        assert!(!Status::Fail.is_satisfied());
        assert!(!Status::Timeout.is_satisfied());
    }
}
