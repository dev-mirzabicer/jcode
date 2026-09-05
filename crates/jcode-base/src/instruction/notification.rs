//! Occurrence-time prose. Callers retain triggers, framing and delivery.
//!
//! No source snapshot survives an occurrence. In particular this must not use
//! the session's frozen system-prompt activation as a notification catalog.

use super::*;
use serde::Serialize;
use std::path::Path;

macro_rules! notifications {
    ($( $variant:ident $( { $( $field:ident : $ty:ty ),* $(,)? } )? => ($id:literal, $owner:literal, $mode:ident) ),* $(,)?) => {
        #[derive(Debug, Serialize)]
        #[serde(untagged)]
        pub enum Notification<'a> {
            $( $variant $( { $( $field: $ty ),* } )? ),*
        }

        impl Notification<'_> {
            pub fn registration(&self) -> Result<ConsumerRegistration, InstructionError> {
                let (id, owner) = match self {
                    $( Self::$variant $( { $( $field: _ ),* } )? => ($id, $owner) ),*
                };
                registration(id, owner)
            }
        }

        /// Code-triggered consumers, including high-impact project redefinitions.
        pub fn registrations() -> Result<Vec<ConsumerRegistration>, InstructionError> {
            vec![$( registration($id, $owner) ),*].into_iter().collect()
        }

        pub(super) fn seed_documents() -> Result<Vec<InstructionDocument>, InstructionError> {
            Ok(vec![$( InstructionDocument {
                id: InstructionId::parse($id)?,
                kind: InstructionKind::Notification,
                scope: InstructionScope::Global,
                template_mode: TemplateMode::$mode,
                metadata: InstructionMetadata::default(),
                body: include_str!(concat!("notification/", $id, ".md")).to_string(),
                path: std::path::PathBuf::from(concat!("notifications/", $id, ".md")),
            } ),*])
        }
    };
}

notifications! {
    BrowserStatusReady => ("browser-status-ready", "browser status", Plain),
    BrowserStatusOutdated { missing_actions: &'a str } => ("browser-status-outdated", "browser status", Handlebars),
    BrowserStatusUnresponsive => ("browser-status-unresponsive", "browser status", Plain),
    BrowserStatusAbsent => ("browser-status-absent", "browser status", Plain),
    BrowserReadinessBlocked { details: &'a str } => ("browser-readiness-blocked", "browser readiness gate", Handlebars),
    ToolInterruptedWait { input: &'a str } => ("tool-interrupted-by-reload-wait", "agent reload tool-result completion", Handlebars),
    ScheduledTaskDue => ("scheduled-task-due", "scheduled task delivery", Plain),
    MacosComputerPermissions => ("macos-computer-permissions", "macOS permission status", Plain),
    ConfigEditInvalid { path: &'a str, error: &'a str } => ("config-edit-invalid", "config edit result", Handlebars),
    ConfigEditLive => ("config-edit-live", "config edit result", Plain),
    ConfigEditRestart { keys: &'a str } => ("config-edit-restart", "config edit result", Handlebars),
    DestructiveCommandDeny { explanation: &'a str } => ("destructive-command-gate-deny", "bash command risk gate", Handlebars),
    DestructiveCommandReflect { explanation: &'a str } => ("destructive-command-gate-reflect", "bash command risk gate", Handlebars),
    BackgroundTaskCompleted => ("background-task-completed", "server background completion wake", Plain),
    SwarmAwaitCompleted => ("swarm-await-completed", "server swarm await wake", Plain),
    SwarmMessageDirect { sender: String } => ("swarm-message-delivery-dm", "server peer-message wake", Handlebars),
    SwarmMessageChannel { channel: String, sender: String } => ("swarm-message-delivery-channel", "server peer-message wake", Handlebars),
    SwarmMessageBroadcast { sender: String } => ("swarm-message-delivery-broadcast", "server peer-message wake", Handlebars),
    StartupContextInitial => ("startup-context-initial", "Startup Context session install", Plain),
    StartupContextUpdate => ("startup-context-update", "Startup Context late apply", Plain),
    StartupContextStaleChanged => ("startup-context-stale-changed", "Startup Context observation", Handlebars),
    StartupContextStaleMissing => ("startup-context-stale-missing", "Startup Context observation", Handlebars),
    StartupContextStaleUnreadable => ("startup-context-stale-unreadable", "Startup Context observation", Handlebars),
    StartupContextStaleUnsupported => ("startup-context-stale-unsupported", "Startup Context observation", Handlebars),
    StartupContextStaleCurrent => ("startup-context-stale-current", "Startup Context observation", Handlebars),
    SessionFork { parent: &'a str, parent_id: &'a str } => ("session-fork", "session fork", Handlebars),
    SessionTransferHandoff => ("session-transfer-handoff", "session transfer", Plain),
    BatchNudge => ("batch-nudge", "agent turn loop", Plain),
    EmptyPostToolContinuation => ("empty-post-tool-continuation", "agent response recovery", Plain),
    IncompleteResponseContinuation { stop_reason: &'a str } => ("incomplete-response-continuation", "agent response recovery", Handlebars),
    StrandedToolUse => ("stranded-tool-use", "agent response recovery", Plain),
    FableGuardrailFirst => ("fable-guardrail-reconsideration-1", "agent response recovery", Plain),
    FableGuardrailSecond => ("fable-guardrail-reconsideration-2", "agent response recovery", Plain),
    FableGuardrailThird => ("fable-guardrail-reconsideration-3", "agent response recovery", Plain),
}

pub(super) fn module_seed_documents() -> Result<Vec<InstructionDocument>, InstructionError> {
    Ok(vec![InstructionDocument {
        id: InstructionId::parse("startup-context-stale-remainder")?,
        kind: InstructionKind::Module,
        scope: InstructionScope::Global,
        template_mode: TemplateMode::Plain,
        metadata: InstructionMetadata::default(),
        body: include_str!("notification/startup-context-stale-remainder.md").to_string(),
        path: std::path::PathBuf::from("modules/startup-context-stale-remainder.md"),
    }])
}

fn registration(id: &str, owner: &str) -> Result<ConsumerRegistration, InstructionError> {
    ConsumerRegistration::new(
        id,
        id,
        InstructionKind::Notification,
        format!("notifications/{id}.md"),
        owner,
        "Occurrence-rendered prose. Project redefinition is high impact. Delivery, structure, runtime data and trigger policy remain code-owned.",
    )
}

impl Notification<'_> {
    pub fn render(
        &self,
        working_dir: Option<&Path>,
    ) -> Result<String, SystemPromptActivationError> {
        self.render_with(&InstructionRepositoryService::new(), working_dir)
    }

    pub fn render_with(
        &self,
        repositories: &InstructionRepositoryService,
        working_dir: Option<&Path>,
    ) -> Result<String, SystemPromptActivationError> {
        let runtime = occurrence_runtime(repositories, working_dir)?;
        Ok(self.render_in(&runtime)?)
    }

    pub fn render_in(&self, runtime: &InstructionRuntime) -> Result<String, InstructionError> {
        InstructionConsumer::<Self>::new(self.registration()?)
            .render(runtime, self)
            .map(|rendered| rendered.text)
    }
}

fn occurrence_runtime(
    repositories: &InstructionRepositoryService,
    working_dir: Option<&Path>,
) -> Result<InstructionRuntime, SystemPromptActivationError> {
    let global = repositories.global_repository()?;
    let state = repositories.inspect(&global)?;
    match state.health {
        InstructionRepositoryHealth::Uninitialized => {
            SystemPromptComposer::from_repository_service(repositories.clone())
                .ensure_global_store()?;
        }
        InstructionRepositoryHealth::Ready => {
            let manifest = repositories.load_manifest(&global)?;
            if manifest.seed_version != INSTRUCTION_STORE_SEED_VERSION {
                SystemPromptComposer::from_repository_service(repositories.clone())
                    .ensure_global_store()?;
            }
        }
        InstructionRepositoryHealth::Damaged(damage) => {
            return Err(SystemPromptActivationError::Compatibility(format!(
                "cannot render notification from {}: {}",
                global.root.display(),
                damage.detail
            )));
        }
    }
    let project = working_dir
        .map(|dir| repositories.resolve_project_repository(dir))
        .transpose()?
        .flatten();
    if let Some(project) = &project {
        repositories.load_manifest(project)?;
    }
    // Ordinary reads validate only selected resources and their dependencies.
    // An unrelated broken agent must not suppress a valid completion notice.
    Ok(InstructionRuntime::discover(
        repositories.instruction_sources(project.as_ref())?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_resource(root: &Path, id: &str, body: &str) {
        std::fs::create_dir_all(root.join("notifications")).unwrap();
        std::fs::write(
            root.join(format!("notifications/{id}.md")),
            format!("---\nid: {id}\nkind: notification\ntemplate: handlebars\n---\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn occurrence_reads_current_source_and_preserves_prior_result() {
        let temp = tempfile::tempdir().unwrap();
        let service = InstructionRepositoryService::from_paths(
            temp.path().join("home"),
            temp.path().join("state"),
        );
        SystemPromptComposer::from_repository_service(service.clone())
            .ensure_global_store()
            .unwrap();
        let root = service.global_repository().unwrap().root;
        let event = Notification::IncompleteResponseContinuation {
            stop_reason: "<&\"fixture\">",
        };
        write_resource(
            &root,
            "incomplete-response-continuation",
            "OLD {{stop_reason}}",
        );
        let old = event.render_with(&service, None).unwrap();
        write_resource(
            &root,
            "incomplete-response-continuation",
            "NEW {{stop_reason}}",
        );
        assert_eq!(
            event.render_with(&service, None).unwrap(),
            "NEW <&\"fixture\">"
        );
        assert_eq!(old, "OLD <&\"fixture\">");
        // An unrelated invalid source is inspectable, not an event-wide block.
        std::fs::write(
            root.join("agents/unrelated.md"),
            "---\nkind: agent\nid: unrelated\nbad: value\n---\n",
        )
        .unwrap();
        assert_eq!(
            event.render_with(&service, None).unwrap(),
            "NEW <&\"fixture\">"
        );
        write_resource(&root, "incomplete-response-continuation", "{{unknown}}");
        assert!(event.render_with(&service, None).is_err());
        assert_eq!(old, "OLD <&\"fixture\">");
    }

    #[test]
    fn project_empty_invalid_and_deleted_definition_keep_specificity() {
        let temp = tempfile::tempdir().unwrap();
        let global = temp.path().join("global");
        let project = temp.path().join("project");
        write_resource(
            &global,
            "incomplete-response-continuation",
            "GLOBAL {{stop_reason}}",
        );
        write_resource(
            &project,
            "incomplete-response-continuation",
            "PROJECT {{stop_reason}}",
        );
        let render = || {
            Notification::IncompleteResponseContinuation {
                stop_reason: "fixture",
            }
            .render_in(&InstructionRuntime::discover(
                InstructionSources::new(&global).with_project_root(&project),
            ))
        };
        assert_eq!(render().unwrap(), "PROJECT fixture");
        write_resource(&project, "incomplete-response-continuation", "");
        assert_eq!(render().unwrap(), "");
        write_resource(&project, "incomplete-response-continuation", "{{missing}}");
        assert!(render().is_err());
        std::fs::remove_file(project.join("notifications/incomplete-response-continuation.md"))
            .unwrap();
        assert_eq!(render().unwrap(), "GLOBAL fixture");
        std::fs::remove_file(global.join("notifications/incomplete-response-continuation.md"))
            .unwrap();
        assert!(matches!(
            render(),
            Err(InstructionError::RegisteredResourceMissing { .. })
        ));
    }
}
