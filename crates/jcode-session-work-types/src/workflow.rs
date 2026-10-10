//! The workflow model. The text grammar, validation and revisions are owned by
//! `jcode-base::session_work::workflow`; this is the parsed shape clients see.
use super::*;
use std::fmt;

/// The five module statuses and their checklist markers.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleStatus {
    /// `[ ]`
    Pending,
    /// `[>]`
    Active,
    /// `[x]`
    Done,
    /// `[-]`, with a `> skipped: <reason>` note.
    Skipped,
    /// `[?]`: decided when the session gets there.
    Undetermined,
}

impl ModuleStatus {
    pub const fn marker(self) -> char {
        match self {
            Self::Pending => ' ',
            Self::Active => '>',
            Self::Done => 'x',
            Self::Skipped => '-',
            Self::Undetermined => '?',
        }
    }
    pub const fn from_marker(marker: char) -> Option<Self> {
        Some(match marker {
            ' ' => Self::Pending,
            '>' => Self::Active,
            'x' => Self::Done,
            '-' => Self::Skipped,
            '?' => Self::Undetermined,
            _ => return None,
        })
    }
    /// Done or skipped.
    pub const fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Skipped)
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Done => "done",
            Self::Skipped => "skipped",
            Self::Undetermined => "undetermined",
        }
    }
}

impl fmt::Display for ModuleStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A module ID: lowercase ASCII letters and digits, then letters, digits,
/// `-` or `_`. Unique within one workflow.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModuleId(String);

impl ModuleId {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut bytes = text.bytes();
        let valid = bytes
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
            && bytes.all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
            });
        if valid {
            Ok(Self(text.to_string()))
        } else {
            Err(format!(
                "module ID `{text}` must use lowercase letters, digits, `-` or `_`, starting with a letter or digit"
            ))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ModuleId {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}
impl From<ModuleId> for String {
    fn from(value: ModuleId) -> Self {
        value.0
    }
}
impl fmt::Display for ModuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A module's `{type}` or `{type/subtype}` label. Labels need not be registered
/// module types; custom labels are allowed.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ModuleTypeRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
}

impl fmt::Display for ModuleTypeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)?;
        if let Some(subtype) = &self.subtype {
            write!(f, "/{subtype}")?;
        }
        Ok(())
    }
}

/// One module and its nested children, in file order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowModule {
    pub id: ModuleId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_type: Option<ModuleTypeRef>,
    pub status: ModuleStatus,
    /// Note lines without their `> ` prefix, such as `skipped: <reason>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<WorkflowModule>,
}

impl WorkflowModule {
    /// The reason recorded by a `> skipped: <reason>` note.
    pub fn skip_reason(&self) -> Option<&str> {
        self.notes.iter().find_map(|note| {
            note.strip_prefix("skipped:")
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
        })
    }
}

/// A parsed, valid workflow: at least one module.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    pub modules: Vec<WorkflowModule>,
}

impl Workflow {
    /// Every module in file order with its nesting depth.
    pub fn walk(&self) -> Vec<(usize, &WorkflowModule)> {
        fn visit<'a>(
            modules: &'a [WorkflowModule],
            depth: usize,
            out: &mut Vec<(usize, &'a WorkflowModule)>,
        ) {
            for module in modules {
                out.push((depth, module));
                visit(&module.children, depth + 1, out);
            }
        }
        let mut out = Vec::new();
        visit(&self.modules, 0, &mut out);
        out
    }

    pub fn module(&self, id: &str) -> Option<&WorkflowModule> {
        self.walk()
            .into_iter()
            .map(|(_, module)| module)
            .find(|module| module.id.as_str() == id)
    }

    /// The active leaf, if any. Validation guarantees at most one.
    pub fn active_leaf(&self) -> Option<&WorkflowModule> {
        self.walk()
            .into_iter()
            .map(|(_, module)| module)
            .find(|module| module.children.is_empty() && module.status == ModuleStatus::Active)
    }

    /// Top-level and nested modules that are not done or skipped, in file
    /// order, without repeating children of an unfinished parent.
    pub fn unfinished(&self) -> Vec<&WorkflowModule> {
        fn visit<'a>(modules: &'a [WorkflowModule], out: &mut Vec<&'a WorkflowModule>) {
            for module in modules {
                if module.status.is_finished() {
                    continue;
                }
                if module.children.is_empty() {
                    out.push(module);
                } else {
                    visit(&module.children, out);
                }
            }
        }
        let mut out = Vec::new();
        visit(&self.modules, &mut out);
        out
    }

    /// Every module is done or skipped.
    pub fn is_finished(&self) -> bool {
        self.modules
            .iter()
            .all(|module| module.status.is_finished())
    }
}

/// Where a stored revision came from.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RevisionSource {
    /// A native file-tool write by the session's agent.
    Agent {},
    /// A task preset's workflow template, as an isolated child's revision 1.
    Template { preset: String },
    /// Copied by transfer from another session's revision.
    Transfer {
        source_session: String,
        source_revision: u32,
    },
}

/// One accepted revision of a session's workflow file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowRevision {
    pub revision: u32,
    pub text: String,
    pub source: RevisionSource,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Host-recorded module timing, kept outside the file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModuleTiming {
    pub module: ModuleId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_active_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When the module last became done or skipped. Cleared if it reopens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_as: Option<ModuleStatus>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(id: &str, status: ModuleStatus, children: Vec<WorkflowModule>) -> WorkflowModule {
        WorkflowModule {
            id: ModuleId::parse(id).unwrap(),
            title: id.to_uppercase(),
            module_type: None,
            status,
            notes: Vec::new(),
            children,
        }
    }

    #[test]
    fn module_ids_accept_slugs_only() {
        for good in ["a", "proto-b", "step_2", "9lives"] {
            assert!(ModuleId::parse(good).is_ok(), "{good}");
        }
        for bad in ["", "-a", "_a", "Proto", "a b", "a.b", "é"] {
            assert!(ModuleId::parse(bad).is_err(), "{bad}");
        }
        assert!(serde_json::from_str::<ModuleId>("\"Bad\"").is_err());
    }

    #[test]
    fn unfinished_lists_leaves_under_unfinished_parents() {
        let workflow = Workflow {
            modules: vec![
                module("ground", ModuleStatus::Done, vec![]),
                module(
                    "proto",
                    ModuleStatus::Active,
                    vec![
                        module("proto-a", ModuleStatus::Done, vec![]),
                        module("proto-b", ModuleStatus::Active, vec![]),
                    ],
                ),
                module("after", ModuleStatus::Undetermined, vec![]),
            ],
        };
        let ids: Vec<_> = workflow
            .unfinished()
            .iter()
            .map(|module| module.id.as_str())
            .collect();
        assert_eq!(ids, ["proto-b", "after"]);
        assert_eq!(workflow.active_leaf().unwrap().id.as_str(), "proto-b");
        assert!(!workflow.is_finished());
        assert_eq!(workflow.walk().len(), 5);
    }

    #[test]
    fn revision_sources_are_closed_tagged_records() {
        let source = RevisionSource::Transfer {
            source_session: "session_a".into(),
            source_revision: 3,
        };
        let json = serde_json::to_string(&source).unwrap();
        assert_eq!(
            serde_json::from_str::<RevisionSource>(&json).unwrap(),
            source
        );
        assert!(serde_json::from_str::<RevisionSource>(r#"{"kind":"agent","extra":1}"#).is_err());
    }
}
