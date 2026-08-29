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
}
