//! Managed workflow prose. Existing callers retain execution and delivery policy.

use super::*;
use serde::Serialize;
use std::path::Path;

macro_rules! workflows {
    ($( $variant:ident => ($id:literal, $kind:ident, $directory:literal, $owner:literal, $mode:ident) ),* $(,)?) => {
        #[derive(Debug, Serialize)]
        pub enum Workflow {
            $( $variant ),*
        }

        impl Workflow {
            pub fn registration(&self) -> Result<ConsumerRegistration, InstructionError> {
                match self {
                    $( Self::$variant => registration($id, InstructionKind::$kind, $directory, $owner) ),*
                }
            }
        }

        pub fn registrations() -> Result<Vec<ConsumerRegistration>, InstructionError> {
            vec![$( registration($id, InstructionKind::$kind, $directory, $owner) ),*].into_iter().collect()
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
    AmbientIdentity => ("ambient-identity", Module, "modules", "ambient cycle", Plain),
    AmbientEmptyQueue => ("ambient-empty-queue", Module, "modules", "ambient cycle", Plain),
    AmbientDirectives => ("ambient-directives", Module, "modules", "ambient cycle", Plain),
    AmbientInstructions => ("ambient-instructions", Module, "modules", "ambient cycle", Plain),
    AmbientCycleStart => ("ambient-cycle-start", Module, "modules", "ambient cycle", Plain),
    TransferHandoffTask => ("transfer-handoff-task", Module, "modules", "transfer summarizer user prompt", Plain),
    TransferHandoffSystem => ("transfer-handoff-system", System, "system", "transfer summarizer system prompt", Plain),
}

fn registration(
    id: &str,
    kind: InstructionKind,
    directory: &str,
    owner: &str,
) -> Result<ConsumerRegistration, InstructionError> {
    ConsumerRegistration::new(
        id,
        id,
        kind,
        format!("{directory}/{id}.md"),
        owner,
        "Invocation-rendered workflow prose. Project redefinition is high impact. Execution, framing, permissions and runtime data remain caller-owned.",
    )
}

impl Workflow {
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
