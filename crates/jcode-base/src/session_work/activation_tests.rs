use super::*;
use crate::config::feature_override::ScopedFeatureOverride;
use crate::instruction::shipped_instruction_seed;
use crate::session_work::SessionWorkStore;
use std::path::PathBuf;

struct Fixture {
    temp: tempfile::TempDir,
    repositories: InstructionRepositoryService,
    surface: SessionWorkSurface,
    global: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let repositories =
            InstructionRepositoryService::from_paths(&home, temp.path().join("state"));
        repositories
            .initialize_global(&shipped_instruction_seed().unwrap(), &[])
            .unwrap();
        let global = repositories.global_repository().unwrap().root;
        let surface = SessionWorkSurface::at(
            temp.path().join("home/session-work"),
            SessionWorkStore::at(temp.path().join("state/session-work")),
        );
        Self {
            temp,
            repositories,
            surface,
            global,
        }
    }

    fn module_type(&self, id: &str, name: &str, extra: &str) {
        let dir = self.global.join("module-types");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{id}.md")),
            format!(
                "---\nkind: module-type\nid: {id}\nname: {name}\ndescription: Synthetic {id} type\n{extra}---\nSynthetic body.\n"
            ),
        )
        .unwrap();
    }

    fn session(&self, id: &str) -> Session {
        let mut session = Session::create_with_id(id.into(), None, None);
        session.working_dir = Some(self.temp.path().to_string_lossy().into_owned());
        session
    }

    fn activate(&self, session: &mut Session, kind: NewSessionWork<'_>) -> bool {
        activate_with(session, kind, &self.repositories, &self.surface).unwrap()
    }
}

#[test]
fn nothing_is_activated_while_the_flag_is_off() {
    let _env = crate::storage::lock_test_env();
    let _off = ScopedFeatureOverride::session_work(false);
    let fixture = Fixture::new();
    let mut session = fixture.session("session_off");
    assert!(!fixture.activate(&mut session, NewSessionWork::Primary));
    assert!(!fixture.activate(
        &mut session,
        NewSessionWork::Child {
            preset: "global:task-preset.general",
            template: Some("- [>] a: A\n"),
        }
    ));
    assert_eq!(session.session_work, None);
    assert!(!fixture.surface.store().exists().unwrap());
    assert!(!fixture.surface.session_dir("session_off").unwrap().exists());
    session.ensure_initial_session_context_message();
    assert!(!format!("{:?}", session.messages).contains("session-work"));
}

#[test]
fn a_new_primary_gets_its_binding_context_line_and_frozen_module_types() {
    let _env = crate::storage::lock_test_env();
    let _on = ScopedFeatureOverride::session_work(true);
    let fixture = Fixture::new();
    fixture.module_type(
        "research",
        "Research",
        "subtypes:\n  - web\n  - code\nskill: global:research\n",
    );
    let mut session = fixture.session("session_primary");
    assert!(fixture.activate(&mut session, NewSessionWork::Primary));
    let binding = session.session_work.clone().unwrap();
    assert_eq!(binding.role, SessionWorkRole::Primary);
    let workflow = fixture.surface.workflow_path("session_primary").unwrap();
    let skill = fixture.global.join("skills/session-work/SKILL.md");
    assert!(binding.context_line.contains(&*workflow.to_string_lossy()));
    assert!(binding.context_line.contains(&*skill.to_string_lossy()));
    assert!(
        fixture
            .surface
            .session_dir("session_primary")
            .unwrap()
            .is_dir()
    );
    assert!(
        !workflow.exists(),
        "no workflow exists until the agent writes one"
    );

    session.ensure_initial_session_context_message();
    let context = format!("{:?}", session.messages);
    assert!(context.contains(&*workflow.to_string_lossy()));

    let store = fixture.surface.store();
    let frozen = crate::session_work::frozen_module_types(store, "session_primary").unwrap();
    assert_eq!(frozen.len(), 1);
    assert_eq!(frozen[0].id, "global:research");
    assert_eq!(frozen[0].title, "Research");
    assert_eq!(frozen[0].subtypes, ["web", "code"]);
    assert_eq!(frozen[0].skill.as_deref(), Some("global:research"));

    // Later registry edits reach new sessions only.
    fixture.module_type("review", "Review", "");
    let current = current_module_types(&fixture.repositories, None).unwrap();
    assert_eq!(current.len(), 2);
    let frozen_again = crate::session_work::frozen_module_types(store, "session_primary").unwrap();
    assert_eq!(frozen_again, frozen);
    // A resumed session reads the same binding from its saved state.
    let json = serde_json::to_string(&session).unwrap();
    let resumed: Session = serde_json::from_str(&json).unwrap();
    assert_eq!(resumed.session_work, session.session_work);
}

#[test]
fn invalid_module_types_are_left_out_of_the_registry() {
    let _env = crate::storage::lock_test_env();
    let fixture = Fixture::new();
    fixture.module_type("good", "Good", "");
    fixture.module_type("bad-subtype", "Bad", "subtypes:\n  - Not Valid\n");
    let dir = fixture.global.join("module-types");
    std::fs::write(
        dir.join("nameless.md"),
        "---\nkind: module-type\nid: nameless\n---\nbody\n",
    )
    .unwrap();
    let types = current_module_types(&fixture.repositories, None).unwrap();
    let ids: Vec<_> = types
        .iter()
        .map(|module_type| module_type.id.as_str())
        .collect();
    assert_eq!(ids, ["global:good"]);
}

#[test]
fn a_child_starts_from_its_presets_workflow_template() {
    let _env = crate::storage::lock_test_env();
    let _on = ScopedFeatureOverride::session_work(true);
    let fixture = Fixture::new();
    let mut child = fixture.session("session_child");
    let template = "- [>] gather: Gather sources {research}\n- [ ] report: Report findings\n";
    assert!(fixture.activate(
        &mut child,
        NewSessionWork::Child {
            preset: "global:task-preset.fixture",
            template: Some(template),
        }
    ));
    assert_eq!(
        child.session_work.as_ref().unwrap().role,
        SessionWorkRole::Child
    );
    let store = fixture.surface.store();
    let head = store.workflow_head("session_child").unwrap().unwrap();
    assert_eq!(head.revision.revision, 1);
    assert_eq!(head.revision.text, template);
    assert_eq!(
        head.revision.source,
        RevisionSource::Template {
            preset: "global:task-preset.fixture".into()
        }
    );
    assert_eq!(
        std::fs::read_to_string(fixture.surface.workflow_path("session_child").unwrap()).unwrap(),
        template
    );
    let history = fixture
        .surface
        .history_dir("session_child")
        .unwrap()
        .join("r01.md");
    assert_eq!(std::fs::read_to_string(history).unwrap(), template);

    let mut plain = fixture.session("session_plain_child");
    assert!(fixture.activate(
        &mut plain,
        NewSessionWork::Child {
            preset: "global:task-preset.general",
            template: None,
        }
    ));
    assert_eq!(store.workflow_head("session_plain_child").unwrap(), None);
}

#[test]
fn transfer_copies_the_workflow_with_provenance_and_split_starts_without_one() {
    let _env = crate::storage::lock_test_env();
    let fixture = Fixture::new();
    let store = fixture.surface.store();
    let mut source = fixture.session("session_source");
    {
        let _on = ScopedFeatureOverride::session_work(true);
        assert!(fixture.activate(&mut source, NewSessionWork::Primary));
    }
    for (request, text) in [
        ("w1", "- [>] a: A\n- [ ] b: B\n"),
        ("w2", "- [x] a: A\n- [>] b: B\n"),
    ] {
        fixture
            .surface
            .write_workflow("session_source", request, text, chrono::Utc::now())
            .unwrap();
    }

    // Split continues an activated conversation even after the flag is off,
    // but never inherits the workflow.
    {
        let _off = ScopedFeatureOverride::session_work(false);
        let mut split = fixture.session("session_split");
        assert!(fixture.activate(&mut split, NewSessionWork::Split { source: &source }));
        assert_eq!(store.workflow_head("session_split").unwrap(), None);
        let activation = store.activation("session_split").unwrap().unwrap();
        assert_eq!(
            activation.origin,
            ActivationOrigin::Split {
                source_session: "session_source".into()
            }
        );
        let mut transfer_off = fixture.session("session_transfer_off");
        assert!(!fixture.activate(
            &mut transfer_off,
            NewSessionWork::Transfer { source: &source }
        ));
    }

    let _on = ScopedFeatureOverride::session_work(true);
    let mut transfer = fixture.session("session_transfer");
    assert!(fixture.activate(&mut transfer, NewSessionWork::Transfer { source: &source }));
    let head = store.workflow_head("session_transfer").unwrap().unwrap();
    assert_eq!(head.revision.revision, 1);
    assert_eq!(head.revision.text, "- [x] a: A\n- [>] b: B\n");
    assert_eq!(
        head.revision.source,
        RevisionSource::Transfer {
            source_session: "session_source".into(),
            source_revision: 2
        }
    );
    assert_eq!(
        std::fs::read_to_string(fixture.surface.workflow_path("session_transfer").unwrap())
            .unwrap(),
        head.revision.text
    );
    // The source's finishes stay with the source.
    let times = store.module_times("session_transfer").unwrap();
    assert!(times.iter().all(|time| time.finished_at.is_none()));

    // A source without session work gives a fresh activation with no workflow.
    let legacy = fixture.session("session_legacy");
    let mut from_legacy = fixture.session("session_from_legacy");
    assert!(fixture.activate(
        &mut from_legacy,
        NewSessionWork::Transfer { source: &legacy }
    ));
    assert_eq!(store.workflow_head("session_from_legacy").unwrap(), None);
    // A split of a session without session work stays without it.
    let mut legacy_split = fixture.session("session_legacy_split");
    assert!(!fixture.activate(&mut legacy_split, NewSessionWork::Split { source: &legacy }));
}

#[test]
fn reconcile_ignores_sessions_without_session_work() {
    let fixture = Fixture::new();
    let session = fixture.session("session_plain");
    assert_eq!(reconcile_session(&session).unwrap(), None);
}

#[test]
fn reconcile_fails_closed_when_the_store_lost_an_activated_session() {
    let _env = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().unwrap();
    let previous = [
        ("JCODE_HOME", std::env::var_os("JCODE_HOME")),
        ("JCODE_RUNTIME_DIR", std::env::var_os("JCODE_RUNTIME_DIR")),
    ];
    crate::env::set_var("JCODE_HOME", temp.path().join("home"));
    crate::env::set_var("JCODE_RUNTIME_DIR", temp.path().join("runtime"));
    let _on = ScopedFeatureOverride::session_work(true);
    let mut session = Session::create_with_id("session_lost".into(), None, None);
    let repositories = InstructionRepositoryService::new();
    assert!(activate_new_session(&mut session, NewSessionWork::Primary, &repositories).unwrap());
    assert_eq!(reconcile_session(&session).unwrap(), None);
    let mut stranger = Session::create_with_id("session_stranger".into(), None, None);
    stranger.session_work = session.session_work.clone();
    let outcome = reconcile_session(&stranger);
    for (key, value) in previous {
        match value {
            Some(value) => crate::env::set_var(key, value),
            None => crate::env::remove_var(key),
        }
    }
    assert!(
        matches!(outcome, Err(SessionWorkError::NotActivated(_))),
        "{outcome:?}"
    );
}
