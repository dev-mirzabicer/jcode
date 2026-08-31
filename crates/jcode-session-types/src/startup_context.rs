use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Stable project identity persisted by Startup Context plans and session receipts.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredStartupProjectIdentity {
    Git { canonical_common_dir: String },
    Directory { canonical_root: String },
}

/// One concrete path selected for Startup Context.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredStartupSelectedPath {
    ProjectRelative { path: String },
    ExternalAbsolute { path: String },
}

/// Approval is bound to the exact canonical target observed when Mirza confirmed it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StoredStartupExternalApproval {
    pub approved_resolved_target: String,
}

/// Durable path selection. File contents never belong in this type.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StoredStartupFileSpec {
    pub id: String,
    pub path: StoredStartupSelectedPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_approval: Option<StoredStartupExternalApproval>,
}

/// Classification of the canonical target used by persisted receipts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredStartupPathClassification {
    Project,
    External,
}

/// Aggregate startup lifecycle for one authoritative session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StoredStartupContextState {
    Empty,
    Prepared,
    Blocked,
    Dispatched,
    ProviderAccepted,
    MetadataRepair {
        target: StoredStartupMetadataRepairTarget,
    },
}

/// Initial primary-session preparation failure that must continue blocking
/// provider dispatch even when no project receipt could be constructed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredStartupContextBlockKind {
    ProjectIdentity,
    PlanStorage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredStartupContextBlock {
    pub kind: StoredStartupContextBlockKind,
    pub message: String,
    pub blocked_at: DateTime<Utc>,
}

/// The durable lifecycle transition that must be repaired after a save failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredStartupMetadataRepairTarget {
    ProviderAccepted,
}

/// Inspectable information for a retryable startup receipt metadata repair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredStartupMetadataRepair {
    pub target: StoredStartupMetadataRepairTarget,
    pub required_at: DateTime<Utc>,
    pub attempts: u32,
    pub last_error: String,
}

/// Whether one captured startup batch has only been persisted, dispatched, or accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredStartupBatchDeliveryState {
    Captured,
    Dispatched,
    ProviderAccepted,
}

/// Initial startup capture or a later explicit addition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredStartupBatchKind {
    Initial,
    Late,
}

/// Latest known disk state for one captured startup file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StoredStartupObservedState {
    Current,
    Changed { sha256: String, bytes: u64 },
    Missing,
    Unreadable,
    Unsupported,
}

/// One timestamped observation retained by the session receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredStartupFileObservation {
    pub observed_at: DateTime<Utc>,
    pub state: StoredStartupObservedState,
}

/// Durable metadata and stable transcript identity for one captured file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredStartupFileReceipt {
    pub spec_id: String,
    pub message_id: String,
    pub ordinal: u32,
    pub logical_path: String,
    pub resolved_path: String,
    pub classification: StoredStartupPathClassification,
    pub sha256: String,
    pub bytes: u64,
    pub estimated_tokens: u64,
    pub latest_observation: StoredStartupFileObservation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_notified_observation: Option<StoredStartupObservedState>,
    #[serde(default)]
    pub notification_count: u8,
}

/// One atomic control-message plus ordered file-message group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredStartupContextBatch {
    pub id: String,
    pub kind: StoredStartupBatchKind,
    pub control_message_id: String,
    pub files: Vec<StoredStartupFileReceipt>,
    pub appended_at: DateTime<Utc>,
    pub delivery_state: StoredStartupBatchDeliveryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_dispatched_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_provider_accepted_at: Option<DateTime<Utc>>,
}

/// Stable target classification used by stored file issues.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredStartupTargetType {
    Directory,
    SymlinkToDirectory,
    DeviceOrSpecial,
}

/// Unsupported file content classification retained without storing the content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredStartupUnsupportedContent {
    Binary,
    Pdf,
    Image,
}

/// Durable issue kind for a failed selection or capture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredStartupFileIssueKind {
    EmptyPath,
    InvalidPathEncoding,
    PathTraversal,
    Missing,
    BrokenSymlink,
    Unreadable {
        detail: String,
    },
    UnsupportedTarget {
        target_type: StoredStartupTargetType,
    },
    UnsupportedContent {
        content: StoredStartupUnsupportedContent,
    },
    NonUtf8,
    ExternalApprovalRequired {
        resolved_target: String,
    },
    ExternalTargetChanged {
        approved_target: String,
        resolved_target: String,
    },
    InvalidExternalApproval {
        detail: String,
    },
    DuplicateSelection {
        first_input_index: u32,
    },
    TooManyEntries {
        count: u32,
        limit: u32,
    },
    FileTooLarge {
        bytes: u64,
        limit: u64,
    },
    BatchTooLarge {
        bytes: u64,
        limit: u64,
    },
    ChangedDuringCapture,
    DirectoryOutsideProject,
    DirectoryReadFailed {
        detail: String,
    },
}

/// One unresolved required-file issue retained by a blocked receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredStartupFileIssue {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_path: Option<String>,
    pub kind: StoredStartupFileIssueKind,
}

/// Durable queued selection shape used by later safe-boundary apply work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredPendingStartupUpdate {
    pub operation_id: String,
    pub created_at: DateTime<Utc>,
    pub expected_plan_revision: u64,
    pub selection: Vec<StoredStartupFileSpec>,
    pub save_project_default: bool,
}

/// Authoritative startup receipt persisted with the session transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredStartupContextReceipt {
    pub schema_version: u32,
    pub project: StoredStartupProjectIdentity,
    pub plan_revision: u64,
    pub state: StoredStartupContextState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub batches: Vec<StoredStartupContextBatch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_issues: Vec<StoredStartupFileIssue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_updates: Vec<StoredPendingStartupUpdate>,
    /// Most recent apply operation durably reflected in this receipt.
    ///
    /// This is metadata only. It lets crash recovery distinguish an operation
    /// that persisted its session target from one that has not yet done so,
    /// including empty or future-only applies that add no transcript batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_apply_operation_id: Option<String>,
    pub prepared_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_dispatched_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_provider_accepted_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_repair: Option<StoredStartupMetadataRepair>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_file_spec_round_trips_without_content_field() {
        let spec = StoredStartupFileSpec {
            id: "file-id".to_string(),
            path: StoredStartupSelectedPath::ExternalAbsolute {
                path: "/outside/PLAN.md".to_string(),
            },
            external_approval: Some(StoredStartupExternalApproval {
                approved_resolved_target: "/outside/PLAN.md".to_string(),
            }),
        };

        let encoded = serde_json::to_value(&spec).expect("serialize stored startup file spec");
        assert!(encoded.get("content").is_none());
        let decoded: StoredStartupFileSpec =
            serde_json::from_value(encoded).expect("deserialize stored startup file spec");
        assert_eq!(decoded, spec);
    }

    #[test]
    fn receipt_states_round_trip_without_file_content() {
        let timestamp = DateTime::parse_from_rfc3339("2026-08-29T16:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        let states = [
            StoredStartupContextState::Empty,
            StoredStartupContextState::Prepared,
            StoredStartupContextState::Blocked,
            StoredStartupContextState::Dispatched,
            StoredStartupContextState::ProviderAccepted,
            StoredStartupContextState::MetadataRepair {
                target: StoredStartupMetadataRepairTarget::ProviderAccepted,
            },
        ];

        for state in states {
            let receipt = StoredStartupContextReceipt {
                schema_version: 1,
                project: StoredStartupProjectIdentity::Directory {
                    canonical_root: "/project".to_string(),
                },
                plan_revision: 7,
                state,
                batches: vec![StoredStartupContextBatch {
                    id: "batch-1".to_string(),
                    kind: StoredStartupBatchKind::Initial,
                    control_message_id: "control-1".to_string(),
                    files: vec![StoredStartupFileReceipt {
                        spec_id: "spec-1".to_string(),
                        message_id: "message-1".to_string(),
                        ordinal: 2,
                        logical_path: "docs/PLAN.md".to_string(),
                        resolved_path: "/project/docs/PLAN.md".to_string(),
                        classification: StoredStartupPathClassification::Project,
                        sha256: "a".repeat(64),
                        bytes: 14,
                        estimated_tokens: 4,
                        latest_observation: StoredStartupFileObservation {
                            observed_at: timestamp,
                            state: StoredStartupObservedState::Current,
                        },
                        last_notified_observation: None,
                        notification_count: 0,
                    }],
                    appended_at: timestamp,
                    delivery_state: StoredStartupBatchDeliveryState::Captured,
                    first_dispatched_at: None,
                    first_provider_accepted_at: None,
                }],
                blocked_issues: Vec::new(),
                pending_updates: Vec::new(),
                last_apply_operation_id: None,
                prepared_at: timestamp,
                first_dispatched_at: None,
                first_provider_accepted_at: None,
                metadata_repair: None,
            };

            let encoded = serde_json::to_value(&receipt).expect("serialize startup receipt");
            assert!(encoded.get("content").is_none());
            let encoded_text = encoded.to_string();
            assert!(!encoded_text.contains("raw file body"));
            let decoded: StoredStartupContextReceipt =
                serde_json::from_value(encoded).expect("deserialize startup receipt");
            assert_eq!(decoded, receipt);
        }
    }
}
