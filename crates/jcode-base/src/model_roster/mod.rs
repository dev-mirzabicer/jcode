//! Global launch-time model policy. Existing session/model callers opt in
//! explicitly; this module never changes a running session or retries a turn.

mod resolver;
mod store;
pub use jcode_provider_core::qualified_model::QualifiedModel;
pub use resolver::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub use store::ModelRosterService;

pub const ROSTER_PATH: &str = "model-roster.toml";
pub const SHIPPED_ROSTER: &str = include_str!("seed.toml");

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRosterEntry {
    pub description: String,
    pub models: Vec<QualifiedModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RosterDiagnostic {
    pub alias: Option<String>,
    pub detail: String,
}

/// An immutable parse of one complete working file. Invalid entries remain
/// inspectable and do not prevent unrelated valid aliases from resolving.
#[derive(Clone, Debug)]
pub struct ModelRoster {
    entries: BTreeMap<String, ModelRosterEntry>,
    diagnostics: Vec<RosterDiagnostic>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoster {
    aliases: BTreeMap<String, toml::Value>,
}

impl ModelRoster {
    pub fn parse(content: &str) -> Result<Self, ModelRosterError> {
        let raw: RawRoster = toml::from_str(content)
            .map_err(|error| ModelRosterError::InvalidRoster(error.to_string()))?;
        let mut roster = Self {
            entries: BTreeMap::new(),
            diagnostics: Vec::new(),
        };
        for (alias, value) in raw.aliases {
            let result = validate_alias(&alias).and_then(|()| {
                let entry: ModelRosterEntry = value
                    .try_into()
                    .map_err(|error: toml::de::Error| error.to_string())?;
                if entry.description.trim().is_empty() {
                    return Err("description is required".into());
                }
                if entry.models.is_empty() {
                    return Err("candidate list must be nonempty".into());
                }
                if let Some(effort) = &entry.default_effort {
                    validate_effort(effort)?;
                }
                Ok(entry)
            });
            match result {
                Ok(entry) => {
                    roster.entries.insert(alias, entry);
                }
                Err(detail) => roster.diagnostics.push(RosterDiagnostic {
                    alias: Some(alias),
                    detail,
                }),
            }
        }
        Ok(roster)
    }

    pub fn validate(&self) -> &[RosterDiagnostic] {
        &self.diagnostics
    }

    pub fn inspect(&self, alias: &str) -> Result<&ModelRosterEntry, ModelRosterError> {
        if let Some(error) = self
            .diagnostics
            .iter()
            .find(|error| error.alias.as_deref() == Some(alias))
        {
            return Err(ModelRosterError::InvalidAlias {
                alias: alias.into(),
                detail: error.detail.clone(),
            });
        }
        self.entries
            .get(alias)
            .ok_or_else(|| ModelRosterError::MissingAlias(alias.into()))
    }

    /// Model-facing discovery deliberately excludes human notes and routes.
    pub fn list(&self) -> Vec<ModelAliasDescription> {
        self.entries
            .iter()
            .map(|(alias, entry)| ModelAliasDescription {
                alias: alias.clone(),
                description: entry.description.clone(),
            })
            .collect()
    }

    pub fn to_toml(&self) -> Result<String, ModelRosterError> {
        if !self.diagnostics.is_empty() {
            return Err(ModelRosterError::InvalidRoster(
                "repair invalid entries before serializing the roster".into(),
            ));
        }
        #[derive(Serialize)]
        struct Document<'a> {
            aliases: &'a BTreeMap<String, ModelRosterEntry>,
        }
        toml::to_string_pretty(&Document {
            aliases: &self.entries,
        })
        .map_err(|error| ModelRosterError::InvalidRoster(error.to_string()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelAliasDescription {
    pub alias: String,
    pub description: String,
}

fn validate_alias(alias: &str) -> Result<(), String> {
    crate::instruction::InstructionId::parse(alias)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn validate_effort(effort: &str) -> Result<(), String> {
    if jcode_provider_core::canonical_reasoning_effort(effort) == Some(effort) {
        Ok(())
    } else {
        Err("effort must be a canonical provider effort, not a workflow sentinel".into())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum ModelRosterError {
    Source {
        detail: String,
    },
    InvalidRoster(String),
    MissingAlias(String),
    InvalidAlias {
        alias: String,
        detail: String,
    },
    InvalidRequest(String),
    CatalogUnavailable(String),
    AllCandidatesRejected {
        alias: Option<String>,
        candidates: Vec<CandidateRejection>,
    },
}

impl std::fmt::Display for ModelRosterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source { detail } => write!(f, "model roster source: {detail}"),
            Self::InvalidRoster(detail) => write!(f, "invalid model roster: {detail}"),
            Self::MissingAlias(alias) => write!(f, "model roster alias '{alias}' is missing"),
            Self::InvalidAlias { alias, detail } => {
                write!(f, "model roster alias '{alias}': {detail}")
            }
            Self::InvalidRequest(detail) => write!(f, "invalid model roster request: {detail}"),
            Self::CatalogUnavailable(detail) => write!(f, "model catalog unavailable: {detail}"),
            Self::AllCandidatesRejected { alias, candidates } => {
                write!(
                    f,
                    "no model candidate is available for {}",
                    alias.as_deref().unwrap_or("explicit override")
                )?;
                for candidate in candidates {
                    write!(f, "\n  {}: {}", candidate.model, candidate.reason)?;
                }
                Ok(())
            }
        }
    }
}
impl std::error::Error for ModelRosterError {}
