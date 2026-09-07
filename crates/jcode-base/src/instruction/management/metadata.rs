use super::*;
use crate::model_roster::{ModelRoster, ModelRosterEntry, QualifiedModel};

pub(super) fn kind(value: InstructionEditKind) -> InstructionKind {
    match value {
        InstructionEditKind::System => InstructionKind::System,
        InstructionEditKind::Agent => InstructionKind::Agent,
        InstructionEditKind::AgentAddendum => InstructionKind::AgentAddendum,
        InstructionEditKind::Module => InstructionKind::Module,
        InstructionEditKind::Notification => InstructionKind::Notification,
        InstructionEditKind::ToolGuidance => InstructionKind::ToolGuidance,
        InstructionEditKind::Skill => InstructionKind::Skill,
    }
}
fn edit_kind(value: InstructionKind) -> InstructionEditKind {
    match value {
        InstructionKind::System => InstructionEditKind::System,
        InstructionKind::Agent => InstructionEditKind::Agent,
        InstructionKind::AgentAddendum => InstructionEditKind::AgentAddendum,
        InstructionKind::Module => InstructionEditKind::Module,
        InstructionKind::Notification => InstructionEditKind::Notification,
        InstructionKind::ToolGuidance => InstructionEditKind::ToolGuidance,
        InstructionKind::Skill => InstructionEditKind::Skill,
    }
}
pub(super) fn scope(repository: &InstructionRepositoryRef) -> InstructionScope {
    if repository.kind == InstructionRepositoryKind::Global {
        InstructionScope::Global
    } else {
        InstructionScope::Project
    }
}
pub(super) fn selector_text(selector: &InstructionSelector) -> String {
    match selector.scope {
        InstructionScopeSelector::Unqualified => selector.id.to_string(),
        InstructionScopeSelector::Global => format!("global:{}", selector.id),
        InstructionScopeSelector::Project => format!("project:{}", selector.id),
    }
}
pub(super) fn selector(resource: &InstructionResourceRef) -> InstructionSelector {
    InstructionSelector {
        scope: if resource.scope == InstructionScope::Global {
            InstructionScopeSelector::Global
        } else {
            InstructionScopeSelector::Project
        },
        kind: resource.kind,
        id: resource.id.clone(),
    }
}
pub(super) fn path_kind(path: &Path) -> Result<InstructionKind> {
    let first = path
        .components()
        .next()
        .and_then(|part| part.as_os_str().to_str())
        .unwrap_or_default();
    [
        InstructionKind::System,
        InstructionKind::Agent,
        InstructionKind::AgentAddendum,
        InstructionKind::Module,
        InstructionKind::Notification,
        InstructionKind::ToolGuidance,
        InstructionKind::Skill,
    ]
    .into_iter()
    .find(|kind| kind.directory() == first)
    .ok_or_else(|| fail("edit source", "Select a managed instruction resource"))
}
pub(super) fn parse(
    repository: &InstructionRepositoryRef,
    path: &Path,
    source: &str,
) -> Result<InstructionDocument> {
    super::super::runtime::parse_document(
        scope(repository),
        path_kind(path)?,
        &repository.root.join(path),
        source,
    )
    .map_err(|error| fail("parse draft", error))
}
pub(super) fn fields(document: &InstructionDocument) -> InstructionResourceFields {
    InstructionResourceFields {
        id: document.id.to_string(),
        kind: edit_kind(document.kind),
        name: document.metadata.display_name.clone(),
        description: document.metadata.description.clone(),
        template: if document.template_mode == TemplateMode::Plain {
            InstructionEditTemplate::Plain
        } else {
            InstructionEditTemplate::Handlebars
        },
        availability: document
            .metadata
            .agent
            .as_ref()
            .map(|agent| match agent.availability {
                AgentAvailability::Primary => InstructionEditAvailability::Primary,
                AgentAvailability::Isolated => InstructionEditAvailability::Isolated,
                AgentAvailability::Both => InstructionEditAvailability::Both,
            }),
        target: document
            .metadata
            .addendum
            .as_ref()
            .map(|value| selector_text(&value.target)),
        includes: document
            .metadata
            .includes
            .iter()
            .map(selector_text)
            .collect(),
        allowed_tools: document.metadata.allowed_tools.clone(),
    }
}
pub(super) fn document(
    repository: &InstructionRepositoryRef,
    path: &Path,
    fields: &InstructionResourceFields,
    body: String,
) -> Result<InstructionDocument> {
    let document = InstructionDocument {
        id: InstructionId::parse(&fields.id).map_err(|error| fail("resource ID", error))?,
        kind: kind(fields.kind),
        scope: scope(repository),
        template_mode: if fields.template == InstructionEditTemplate::Plain {
            TemplateMode::Plain
        } else {
            TemplateMode::Handlebars
        },
        metadata: InstructionMetadata {
            display_name: fields.name.clone(),
            description: fields.description.clone(),
            agent: fields.availability.map(|value| AgentMetadata {
                availability: match value {
                    InstructionEditAvailability::Primary => AgentAvailability::Primary,
                    InstructionEditAvailability::Isolated => AgentAvailability::Isolated,
                    InstructionEditAvailability::Both => AgentAvailability::Both,
                },
            }),
            addendum: fields
                .target
                .as_deref()
                .map(|target| {
                    InstructionSelector::parse(InstructionKind::Agent, target)
                        .map(|target| AddendumMetadata { target })
                })
                .transpose()
                .map_err(|error| fail("addendum target", error))?,
            includes: fields
                .includes
                .iter()
                .map(|value| InstructionSelector::parse(InstructionKind::Module, value))
                .collect::<std::result::Result<_, _>>()
                .map_err(|error| fail("module references", error))?,
            allowed_tools: fields.allowed_tools.clone(),
        },
        body,
        path: repository.root.join(path),
    };
    // Use the authoritative parser to reject unsupported kind/field combinations.
    let serialized = document
        .to_markdown()
        .map_err(|error| fail("serialize metadata", error))?;
    let parsed = parse(repository, path, &serialized)?;
    if parsed != document {
        return Err(fail(
            "metadata",
            "Metadata does not round-trip without loss",
        ));
    }
    Ok(document)
}
pub(super) fn file(
    repository: &InstructionRepositoryRef,
    path: &Path,
    source: Option<&str>,
) -> InstructionEditFile {
    let key = path.to_string_lossy().into_owned();
    let source = source.unwrap_or_default();
    let extracted: Result<(InstructionEditMetadata, String)> = (|| {
        if path == Path::new("instruction-store.toml") {
            let manifest: InstructionStoreManifest =
                toml::from_str(source).map_err(|error| fail("store settings", error))?;
            return Ok((
                InstructionEditMetadata::StoreSettings {
                    default_agent: manifest.default_agent,
                },
                String::new(),
            ));
        }
        if path == Path::new(crate::model_roster::ROSTER_PATH) {
            let roster = ModelRoster::parse(source).map_err(|error| fail("model roster", error))?;
            let entries = roster
                .list()
                .into_iter()
                .map(|entry| {
                    let value = roster
                        .inspect(&entry.alias)
                        .map_err(|error| fail("model alias", error))?;
                    Ok(InstructionRosterFields {
                        alias: entry.alias,
                        description: value.description.clone(),
                        candidates: value.models.iter().map(ToString::to_string).collect(),
                        effort: value.default_effort.clone(),
                        notes: value.notes.clone(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            return Ok((InstructionEditMetadata::Roster(entries), String::new()));
        }
        if auxiliary(path) {
            return Ok((InstructionEditMetadata::Ecosystem, source.into()));
        }
        let document = parse(repository, path, source)?;
        Ok((
            InstructionEditMetadata::Resource(fields(&document)),
            document.body,
        ))
    })();
    let (metadata, body) = extracted.unwrap_or_else(|error| {
        (
            InstructionEditMetadata::Damaged {
                detail: error.detail,
            },
            source.to_string(),
        )
    });
    InstructionEditFile {
        executable: false,
        key: key.clone(),
        path: key,
        metadata,
        body,
        deleted: false,
    }
}
pub(super) fn apply(
    repository: &InstructionRepositoryRef,
    path: &Path,
    original: &str,
    change: &InstructionDraftChange,
) -> Result<String> {
    if auxiliary(path) {
        return match change {
            InstructionDraftChange::Body { body, .. } => Ok(body.clone()),
            _ => Err(fail(
                "package reference",
                "Package reference text uses plain body editing; it is not resource metadata",
            )),
        };
    }
    match change {
        InstructionDraftChange::RepairSource { source, .. } => {
            let invalid_roster = path == Path::new(crate::model_roster::ROSTER_PATH)
                && ModelRoster::parse(original).is_ok_and(|roster| !roster.validate().is_empty());
            if !invalid_roster
                && !matches!(
                    file(repository, path, Some(original)).metadata,
                    InstructionEditMetadata::Damaged { .. }
                )
            {
                return Err(fail(
                    "repair source",
                    "Valid resources use typed metadata and body edits. Identity changes require Rename with reference repair.",
                ));
            }
            Ok(source.clone())
        }
        InstructionDraftChange::Body { body, .. } => {
            let mut document = parse(repository, path, original)?;
            document.body = body.clone();
            document
                .to_markdown()
                .map_err(|error| fail("serialize body", error))
        }
        InstructionDraftChange::Metadata {
            metadata: InstructionEditMetadata::Resource(fields),
            ..
        } => {
            let current = parse(repository, path, original)?;
            if current.id.as_str() != fields.id || current.kind != kind(fields.kind) {
                return Err(fail(
                    "metadata",
                    "Use Rename to change identity; resource kind is fixed",
                ));
            }
            document(repository, path, fields, current.body)?
                .to_markdown()
                .map_err(|error| fail("serialize metadata", error))
        }
        InstructionDraftChange::Metadata {
            metadata: InstructionEditMetadata::StoreSettings { default_agent },
            ..
        } if path == Path::new("instruction-store.toml") => {
            let mut manifest: InstructionStoreManifest =
                toml::from_str(original).map_err(|error| fail("parse settings", error))?;
            manifest.default_agent = default_agent.clone();
            toml::to_string_pretty(&manifest).map_err(|error| fail("serialize settings", error))
        }
        InstructionDraftChange::Metadata {
            metadata: InstructionEditMetadata::Roster(entries),
            ..
        } if path == Path::new(crate::model_roster::ROSTER_PATH)
            && scope(repository) == InstructionScope::Global =>
        {
            let mut source: toml::Value =
                toml::from_str(original).map_err(|error| fail("parse roster", error))?;
            let roster =
                ModelRoster::parse(original).map_err(|error| fail("parse roster", error))?;
            let aliases = source
                .get_mut("aliases")
                .and_then(toml::Value::as_table_mut)
                .ok_or_else(|| fail("roster", "Missing aliases table"))?;
            for old in roster.list() {
                aliases.remove(&old.alias);
            }
            let mut seen = BTreeSet::new();
            for entry in entries {
                InstructionId::parse(&entry.alias).map_err(|error| fail("alias ID", error))?;
                if !seen.insert(&entry.alias) {
                    return Err(fail("roster", "Duplicate alias ID"));
                }
                let model = ModelRosterEntry {
                    description: entry.description.clone(),
                    models: entry
                        .candidates
                        .iter()
                        .map(QualifiedModel::parse)
                        .collect::<std::result::Result<_, _>>()
                        .map_err(|error| fail("candidate route", error))?,
                    default_effort: entry.effort.clone(),
                    notes: entry.notes.clone(),
                };
                aliases.insert(
                    entry.alias.clone(),
                    toml::Value::try_from(model).map_err(|error| fail("serialize alias", error))?,
                );
            }
            toml::to_string_pretty(&source).map_err(|error| fail("serialize roster", error))
        }
        _ => Err(fail("metadata", "Metadata does not apply to this file")),
    }
}

pub(super) fn auxiliary(path: &Path) -> bool {
    path.starts_with("skills") && path.file_name().is_some_and(|name| name != "SKILL.md")
}
pub(super) fn binary_file(path: &Path, bytes: &[u8]) -> InstructionEditFile {
    use sha2::{Digest, Sha256};
    let sha256 = format!("{:x}", Sha256::digest(bytes));
    InstructionEditFile {
        executable: false,
        key: path.to_string_lossy().into_owned(),
        path: path.to_string_lossy().into_owned(),
        body: format!(
            "Binary file: {} bytes. SHA-256 {sha256}. Complete bytes are retained in the private draft; no text substitution is saved.",
            bytes.len()
        ),
        metadata: InstructionEditMetadata::Binary {
            bytes: bytes.len(),
            sha256,
        },
        deleted: false,
    }
}
pub(super) fn display_bytes(bytes: Option<Vec<u8>>) -> Option<String> {
    bytes.map(|bytes| match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => binary_file(Path::new("binary"), &error.into_bytes()).body,
    })
}
