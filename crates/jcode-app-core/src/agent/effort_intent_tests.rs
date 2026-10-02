//! INT-01/WP-06 R24: a session persists what it asked for as its effort
//! (`Default` or `Explicit(level)`), and the effective value follows the
//! current model across switches, saves, loads and resumes.

use super::*;
use crate::provider::EventStream;
use jcode_session_types::StoredReasoningEffortIntent as Intent;
use std::sync::Mutex as StdMutex;

/// A runtime whose default effort and ladder depend on the model, as the
/// production runtimes' do:
///
/// | model | default | offers |
/// |---|---|---|
/// | `effort-model-a` | `low` | `none`, `low`, `medium`, `high` |
/// | `effort-model-b` | `high` | `low`, `medium`, `high`, `xhigh` |
#[derive(Default)]
pub(crate) struct ModelDefaultsProvider {
    model: StdMutex<Option<String>>,
    chosen: StdMutex<Option<String>>,
}

pub(crate) const MODEL_A: &str = "effort-model-a";
pub(crate) const MODEL_B: &str = "effort-model-b";

#[async_trait::async_trait]
impl Provider for ModelDefaultsProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        panic!("effort intent tests send no requests")
    }

    fn name(&self) -> &str {
        "effort-fixture"
    }

    fn model(&self) -> String {
        self.model
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| MODEL_A.to_string())
    }

    fn set_model(&self, model: &str) -> Result<()> {
        anyhow::ensure!(
            [MODEL_A, MODEL_B].contains(&model),
            "unknown fixture model {model}"
        );
        *self.model.lock().unwrap() = Some(model.to_string());
        // As the production runtimes do, keep a chosen level only as far as
        // the new model takes it: the session's intent restores the rest.
        let mut chosen = self.chosen.lock().unwrap();
        if chosen
            .as_deref()
            .is_some_and(|level| !self.available_efforts().contains(&level))
        {
            *chosen = None;
        }
        Ok(())
    }

    fn available_models_for_switching(&self) -> Vec<String> {
        vec![MODEL_A.to_string(), MODEL_B.to_string()]
    }

    fn reasoning_effort(&self) -> Option<String> {
        self.chosen.lock().unwrap().clone().or_else(|| {
            Some(
                if self.model() == MODEL_A {
                    "low"
                } else {
                    "high"
                }
                .to_string(),
            )
        })
    }

    fn set_reasoning_effort(&self, effort: &str) -> Result<()> {
        anyhow::ensure!(
            self.available_efforts().contains(&effort),
            "effort {effort:?} is not offered by {}",
            self.model()
        );
        *self.chosen.lock().unwrap() = Some(effort.to_string());
        Ok(())
    }

    fn reset_reasoning_effort(&self) -> Result<()> {
        *self.chosen.lock().unwrap() = None;
        Ok(())
    }

    fn available_efforts(&self) -> Vec<&'static str> {
        if self.model() == MODEL_A {
            vec!["none", "low", "medium", "high"]
        } else {
            vec!["low", "medium", "high", "xhigh"]
        }
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            model: StdMutex::new(Some(self.model())),
            chosen: StdMutex::new(self.chosen.lock().unwrap().clone()),
        })
    }
}

struct Fixture {
    _guards: (
        super::tests::AgentTestEnvRestore,
        super::tests::AgentTestEnvRestore,
        tempfile::TempDir,
        std::sync::MutexGuard<'static, ()>,
    ),
}

fn fixture() -> Result<Fixture> {
    let lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let home_guard = super::tests::AgentTestEnvRestore::set_path("JCODE_HOME", home.path());
    let runtime_guard = super::tests::AgentTestEnvRestore::set_path(
        "JCODE_RUNTIME_DIR",
        &home.path().join("runtime"),
    );
    crate::config::invalidate_config_cache();
    Ok(Fixture {
        _guards: (runtime_guard, home_guard, home, lock),
    })
}

/// A process start: the persisted session in a fresh Agent on a fresh
/// runtime, which begins on its own default model and effort.
async fn resume(session_id: &str) -> Result<(Arc<ModelDefaultsProvider>, Agent)> {
    let session = crate::session::Session::load(session_id)?;
    let provider = Arc::new(ModelDefaultsProvider::default());
    let dynamic: Arc<dyn Provider> = provider.clone();
    let registry = Registry::new(Arc::clone(&dynamic)).await;
    let agent = Agent::new_with_session(dynamic, registry, session, None);
    Ok((provider, agent))
}

async fn new_session() -> Result<(Arc<ModelDefaultsProvider>, Agent)> {
    let mut session = crate::session::Session::create(None, None);
    session.model = Some(MODEL_A.to_string());
    session.save()?;
    resume(&session.id.clone()).await
}

fn switch(agent: &mut Agent, model: &str) -> Result<()> {
    agent.set_model(model)
}

/// The sequence the review reproduced (GPT F4): the default on one model must
/// not come back as a chosen level on another after a reload.
#[tokio::test]
async fn a_default_effort_follows_the_model_across_a_switch_and_two_resumes() -> Result<()> {
    let _fixture = fixture()?;
    let (provider, mut agent) = new_session().await?;
    assert_eq!(agent.session.reasoning_effort_intent, Some(Intent::Default));

    assert_eq!(agent.set_reasoning_effort("high")?.as_deref(), Some("high"));
    assert_eq!(
        agent.set_reasoning_effort("default")?.as_deref(),
        Some("low")
    );
    assert_eq!(agent.session.reasoning_effort_intent, Some(Intent::Default));

    switch(&mut agent, MODEL_B)?;
    assert_eq!(provider.reasoning_effort().as_deref(), Some("high"));
    assert_eq!(agent.session.reasoning_effort.as_deref(), Some("high"));
    agent.session.save()?;
    let session_id = agent.session.id.clone();

    for _ in 0..2 {
        let (provider, mut agent) = resume(&session_id).await?;
        assert_eq!(provider.model(), MODEL_B);
        assert_eq!(provider.reasoning_effort().as_deref(), Some("high"));
        assert_eq!(agent.session.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(agent.session.reasoning_effort_intent, Some(Intent::Default));
        agent.session.save()?;
    }
    Ok(())
}

/// A chosen level is applied again after every switch. A model that lacks
/// it runs at its default while it is current; the intent keeps the level.
#[tokio::test]
async fn a_chosen_effort_is_kept_across_switches_and_resumes() -> Result<()> {
    let _fixture = fixture()?;
    let (provider, mut agent) = new_session().await?;
    switch(&mut agent, MODEL_B)?;
    assert_eq!(
        agent.set_reasoning_effort("xhigh")?.as_deref(),
        Some("xhigh")
    );
    let chosen = Some(Intent::Explicit {
        level: "xhigh".to_string(),
    });
    assert_eq!(agent.session.reasoning_effort_intent, chosen);

    // Model A has no `xhigh`.
    switch(&mut agent, MODEL_A)?;
    assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));
    assert_eq!(agent.session.reasoning_effort.as_deref(), Some("low"));
    assert_eq!(agent.session.reasoning_effort_intent, chosen);

    switch(&mut agent, MODEL_B)?;
    assert_eq!(provider.reasoning_effort().as_deref(), Some("xhigh"));
    agent.session.save()?;
    let session_id = agent.session.id.clone();

    for _ in 0..2 {
        let (provider, mut agent) = resume(&session_id).await?;
        assert_eq!(provider.reasoning_effort().as_deref(), Some("xhigh"));
        assert_eq!(agent.session.reasoning_effort_intent, chosen);
        agent.session.save()?;
    }

    // A level both models offer stays as chosen on each.
    let (provider, mut agent) = resume(&session_id).await?;
    agent.set_reasoning_effort("medium")?;
    switch(&mut agent, MODEL_A)?;
    assert_eq!(provider.reasoning_effort().as_deref(), Some("medium"));
    Ok(())
}

/// Sessions stored before intents existed keep the effort they ran with: a
/// stored value equal to the model's default becomes `Default`, any other
/// stays chosen.
#[tokio::test]
async fn a_session_stored_before_intents_keeps_its_effective_effort() -> Result<()> {
    let _fixture = fixture()?;
    for (model, stored, intent) in [
        (MODEL_A, Some("low"), Intent::Default),
        (MODEL_B, Some("high"), Intent::Default),
        (MODEL_B, None, Intent::Default),
        (
            MODEL_B,
            Some("low"),
            Intent::Explicit {
                level: "low".to_string(),
            },
        ),
    ] {
        let mut session = crate::session::Session::create(None, None);
        session.model = Some(model.to_string());
        session.reasoning_effort = stored.map(str::to_string);
        session.save()?;
        let (provider, agent) = resume(&session.id).await?;
        let expected = stored.unwrap_or("high");
        assert_eq!(provider.reasoning_effort().as_deref(), Some(expected));
        assert_eq!(agent.session.reasoning_effort.as_deref(), Some(expected));
        assert_eq!(agent.session.reasoning_effort_intent, Some(intent));
    }
    Ok(())
}

/// A session change inside one process must not inherit the previous
/// session's chosen level through the shared runtime.
#[tokio::test]
async fn a_restored_session_does_not_inherit_the_previous_sessions_level() -> Result<()> {
    let _fixture = fixture()?;
    let (provider, mut agent) = new_session().await?;
    let mut other = crate::session::Session::create(None, None);
    other.model = Some(MODEL_A.to_string());
    other.reasoning_effort_intent = Some(Intent::Default);
    other.save()?;

    agent.set_reasoning_effort("high")?;
    agent.restore_session(&other.id)?;
    assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));
    assert_eq!(agent.session.reasoning_effort_intent, Some(Intent::Default));
    Ok(())
}
