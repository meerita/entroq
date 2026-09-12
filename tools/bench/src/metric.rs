//! Owns what a metric is in a result: a number with a unit and the method that produced it,
//! or an absence with the reason for it.
//!
//! There is no third state. A metric no host, no library, or no codec path can produce is
//! written as unavailable with its reason, never as a zero, an empty field, or an omitted
//! key. A reader who sees a number can act on it, and a reader who sees a reason knows what
//! would have to change.
//!
//! This module does not own which metrics exist or how any of them is measured.

use serde_json::{Value, json};

/// One metric, as a result carries it.
pub enum Metric {
    Measured {
        value: Value,
        unit: &'static str,
        method: String,
    },
    Unavailable {
        reason: String,
    },
}

impl Metric {
    /// A counted quantity.
    pub fn count(value: u64, unit: &'static str, method: impl Into<String>) -> Self {
        Self::Measured {
            value: Value::from(value),
            unit,
            method: method.into(),
        }
    }

    /// A continuous quantity, such as a rate or a ratio.
    pub fn rate(value: f64, unit: &'static str, method: impl Into<String>) -> Self {
        Self::Measured {
            value: Value::from(value),
            unit,
            method: method.into(),
        }
    }

    pub fn absent(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }

    pub const fn is_measured(&self) -> bool {
        matches!(self, Self::Measured { .. })
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::Measured {
                value,
                unit,
                method,
            } => json!({
                "measured": true,
                "value": value,
                "unit": unit,
                "method": method,
            }),
            Self::Unavailable { reason } => json!({
                "measured": false,
                "reason": reason,
            }),
        }
    }
}

/// A metric and the name a result files it under, kept in declaration order.
pub struct Set {
    entries: Vec<(&'static str, Metric)>,
}

impl Set {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn add(&mut self, name: &'static str, metric: Metric) {
        self.entries.push((name, metric));
    }

    /// How many of the metrics in this set carry a number.
    pub fn measured(&self) -> usize {
        self.entries
            .iter()
            .filter(|(_, metric)| metric.is_measured())
            .count()
    }

    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Every metric this set carries that no number could be produced for.
    pub fn unavailable(&self) -> Vec<(&'static str, &str)> {
        self.entries
            .iter()
            .filter_map(|(name, metric)| match metric {
                Metric::Measured { .. } => None,
                Metric::Unavailable { reason } => Some((*name, reason.as_str())),
            })
            .collect()
    }

    pub fn to_json(&self) -> Value {
        let mut object = serde_json::Map::new();
        for (name, metric) in &self.entries {
            let _ = object.insert(String::from(*name), metric.to_json());
        }
        Value::Object(object)
    }
}

#[cfg(test)]
mod tests {
    use super::{Metric, Set};

    #[test]
    fn a_measured_metric_carries_its_value_unit_and_method() {
        let json = Metric::count(1024, "bytes", "ZSTD_sizeof_CCtx").to_json();
        assert_eq!(
            json.get("measured").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            json.get("value").and_then(serde_json::Value::as_u64),
            Some(1024)
        );
        assert_eq!(
            json.get("unit").and_then(serde_json::Value::as_str),
            Some("bytes")
        );
        assert_eq!(
            json.get("method").and_then(serde_json::Value::as_str),
            Some("ZSTD_sizeof_CCtx")
        );
    }

    #[test]
    fn an_unavailable_metric_carries_a_reason_and_no_value() {
        let json = Metric::absent("the host denies the counter").to_json();
        assert_eq!(
            json.get("measured").and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert!(json.get("value").is_none());
        assert!(
            json.get("reason")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|reason| reason.contains("denies"))
        );
    }

    #[test]
    fn an_unavailable_metric_is_never_a_zero() {
        let metric = Metric::absent("no codec path produces it");
        assert!(!metric.is_measured());
        let json = metric.to_json();
        assert_ne!(
            json.get("value").and_then(serde_json::Value::as_u64),
            Some(0)
        );
    }

    #[test]
    fn a_set_keeps_the_order_it_was_declared_in() {
        let mut set = Set::new();
        set.add("compressed_bytes", Metric::count(1, "bytes", "m"));
        set.add("compression_ratio", Metric::rate(2.0, "ratio", "m"));
        let json = set.to_json();
        let keys: Vec<&String> = json
            .as_object()
            .map(|o| o.keys().collect())
            .unwrap_or_default();
        assert_eq!(keys.len(), 2);
        assert_eq!(set.len(), 2);
        assert_eq!(set.measured(), 2);
    }

    #[test]
    fn a_set_names_every_metric_it_could_not_produce() {
        let mut set = Set::new();
        set.add("compressed_bytes", Metric::count(1, "bytes", "m"));
        set.add(
            "instructions_per_byte",
            Metric::absent("the host denies the counter"),
        );
        let missing = set.unavailable();
        assert_eq!(set.measured(), 1);
        assert_eq!(missing.len(), 1);
        assert_eq!(
            missing.first().map(|(name, _)| *name),
            Some("instructions_per_byte")
        );
    }
}
