//! Scoped in-process execution liveness. Both a Registry dispatch and its
//! surviving execution supervisor hold guards. Keys are durable invocation IDs,
//! never raw provider tool IDs shared by unrelated sessions/messages.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// Reference counts per invocation_id. A count (rather than a set) keeps the
/// registry correct if the same id is somehow executed concurrently, e.g. a
/// retry racing the original.
static IN_FLIGHT: LazyLock<Mutex<HashMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// RAII registration for one executing tool call.
pub struct InFlightToolGuard {
    invocation_id: String,
}

impl Drop for InFlightToolGuard {
    fn drop(&mut self) {
        let Ok(mut map) = IN_FLIGHT.lock() else {
            return;
        };
        if let Some(count) = map.get_mut(&self.invocation_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                map.remove(&self.invocation_id);
            }
        }
    }
}

/// Mark `invocation_id` as executing until the returned guard is dropped.
/// Empty ids are not tracked (nothing can match them during repair).
pub fn mark_tool_in_flight(invocation: &crate::execution::Invocation) -> Option<InFlightToolGuard> {
    if invocation.session_id.is_empty()
        || invocation.message_id.is_empty()
        || invocation.call_path.is_empty()
        || invocation.call_path.iter().any(String::is_empty)
    {
        return None;
    }
    let invocation_id = invocation.id();
    let mut map = IN_FLIGHT.lock().ok()?;
    *map.entry(invocation_id.to_string()).or_insert(0) += 1;
    Some(InFlightToolGuard {
        invocation_id: invocation_id.to_string(),
    })
}

/// True while a tool call with this id is executing somewhere in this process.
pub fn is_tool_in_flight(invocation: &crate::execution::Invocation) -> bool {
    let invocation_id = invocation.id();
    IN_FLIGHT
        .lock()
        .map(|map| map.contains_key(&invocation_id))
        .unwrap_or(false)
}

/// Number of tool calls currently executing (diagnostics only).
pub fn in_flight_tool_count() -> usize {
    IN_FLIGHT.lock().map(|map| map.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn invocation(id: &str) -> crate::execution::Invocation {
        crate::execution::Invocation {
            session_id: "session".into(),
            message_id: "message".into(),
            call_path: vec![id.into()],
            tool: "fixture".into(),
            input: serde_json::Value::Null,
            working_dir: None,
            received_result_digest: None,
        }
    }

    #[test]
    fn guard_tracks_and_releases() {
        let id = &invocation("toolu_inflight_test_basic");
        assert!(!is_tool_in_flight(id));
        {
            let _guard = mark_tool_in_flight(id).expect("guard");
            assert!(is_tool_in_flight(id));
        }
        assert!(!is_tool_in_flight(id));
    }

    #[test]
    fn nested_guards_release_only_after_the_last_one() {
        let id = &invocation("toolu_inflight_test_nested");
        let outer = mark_tool_in_flight(id).expect("guard");
        let inner = mark_tool_in_flight(id).expect("guard");
        assert!(is_tool_in_flight(id));
        drop(inner);
        assert!(is_tool_in_flight(id), "one registration still outstanding");
        drop(outer);
        assert!(!is_tool_in_flight(id));
    }

    #[test]
    fn empty_ids_are_not_tracked() {
        assert!(mark_tool_in_flight(&invocation("")).is_none());
        assert!(!is_tool_in_flight(&invocation("")));
    }

    #[test]
    fn identical_provider_ids_in_other_scopes_do_not_share_liveness() {
        let original = invocation("same");
        let _guard = mark_tool_in_flight(&original).unwrap();
        let mut other = original.clone();
        other.session_id = "other".into();
        assert!(!is_tool_in_flight(&other));
        let mut other = original.clone();
        other.message_id = "other".into();
        assert!(!is_tool_in_flight(&other));
        let mut other = original.clone();
        other.call_path.insert(0, "parent".into());
        assert!(!is_tool_in_flight(&other));
        assert!(is_tool_in_flight(&original));
    }
}
