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

        /// Typed synthetic values for human draft validation, never execution.
        pub(crate) fn preview_values(id: &str) -> Result<Option<serde_json::Value>, serde_json::Error> {
            let value = match id {
                $( $id => Workflow::$variant $( { $( $field: Default::default() ),* } )? ),*,
                _ => return Ok(None),
            };
            serde_json::to_value(value).map(Some)
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
    SwarmRouting => ("swarm-routing", ToolGuidance, "tools", "swarm tool routing guidance", Plain, ProjectThenGlobal),
    SwarmComposite { item_id: &'a str } => ("swarm-composite-synthesis", Notification, "notifications", "swarm task control", Handlebars, ProjectThenGlobal),
    SwarmAssignmentAddendum => ("swarm-coordinator-assignment-addendum", Notification, "notifications", "swarm task control", Plain, ProjectThenGlobal),
    SwarmTaskResume => ("swarm-task-resume", Notification, "notifications", "swarm task control", Plain, ProjectThenGlobal),
    SwarmTaskRetryFull => ("swarm-task-retry-full", Notification, "notifications", "swarm task control", Plain, ProjectThenGlobal),
    SwarmTaskRetry => ("swarm-task-retry", Notification, "notifications", "swarm task control", Plain, ProjectThenGlobal),
    SwarmTaskAssigned => ("swarm-task-assigned", Notification, "notifications", "swarm task control", Plain, ProjectThenGlobal),
    SwarmTaskWake { task_id: &'a str } => ("swarm-task-wake", Notification, "notifications", "swarm task control", Handlebars, ProjectThenGlobal),
    SwarmStandDown { task_id: &'a str, target: &'a str, action: &'a str } => ("swarm-task-stand-down", Notification, "notifications", "swarm task control", Handlebars, ProjectThenGlobal),
    SwarmSalvage { label: &'a str } => ("swarm-task-salvage", Notification, "notifications", "swarm task control", Handlebars, ProjectThenGlobal),
    SwarmReplace { assignee: &'a str } => ("swarm-task-replacement", Notification, "notifications", "swarm task control", Handlebars, ProjectThenGlobal),
    SwarmPlanner { request: &'a str, working_dir: &'a str } => ("swarm-task-planner", Module, "modules", "swarm task control", Plain, ProjectThenGlobal),
    SwarmIntegrator { request: &'a str, outputs: &'a str } => ("swarm-result-integrator", Module, "modules", "swarm task control", Plain, ProjectThenGlobal),
    SwarmIntegratorFinish => ("swarm-result-integrator-finish", Module, "modules", "swarm task control", Plain, ProjectThenGlobal),
    SwarmWorkerReport => ("swarm-worker-report-contract", Notification, "notifications", "swarm worker contracts", Plain, ProjectThenGlobal),
    SwarmDeepNodeBounded { node_id: &'a str, member_cap: usize } => ("swarm-deep-node-bounded", Notification, "notifications", "swarm worker contracts", Handlebars, ProjectThenGlobal),
    SwarmDeepNode { node_id: &'a str, member_cap: usize } => ("swarm-deep-node-contract", Notification, "notifications", "swarm worker contracts", Handlebars, ProjectThenGlobal),
    SwarmDeepGate { gate_id: &'a str } => ("swarm-deep-gate-contract", Notification, "notifications", "swarm worker contracts", Handlebars, ProjectThenGlobal),
    SwarmDeepGateScope { ids: &'a str } => ("swarm-deep-gate-scope", Notification, "notifications", "swarm worker contracts", Handlebars, ProjectThenGlobal),
    SwarmDeepGatePriority { ids: &'a str } => ("swarm-deep-gate-priority", Notification, "notifications", "swarm worker contracts", Handlebars, ProjectThenGlobal),
    SwarmDeepGateFinish => ("swarm-deep-gate-finish", Notification, "notifications", "swarm worker contracts", Plain, ProjectThenGlobal),
    CommandCommit => ("workflow-commit", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandCommitPush => ("workflow-commit-push", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandReleaseSelect => ("workflow-release-select", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandReleaseMetadata => ("workflow-release-metadata", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandReleaseFinish => ("workflow-release-finish", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandReleaseLayout { preparation: &'a str, publication: &'a str } => ("workflow-release-layout", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandReleaseFast { preparation: &'a str, publication: &'a str } => ("workflow-release-fast", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandReleaseFastPrepare => ("workflow-release-fast-prepare", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandReleaseFastPublish => ("workflow-release-fast-publish", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandReleaseMacos { preparation: &'a str, publication: &'a str } => ("workflow-release-fast-macos", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandReleaseMacosPrepare => ("workflow-release-fast-macos-prepare", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandReleaseMacosPublish => ("workflow-release-fast-macos-publish", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandReleaseRemote { preparation: &'a str, publication: &'a str } => ("workflow-release-remote", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandReleaseRemotePublish => ("workflow-release-remote-publish", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandTriage => ("workflow-github-triage", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandTriageFocus { focus: &'a str } => ("workflow-triage-focus", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandTest { target: &'a str } => ("workflow-test-verification", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandTestDefault => ("workflow-test-default-target", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandPlan { goal_line: &'a str } => ("workflow-plan", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandPlanDefault => ("workflow-plan-default-goal", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandImproveFocus { focus: &'a str } => ("workflow-improve-focus", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandImprove { focus_line: &'a str } => ("workflow-improve", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandImprovePlan { focus_line: &'a str } => ("workflow-improve-plan", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandImproveStop => ("workflow-improve-stop", Notification, "notifications", "command workflow", Plain, ProjectThenGlobal),
    CommandImproveResumeRun { todo_rows: &'a str, count: usize, plural: &'a str } => ("workflow-improve-resume-run", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandImproveResumeRunEmpty => ("workflow-improve-resume-run-empty", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandImproveResumePlan { todo_rows: &'a str, count: usize, plural: &'a str } => ("workflow-improve-resume-plan", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandImproveResumePlanEmpty => ("workflow-improve-resume-plan-empty", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandImproveResumeOther { todo_rows: &'a str, count: usize, plural: &'a str } => ("workflow-improve-resume-other", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandImproveResumeOtherEmpty => ("workflow-improve-resume-other-empty", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandRefactorFocus { focus: &'a str } => ("workflow-refactor-focus", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandRefactor { focus_line: &'a str } => ("workflow-refactor", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandRefactorPlan { focus_line: &'a str } => ("workflow-refactor-plan", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandRefactorStop => ("workflow-refactor-stop", Notification, "notifications", "command workflow", Plain, ProjectThenGlobal),
    CommandRefactorResumeRun { todo_rows: &'a str, count: usize, plural: &'a str } => ("workflow-refactor-resume-run", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandRefactorResumeRunEmpty => ("workflow-refactor-resume-run-empty", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandRefactorResumePlan { todo_rows: &'a str, count: usize, plural: &'a str } => ("workflow-refactor-resume-plan", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandRefactorResumePlanEmpty => ("workflow-refactor-resume-plan-empty", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    CommandRefactorResumeOther { todo_rows: &'a str, count: usize, plural: &'a str } => ("workflow-refactor-resume-other", Module, "modules", "command workflow", Handlebars, ProjectThenGlobal),
    CommandRefactorResumeOtherEmpty => ("workflow-refactor-resume-other-empty", Module, "modules", "command workflow", Plain, ProjectThenGlobal),
    MissionIntroduction { objective: &'a str, long_horizon_intent: &'a str } => ("mission-introduction", Module, "modules", "mission turn", Plain, ProjectThenGlobal),
    MissionContinuation { objective: &'a str, long_horizon_intent: &'a str } => ("mission-continuation", Module, "modules", "mission turn", Plain, ProjectThenGlobal),
    MissionDefaultIntent { objective: &'a str } => ("mission-default-intent", Module, "modules", "mission creation", Handlebars, ProjectThenGlobal),
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
    fn is_legacy_swarm_workflow(&self) -> bool {
        matches!(
            self,
            Self::ReviewStartup { .. }
                | Self::AutoreviewStartup { .. }
                | Self::JudgeStartup { .. }
                | Self::AutojudgeStartup { .. }
                | Self::OvernightCoordinator { .. }
                | Self::OvernightVisible { .. }
                | Self::OvernightDefaultMission
                | Self::OvernightContinuation { .. }
                | Self::OvernightHandoff { .. }
                | Self::OvernightMorning { .. }
                | Self::OvernightPostWake { .. }
                | Self::OvernightFinal { .. }
                | Self::OvernightPokeIntro { .. }
                | Self::OvernightPokeDiagnostic { .. }
                | Self::OvernightPokeHandoff
                | Self::OvernightPokeMorningReport
                | Self::OvernightPokePostWake
                | Self::OvernightPokeFinalWrap
                | Self::OvernightPokeContinue
        )
    }

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
        if self.is_legacy_swarm_workflow() && !crate::config::config().features.swarm {
            return Err(SystemPromptActivationError::Compatibility(
                crate::config::SWARM_WORKFLOW_UNAVAILABLE.into(),
            ));
        }
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

mod commands;
pub use commands::render_command;
