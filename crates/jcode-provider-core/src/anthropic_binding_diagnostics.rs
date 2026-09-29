//! Process-wide counts of Anthropic reasoning-binding events.
//!
//! Every `input_transformations` entry a response reports, and every replayed
//! thinking block jcode's own check finds invalid before sending, is counted
//! here and logged by the Anthropic runtime. Under INT-01 INV-1 a
//! prefix-binding mismatch means jcode edited history it should only have
//! appended to, so a non-zero count is a defect signal. The counts are
//! diagnostics; nothing reads them to change behavior.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// Source of a counted event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BindingEventSource {
    /// Reported by the provider in `input_transformations`.
    Provider,
    /// Found by jcode's own check of the request before sending it.
    LocalCheck,
}

/// One counted event kind with its total.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingEventCount {
    pub source: BindingEventSource,
    /// `input_transformations` type, or the local validity label.
    pub kind: String,
    /// `input_transformations` reason; empty for local checks.
    pub reason: String,
    pub count: u64,
}

type Key = (BindingEventSource, String, String);

static COUNTS: Mutex<BTreeMap<Key, u64>> = Mutex::new(BTreeMap::new());

fn record(source: BindingEventSource, kind: &str, reason: &str) {
    let mut counts = COUNTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *counts
        .entry((source, kind.to_string(), reason.to_string()))
        .or_default() += 1;
}

/// Count one `input_transformations` entry.
pub fn record_input_transformation(kind: &str, reason: &str) {
    record(BindingEventSource::Provider, kind, reason);
}

/// Count one replayed block jcode's check found invalid before sending.
pub fn record_local_invalid_replay(validity: &str) {
    record(BindingEventSource::LocalCheck, validity, "");
}

/// Every counted event kind, in a stable order.
pub fn snapshot() -> Vec<BindingEventCount> {
    COUNTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .map(|((source, kind, reason), count)| BindingEventCount {
            source: *source,
            kind: kind.clone(),
            reason: reason.clone(),
            count: *count,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_counted_by_source_kind_and_reason() {
        let before = |kind: &str| {
            snapshot()
                .into_iter()
                .find(|entry| entry.kind == kind)
                .map_or(0, |entry| entry.count)
        };
        let dropped = before("thinking_dropped_test_kind");
        record_input_transformation("thinking_dropped_test_kind", "prefix_binding_mismatch");
        record_input_transformation("thinking_dropped_test_kind", "prefix_binding_mismatch");
        record_local_invalid_replay("prefix_changed_test_kind");
        assert_eq!(before("thinking_dropped_test_kind"), dropped + 2);
        assert!(snapshot().iter().any(|entry| {
            entry.source == BindingEventSource::LocalCheck
                && entry.kind == "prefix_changed_test_kind"
                && entry.reason.is_empty()
        }));
    }
}
