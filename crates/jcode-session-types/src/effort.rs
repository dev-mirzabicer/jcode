//! What a session asked for as its reasoning effort (INT-01/WP-06 R24).
//!
//! The effective effort depends on the model: a model's default differs from
//! another's, and a level one model offers another may not. A session
//! therefore stores what was asked, and the runtime resolves the effective
//! value for the current model on every request. Storing a resolved default
//! as if it had been chosen would carry one model's default to the next.

use serde::{Deserialize, Serialize};

/// A session's reasoning-effort intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredReasoningEffortIntent {
    /// Follow the runtime's default for whichever model is current: its
    /// configured default, else the model's own.
    Default,
    /// A level a person or caller chose. It is applied again after every
    /// model switch, so a model that lacks it only narrows it while that
    /// model is current.
    Explicit { level: String },
}

impl StoredReasoningEffortIntent {
    /// The intent a request for `requested` expresses: nothing, `default` or
    /// `auto` asks for the default, anything else names a level.
    pub fn from_request(requested: &str) -> Self {
        let requested = requested.trim().to_ascii_lowercase();
        if requested.is_empty() || matches!(requested.as_str(), "default" | "auto") {
            Self::Default
        } else {
            Self::Explicit { level: requested }
        }
    }

    /// The intent of a session stored before intents existed, from the
    /// effort string it kept and the runtime's default for its current
    /// model. A stored value equal to that default was most likely the
    /// resolved default, so it becomes `Default`; any other value was chosen
    /// and stays `Explicit`. Either way the session's effective effort is
    /// the one it had.
    pub fn migrated(stored: Option<&str>, runtime_default: Option<&str>) -> Self {
        match stored {
            Some(stored) if Some(stored) != runtime_default => Self::Explicit {
                level: stored.to_string(),
            },
            _ => Self::Default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StoredReasoningEffortIntent as Intent;

    #[test]
    fn a_request_names_a_level_or_asks_for_the_default() {
        for requested in ["", "  ", "default", "Default", "auto"] {
            assert_eq!(Intent::from_request(requested), Intent::Default);
        }
        assert_eq!(
            Intent::from_request(" XHigh "),
            Intent::Explicit {
                level: "xhigh".to_string()
            }
        );
        assert_eq!(
            Intent::from_request("none"),
            Intent::Explicit {
                level: "none".to_string()
            }
        );
    }

    #[test]
    fn an_old_stored_effort_migrates_without_changing_the_effective_value() {
        assert_eq!(Intent::migrated(None, Some("low")), Intent::Default);
        assert_eq!(Intent::migrated(Some("low"), Some("low")), Intent::Default);
        assert_eq!(
            Intent::migrated(Some("low"), Some("high")),
            Intent::Explicit {
                level: "low".to_string()
            }
        );
        assert_eq!(
            Intent::migrated(Some("high"), None),
            Intent::Explicit {
                level: "high".to_string()
            }
        );
    }

    #[test]
    fn the_intent_round_trips() {
        for intent in [
            Intent::Default,
            Intent::Explicit {
                level: "max".to_string(),
            },
        ] {
            let json = serde_json::to_string(&intent).unwrap();
            assert_eq!(serde_json::from_str::<Intent>(&json).unwrap(), intent);
        }
    }
}
