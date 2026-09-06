//! Managed workflow prose. Existing callers retain execution and delivery policy.

use super::*;
use serde::Serialize;
use std::path::Path;

macro_rules! workflows {
    ($( $variant:ident $( { $( $field:ident : $ty:ty ),* $(,)? } )? => ($id:literal, $kind:ident, $directory:literal, $owner:literal, $mode:ident, $scope:ident) ),* $(,)?) => {
        #[derive(Debug, Serialize)]
        #[serde(untagged)]
        pub enum Workflow<'a> {
            $( $variant $( { $( $field: $ty ),* } )? ),*
        }

        impl Workflow<'_> {
            pub fn registration(&self) -> Result<ConsumerRegistration, InstructionError> {
                match self {
                    $( Self::$variant $( { $( $field: _ ),* } )? => registration($id, InstructionKind::$kind, $directory, $owner, ConsumerScopePolicy::$scope) ),*
                }
            }
        }

        pub fn registrations() -> Result<Vec<ConsumerRegistration>, InstructionError> {
            vec![$( registration($id, InstructionKind::$kind, $directory, $owner, ConsumerScopePolicy::$scope) ),*].into_iter().collect()
        }

        pub(super) fn seed_documents() -> Result<Vec<InstructionDocument>, InstructionError> {
            Ok(vec![$( InstructionDocument {
                id: InstructionId::parse($id)?,
                kind: InstructionKind::$kind,
                scope: InstructionScope::Global,
                template_mode: TemplateMode::$mode,
                metadata: InstructionMetadata::default(),
                body: include_str!(concat!("workflow/", $id, ".md")).to_string(),
                path: PathBuf::from(concat!($directory, "/", $id, ".md")),
            } ),*])
        }
    };
}

workflows! {
    ReviewReadOnly => ("review-read-only-guardrails", Module, "modules", "review startup", Plain, ProjectThenGlobal),
    JudgeVisibleContext => ("judge-visible-context", Module, "modules", "judge startup", Plain, ProjectThenGlobal),
    ReviewStartup { parent_session_id: &'a str } => ("review-startup", Module, "modules", "review/judge child startup", Handlebars, ProjectThenGlobal),
    AutoreviewStartup { parent_session_id: &'a str } => ("autoreview-startup", Module, "modules", "review/judge child startup", Handlebars, ProjectThenGlobal),
    JudgeStartup { parent_session_id: &'a str } => ("judge-startup", Module, "modules", "review/judge child startup", Handlebars, ProjectThenGlobal),
    AutojudgeStartup { parent_session_id: &'a str } => ("autojudge-startup", Module, "modules", "review/judge child startup", Handlebars, ProjectThenGlobal),
    OvernightPokeIntro { manifest_path: &'a str, review_notes: &'a str, task_cards: &'a str, validation: &'a str } => ("overnight-poke-intro", Notification, "notifications", "overnight visible follow-up", Handlebars, ProjectThenGlobal),
    OvernightPokeDiagnostic { stalled_turns: u8 } => ("overnight-poke-diagnostic", Notification, "notifications", "overnight visible follow-up", Handlebars, ProjectThenGlobal),
    OvernightPokeHandoff => ("overnight-poke-handoff", Notification, "notifications", "overnight visible follow-up", Plain, ProjectThenGlobal),
    OvernightPokeMorningReport => ("overnight-poke-morning", Notification, "notifications", "overnight visible follow-up", Plain, ProjectThenGlobal),
    OvernightPokePostWake => ("overnight-poke-post-wake", Notification, "notifications", "overnight visible follow-up", Plain, ProjectThenGlobal),
    OvernightPokeFinalWrap => ("overnight-poke-final", Notification, "notifications", "overnight visible follow-up", Plain, ProjectThenGlobal),
    OvernightPokeContinue => ("overnight-poke-continue", Notification, "notifications", "overnight visible follow-up", Plain, ProjectThenGlobal),
    OvernightCoordinator { run_id: &'a str, target_wake_at: &'a str, post_wake_grace_until: &'a str, mission: &'a str, issue_drafts: &'a str, review_notes: &'a str, task_cards: &'a str, task_card_schema: &'a str, validation: &'a str, review_html: &'a str, preflight_summary: &'a str } => ("overnight-coordinator", Module, "modules", "overnight runner", Handlebars, ProjectThenGlobal),
    OvernightVisible { run_id: &'a str, target_wake_at: &'a str, post_wake_grace_until: &'a str, mission: &'a str, review_notes: &'a str, task_cards: &'a str, task_card_schema: &'a str, validation: &'a str, review_html: &'a str, manifest_path: &'a str } => ("overnight-visible-coordinator", Module, "modules", "overnight runner", Handlebars, ProjectThenGlobal),
    OvernightContinuation { remaining: &'a str, review_notes: &'a str } => ("overnight-continuation", Notification, "notifications", "overnight runner", Handlebars, ProjectThenGlobal),
    OvernightHandoff { review_notes: &'a str } => ("overnight-handoff-ready", Notification, "notifications", "overnight runner", Handlebars, ProjectThenGlobal),
    OvernightMorning { review_notes: &'a str, review_html: &'a str } => ("overnight-morning-report", Notification, "notifications", "overnight runner", Handlebars, ProjectThenGlobal),
    OvernightPostWake { review_notes: &'a str, post_wake_grace_until: &'a str } => ("overnight-post-wake", Notification, "notifications", "overnight runner", Handlebars, ProjectThenGlobal),
    OvernightFinal { review_notes: &'a str, review_html: &'a str } => ("overnight-final-wrap", Notification, "notifications", "overnight runner", Handlebars, ProjectThenGlobal),
    OvernightDefaultMission => ("overnight-default-mission", Module, "modules", "overnight runner", Plain, ProjectThenGlobal),
    StructuredOutput { schema: &'a str } => ("structured-output", Module, "modules", "structured SDK initial prompt", Plain, ProjectThenGlobal),
    StructuredCorrection { schema: &'a str, error_lines: &'a str, previous_response: &'a str } => ("structured-output-correction", Notification, "notifications", "structured SDK correction", Plain, ProjectThenGlobal),
    SwarmEffort => ("swarm-effort", System, "system", "request dynamic effort directive", Plain, ProjectThenGlobal),
    SwarmDeepEffort => ("swarm-deep-effort", System, "system", "request dynamic effort directive", Plain, ProjectThenGlobal),
    AmbientIdentity => ("ambient-identity", Module, "modules", "ambient cycle", Plain, GlobalOnly),
    AmbientEmptyQueue => ("ambient-empty-queue", Module, "modules", "ambient cycle", Plain, GlobalOnly),
    AmbientDirectives => ("ambient-directives", Module, "modules", "ambient cycle", Plain, GlobalOnly),
    AmbientInstructions => ("ambient-instructions", Module, "modules", "ambient cycle", Plain, GlobalOnly),
    AmbientCycleStart => ("ambient-cycle-start", Module, "modules", "ambient cycle", Plain, GlobalOnly),
    TransferHandoffTask => ("transfer-handoff-task", Module, "modules", "transfer summarizer user prompt", Plain, ProjectThenGlobal),
    TransferHandoffSystem => ("transfer-handoff-system", System, "system", "transfer summarizer system prompt", Plain, ProjectThenGlobal),
}

fn registration(
    id: &str,
    kind: InstructionKind,
    directory: &str,
    owner: &str,
    scope_policy: ConsumerScopePolicy,
) -> Result<ConsumerRegistration, InstructionError> {
    let mut registration = ConsumerRegistration::new(
        id,
        id,
        kind,
        format!("{directory}/{id}.md"),
        owner,
        "Invocation-rendered workflow prose. Project redefinition is high impact. Execution, framing, permissions and runtime data remain caller-owned.",
    )?;
    registration.scope_policy = scope_policy;
    Ok(registration)
}

impl Workflow<'_> {
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
        let runtime = super::notification::occurrence_runtime(repositories, working_dir)?;
        Ok(self.render_in(&runtime)?)
    }

    pub fn render_in(&self, runtime: &InstructionRuntime) -> Result<String, InstructionError> {
        InstructionConsumer::<Self>::new(self.registration()?)
            .render(runtime, self)
            .map(|rendered| rendered.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_scope_and_damage_follow_registered_consumer_policy() {
        let temp = tempfile::tempdir().unwrap();
        let global = temp.path().join("global");
        let project = temp.path().join("project");
        let write = |root: &Path, resource: Workflow<'_>, body: &str| {
            let registration = resource.registration().unwrap();
            let path = root.join(registration.default_relative_path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!(
                    "---\nid: {}\nkind: {}\ntemplate: handlebars\n---\n{body}",
                    registration.id, registration.kind
                ),
            )
            .unwrap();
        };
        for root in [&global, &project] {
            write(
                root,
                Workflow::AmbientIdentity,
                if root == &global { "GLOBAL" } else { "PROJECT" },
            );
            write(
                root,
                Workflow::SwarmEffort,
                if root == &global { "GLOBAL" } else { "PROJECT" },
            );
        }
        let runtime = || {
            InstructionRuntime::discover(
                InstructionSources::new(&global).with_project_root(&project),
            )
        };
        assert_eq!(
            Workflow::AmbientIdentity.render_in(&runtime()).unwrap(),
            "GLOBAL"
        );
        assert_eq!(
            Workflow::SwarmEffort.render_in(&runtime()).unwrap(),
            "PROJECT"
        );
        write(&project, Workflow::SwarmEffort, "");
        assert_eq!(Workflow::SwarmEffort.render_in(&runtime()).unwrap(), "");
        write(&project, Workflow::SwarmEffort, "{{missing}}");
        assert!(Workflow::SwarmEffort.render_in(&runtime()).is_err());
        std::fs::remove_file(project.join("system/swarm-effort.md")).unwrap();
        assert_eq!(
            Workflow::SwarmEffort.render_in(&runtime()).unwrap(),
            "GLOBAL"
        );
        std::fs::remove_file(global.join("system/swarm-effort.md")).unwrap();
        assert!(matches!(
            Workflow::SwarmEffort.render_in(&runtime()),
            Err(InstructionError::RegisteredResourceMissing { .. })
        ));
    }
}
