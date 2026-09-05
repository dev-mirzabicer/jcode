use super::Session;
use crate::message::ContentBlock;
use jcode_session_types::{
    StoredStartupBatchDeliveryState, StoredStartupBatchKind, StoredStartupObservedState,
};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// Warning callers must present before exporting complete Startup Context bodies.
pub const STARTUP_CONTEXT_FULL_EXPORT_WARNING: &str = "Full-context export includes complete Startup Context file contents after secret-pattern redaction. Review the destination before continuing.";

/// Controls whether a public export includes receipt-owned Startup Context bodies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StartupContextExportPolicy {
    /// Replace control, file, and stale-marker bodies with receipt-based omission records.
    #[default]
    ReceiptsOnly,
    /// Include complete receipt-owned bodies after the existing export redaction pass.
    IncludeContents,
}

impl StartupContextExportPolicy {
    pub fn warning(self) -> Option<&'static str> {
        matches!(self, Self::IncludeContents).then_some(STARTUP_CONTEXT_FULL_EXPORT_WARNING)
    }
}

#[derive(Serialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum StartupContextExportOmission<'a> {
    #[serde(rename = "startup_context_batch_omission")]
    Batch {
        contents: &'static str,
        include_contents_policy: &'static str,
        batch_kind: StoredStartupBatchKind,
        file_count: usize,
        delivery_state: StoredStartupBatchDeliveryState,
    },
    #[serde(rename = "startup_context_file_omission")]
    File {
        contents: &'static str,
        include_contents_policy: &'static str,
        path: &'a str,
        resolved_path: &'a str,
        sha256: &'a str,
        bytes: u64,
        ordinal: u32,
        batch_kind: StoredStartupBatchKind,
        delivery_state: StoredStartupBatchDeliveryState,
    },
    #[serde(rename = "startup_context_stale_marker_omission")]
    StaleMarker {
        contents: &'static str,
        include_contents_policy: &'static str,
        path: &'a str,
        startup_sha256: &'a str,
        notification_ordinal: usize,
        latest_observation: &'a StoredStartupObservedState,
        batch_kind: StoredStartupBatchKind,
        delivery_state: StoredStartupBatchDeliveryState,
    },
}

pub(super) fn project_startup_context_messages(
    session: &mut Session,
    policy: StartupContextExportPolicy,
) {
    if policy == StartupContextExportPolicy::IncludeContents {
        return;
    }

    let Some(receipt) = session.startup_context.as_ref() else {
        return;
    };
    let mut omissions = HashMap::new();
    for batch in &receipt.batches {
        omissions.insert(
            batch.control_message_id.clone(),
            omission_json(&StartupContextExportOmission::Batch {
                contents: "omitted_by_default",
                include_contents_policy: "include_contents",
                batch_kind: batch.kind,
                file_count: batch.files.len(),
                delivery_state: batch.delivery_state,
            }),
        );
        for file in &batch.files {
            omissions.insert(
                file.message_id.clone(),
                omission_json(&StartupContextExportOmission::File {
                    contents: "omitted_by_default",
                    include_contents_policy: "include_contents",
                    path: &file.logical_path,
                    resolved_path: &file.resolved_path,
                    sha256: &file.sha256,
                    bytes: file.bytes,
                    ordinal: file.ordinal,
                    batch_kind: batch.kind,
                    delivery_state: batch.delivery_state,
                }),
            );
            for (index, marker_id) in file.stale_marker_message_ids.iter().enumerate() {
                omissions.insert(
                    marker_id.clone(),
                    omission_json(&StartupContextExportOmission::StaleMarker {
                        contents: "omitted_by_default",
                        include_contents_policy: "include_contents",
                        path: &file.logical_path,
                        startup_sha256: &file.sha256,
                        notification_ordinal: index.saturating_add(1),
                        latest_observation: &file.latest_observation.state,
                        batch_kind: batch.kind,
                        delivery_state: batch.delivery_state,
                    }),
                );
            }
        }
    }

    for message in &mut session.messages {
        if let Some(omission) = omissions.get(&message.id) {
            message.content = vec![ContentBlock::Text {
                text: omission.clone(),
                cache_control: None,
            }];
        }
    }
}

fn omission_json(record: &StartupContextExportOmission<'_>) -> String {
    serde_json::to_string_pretty(record)
        .expect("Startup Context omission records contain only serializable receipt metadata")
}

impl Session {
    /// Structural identities for receipt-owned control, file, and stale-marker messages.
    pub fn startup_context_message_ids(&self) -> HashSet<&str> {
        let mut ids = HashSet::new();
        if let Some(receipt) = self.startup_context.as_ref() {
            for batch in &receipt.batches {
                ids.insert(batch.control_message_id.as_str());
                for file in &batch.files {
                    ids.insert(file.message_id.as_str());
                    ids.extend(file.stale_marker_message_ids.iter().map(String::as_str));
                }
            }
        }
        ids
    }

    pub fn is_startup_context_message_id(&self, message_id: &str) -> bool {
        self.startup_context_message_ids().contains(message_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;
    use chrono::Utc;
    use jcode_session_types::{
        StoredDisplayRole, StoredMessage, StoredStartupContextBatch, StoredStartupContextReceipt,
        StoredStartupContextState, StoredStartupFileObservation, StoredStartupFileReceipt,
        StoredStartupMetadataRepair, StoredStartupPathClassification, StoredStartupProjectIdentity,
    };

    const RAW_BODY: &str = "RAW_STARTUP_BODY_SENTINEL\nOPENROUTER_API_KEY=sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789";
    const STALE_BODY: &str = "RAW_STALE_MARKER_SENTINEL";

    fn stored(id: &str, content: Vec<ContentBlock>) -> StoredMessage {
        StoredMessage {
            origin: None,
            id: id.to_string(),
            role: Role::User,
            content,
            display_role: Some(StoredDisplayRole::System),
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        }
    }

    fn startup_export_session() -> Session {
        let now = Utc::now();
        let mut session = Session::create_with_id(
            "session-startup-export".to_string(),
            None,
            Some("Startup export".to_string()),
        );
        session.append_stored_message(stored(
            "startup-control",
            vec![ContentBlock::Text {
                text: "RAW_STARTUP_CONTROL_SENTINEL".to_string(),
                cache_control: None,
            }],
        ));
        session.append_stored_message(stored(
            "startup-file",
            vec![
                ContentBlock::Text {
                    text: "synthetic metadata".to_string(),
                    cache_control: None,
                },
                ContentBlock::Text {
                    text: RAW_BODY.to_string(),
                    cache_control: None,
                },
            ],
        ));
        session.append_stored_message(stored(
            "startup-stale",
            vec![ContentBlock::Text {
                text: STALE_BODY.to_string(),
                cache_control: None,
            }],
        ));
        session.startup_context = Some(StoredStartupContextReceipt {
            schema_version: 1,
            project: StoredStartupProjectIdentity::Directory {
                canonical_root: "/synthetic/project".to_string(),
            },
            plan_revision: 7,
            state: StoredStartupContextState::ProviderAccepted,
            batches: vec![StoredStartupContextBatch {
                id: "startup-batch".to_string(),
                kind: StoredStartupBatchKind::Initial,
                control_message_id: "startup-control".to_string(),
                files: vec![StoredStartupFileReceipt {
                    spec_id: "startup-spec".to_string(),
                    message_id: "startup-file".to_string(),
                    ordinal: 2,
                    logical_path: "docs/PLAN.md".to_string(),
                    resolved_path: "/synthetic/project/docs/PLAN.md".to_string(),
                    classification: StoredStartupPathClassification::Project,
                    sha256: "a".repeat(64),
                    bytes: RAW_BODY.len() as u64,
                    estimated_tokens: 24,
                    latest_observation: StoredStartupFileObservation {
                        observed_at: now,
                        state: StoredStartupObservedState::Changed {
                            sha256: "b".repeat(64),
                            bytes: 42,
                        },
                    },
                    last_notified_observation: Some(StoredStartupObservedState::Changed {
                        sha256: "b".repeat(64),
                        bytes: 42,
                    }),
                    notification_count: 1,
                    stale_marker_message_ids: vec!["startup-stale".to_string()],
                }],
                appended_at: now,
                delivery_state: StoredStartupBatchDeliveryState::ProviderAccepted,
                first_dispatched_at: Some(now),
                first_provider_accepted_at: Some(now),
            }],
            blocked_issues: Vec::new(),
            pending_updates: Vec::new(),
            last_apply_operation_id: None,
            prepared_at: now,
            first_dispatched_at: Some(now),
            first_provider_accepted_at: Some(now),
            metadata_repair: None::<StoredStartupMetadataRepair>,
        });
        session
    }

    #[test]
    fn default_export_omits_every_receipt_owned_body_and_keeps_receipt_metadata() {
        let session = startup_export_session();
        let source_before = serde_json::to_vec(&session).expect("source session");

        let exported = session.redacted_for_export();
        let json = serde_json::to_string(&exported).expect("default export");

        assert!(!json.contains("RAW_STARTUP_CONTROL_SENTINEL"));
        assert!(!json.contains("RAW_STARTUP_BODY_SENTINEL"));
        assert!(!json.contains("RAW_STALE_MARKER_SENTINEL"));
        assert!(json.contains("startup_context_batch"));
        assert!(json.contains("startup_context_file"));
        assert!(json.contains("startup_context_stale_marker"));
        assert!(json.contains("docs/PLAN.md"));
        assert!(json.contains("/synthetic/project/docs/PLAN.md"));
        assert!(json.contains(&"a".repeat(64)));
        assert!(json.contains(&RAW_BODY.len().to_string()));
        assert!(json.contains("initial"));
        assert!(json.contains("provider_accepted"));
        assert!(json.contains("include_contents"));
        assert_eq!(
            serde_json::to_vec(&session).expect("source after export"),
            source_before
        );
        assert_eq!(session.startup_context_message_ids().len(), 3);
    }

    #[test]
    fn explicit_full_export_includes_redacted_contents_and_requires_warning() {
        let session = startup_export_session();
        let policy = StartupContextExportPolicy::IncludeContents;

        let exported = session.redacted_for_export_with_policy(policy);
        let json = serde_json::to_string(&exported).expect("full export");

        assert!(json.contains("RAW_STARTUP_CONTROL_SENTINEL"));
        assert!(json.contains("RAW_STARTUP_BODY_SENTINEL"));
        assert!(json.contains("RAW_STALE_MARKER_SENTINEL"));
        assert!(json.contains("OPENROUTER_API_KEY=[REDACTED_SECRET]"));
        assert!(!json.contains("sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789"));
        assert_eq!(policy.warning(), Some(STARTUP_CONTEXT_FULL_EXPORT_WARNING));
        assert_eq!(StartupContextExportPolicy::ReceiptsOnly.warning(), None);
    }
}
