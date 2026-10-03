//! Discovery reads current catalog state for an authoritative Session and
//! pages large memberships without a member-count cap.
use super::*;
use crate::session::Session;

struct Environment {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    temporary: tempfile::TempDir,
}
impl Environment {
    fn new() -> Self {
        let lock = crate::storage::lock_test_env();
        let temporary = tempfile::tempdir().unwrap();
        let saved = vec![
            ("JCODE_HOME", std::env::var_os("JCODE_HOME")),
            ("JCODE_RUNTIME_DIR", std::env::var_os("JCODE_RUNTIME_DIR")),
        ];
        crate::env::set_var("JCODE_HOME", temporary.path().join("home"));
        crate::env::set_var("JCODE_RUNTIME_DIR", temporary.path().join("runtime"));
        crate::config::Config::invalidate_cache();
        Self {
            _lock: lock,
            saved,
            temporary,
        }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.temporary.path().join(name)
    }
}
impl Drop for Environment {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(value) => crate::env::set_var(key, value),
                None => crate::env::remove_var(key),
            }
        }
        crate::config::Config::invalidate_cache();
    }
}

fn change(service: &WorkspaceService, change: OrganizationChange) -> EntityId {
    let review = service
        .review_organization_change(service.status().unwrap().revision, change)
        .unwrap();
    service
        .apply_organization_change(RequestId::new(), review.id)
        .unwrap()
        .targets[0]
}

fn directory(service: &WorkspaceService, path: &Path, home: Option<Home>) -> LocationId {
    std::fs::create_dir_all(path).unwrap();
    let EntityId::Location(id) = change(
        service,
        OrganizationChange::RegisterLocation {
            name: path.file_name().unwrap().to_string_lossy().into(),
            path: path.into(),
            registration: match home {
                Some(home) => Registration::Directory { home },
                None => Registration::Standalone,
            },
        },
    ) else {
        panic!()
    };
    id
}

fn placed(service: &WorkspaceService, placement: Placement, cwd: &Path) -> Session {
    let prepared = service
        .prepare_primary_location(placement, Some(cwd), OperationId::new())
        .unwrap();
    let mut session = Session::create_with_id(
        format!("session_discovery_{}", RequestId::new()),
        None,
        None,
    );
    session.working_dir = Some(cwd.to_string_lossy().into());
    session.location = Some(prepared.location.clone());
    session
}

struct Catalog {
    service: WorkspaceService,
    project: ProjectId,
    area: WorkAreaId,
    repository: RepositoryId,
    member: LocationId,
    in_area: LocationId,
    outside: LocationId,
}

fn catalog(environment: &Environment) -> Catalog {
    let service = WorkspaceService::new(&crate::storage::durable_state_dir());
    service.initialize(RequestId::new()).unwrap();
    let EntityId::Project(project) = change(
        &service,
        OrganizationChange::CreateProject {
            name: "alpha".into(),
        },
    ) else {
        panic!()
    };
    let EntityId::WorkArea(area) = change(
        &service,
        OrganizationChange::CreateWorkArea {
            project,
            name: "sprint".into(),
        },
    ) else {
        panic!()
    };
    let EntityId::Repository(repository) = change(
        &service,
        OrganizationChange::CreateRepository {
            name: "api".into(),
            remotes: vec![],
        },
    ) else {
        panic!()
    };
    change(
        &service,
        OrganizationChange::AssociateRepository {
            project,
            repository,
        },
    );
    let member = directory(
        &service,
        &environment.path("member"),
        Some(Home::Project(project)),
    );
    let in_area = directory(
        &service,
        &environment.path("in-area"),
        Some(Home::WorkArea(area)),
    );
    let outside = directory(&service, &environment.path("outside"), None);
    Catalog {
        service,
        project,
        area,
        repository,
        member,
        in_area,
        outside,
    }
}

#[test]
#[cfg(target_os = "macos")]
fn location_context_reports_home_chain_scope_and_members_for_each_placement() {
    let environment = Environment::new();
    let catalog = catalog(&environment);
    let project_session = placed(
        &catalog.service,
        Placement::Project(catalog.project),
        &environment.path("member"),
    );
    let context = catalog.service.location_context(&project_session).unwrap();
    assert_eq!(context.placement_name, "alpha");
    assert_eq!(
        context.project.as_ref().unwrap().id,
        EntityId::Project(catalog.project)
    );
    assert!(context.work_area.is_none() && context.repository.is_none());
    let members = context.members.as_ref().unwrap();
    assert_eq!(
        (members.repositories, members.work_areas, members.locations),
        (1, 1, 2)
    );
    assert!(!members.more);
    assert_eq!(context.scope.ordinary_roots, 2);
    assert_eq!(context.scope.granted_roots, 0);
    let text = location_context_text(&context);
    for id in [
        catalog.project.to_string(),
        catalog.area.to_string(),
        catalog.repository.to_string(),
        catalog.member.to_string(),
        catalog.in_area.to_string(),
    ] {
        assert!(text.contains(&id), "{id} missing from {text}");
    }
    assert!(!text.contains(&catalog.outside.to_string()));

    // A directory placed in a work area names its whole home chain.
    let area_session = placed(
        &catalog.service,
        Placement::Directory(catalog.in_area),
        &environment.path("in-area"),
    );
    let context = catalog.service.location_context(&area_session).unwrap();
    assert_eq!(
        context.work_area.as_ref().unwrap().id,
        EntityId::WorkArea(catalog.area)
    );
    assert_eq!(
        context.project.as_ref().unwrap().id,
        EntityId::Project(catalog.project)
    );
    assert_eq!(context.location.as_ref().unwrap().id, catalog.in_area);
    assert!(context.members.is_none());
    assert_eq!(context.scope.ordinary_roots, 1);
    let text = location_context_text(&context);
    assert!(
        text.contains(&catalog.area.to_string()) && text.contains(&catalog.project.to_string())
    );

    // Legacy (unplaced) sessions have no workspace facts.
    let legacy = Session::create_with_id("session_discovery_legacy".into(), None, None);
    assert_eq!(
        catalog.service.location_context(&legacy).unwrap_err().code,
        IssueCode::RecoveryRequired
    );
}

#[test]
#[cfg(target_os = "macos")]
fn initial_session_context_includes_location_facts_only_for_placed_sessions() {
    let environment = Environment::new();
    let catalog = catalog(&environment);
    let mut session = placed(
        &catalog.service,
        Placement::Directory(catalog.in_area),
        &environment.path("in-area"),
    );
    assert!(session.ensure_initial_session_context_message());
    let text = serde_json::to_string(&session.messages).unwrap();
    assert!(text.contains(&catalog.in_area.to_string()));
    assert!(text.contains(&catalog.area.to_string()));
    let mut legacy = Session::create_with_id("session_discovery_legacy_context".into(), None, None);
    legacy.working_dir = Some(environment.path("in-area").to_string_lossy().into());
    assert!(legacy.ensure_initial_session_context_message());
    let text = serde_json::to_string(&legacy.messages).unwrap();
    assert!(!text.contains(&catalog.in_area.to_string()));
}

#[test]
#[cfg(target_os = "macos")]
fn memberships_page_completely_across_continuations() {
    memberships_page_completely(23, 10, 3);
}

/// Above the 200-entry page bound, so one maximal page is never the whole
/// set. Each registration binds a real volume, so this takes about 25 minutes;
/// run it explicitly for acceptance evidence.
#[test]
#[ignore = "expensive: registers 250 physical locations"]
#[cfg(target_os = "macos")]
fn large_memberships_page_completely_without_a_member_cap() {
    memberships_page_completely(248, 200, 2);
}

#[cfg(target_os = "macos")]
fn memberships_page_completely(extra: usize, size: u32, expected_pages: usize) {
    let environment = Environment::new();
    let catalog = catalog(&environment);
    for index in 0..extra {
        directory(
            &catalog.service,
            &environment.path(&format!("bulk/{index:03}")),
            Some(Home::Project(catalog.project)),
        );
    }
    let session = placed(
        &catalog.service,
        Placement::Project(catalog.project),
        &environment.path("member"),
    );
    let context = catalog.service.location_context(&session).unwrap();
    let members = context.members.unwrap();
    assert_eq!(members.locations, extra as u64 + 2);
    assert!(members.more);
    assert_eq!(members.first.len(), MEMBER_PREVIEW as usize);

    let mut seen = std::collections::BTreeSet::new();
    let mut after = None;
    let mut pages = 0;
    loop {
        let page = catalog
            .service
            .session_scope_page(&session, after.clone(), size)
            .unwrap();
        assert_eq!(page.total, extra as u64 + 2);
        assert!(page.roots.len() <= size as usize);
        for root in &page.roots {
            assert!(root.issue.is_none(), "{root:?}");
            assert!(seen.insert(root.location.id));
        }
        pages += 1;
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert_eq!(pages, expected_pages);
    assert_eq!(seen.len(), extra + 2);

    // Catalog list paging reaches every member too.
    let mut listed = 0;
    let mut after = None;
    loop {
        let page = catalog
            .service
            .list(
                Query {
                    project: Some(catalog.project),
                    kind: Some(EntityKind::Location),
                    ..Default::default()
                },
                after,
                size,
            )
            .unwrap();
        listed += page.items.len();
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(listed, extra + 2);

    // A continuation from older catalog state is a conflict, not a silent gap.
    let first = catalog
        .service
        .session_scope_page(&session, None, 10)
        .unwrap();
    directory(
        &catalog.service,
        &environment.path("late"),
        Some(Home::Project(catalog.project)),
    );
    assert_eq!(
        catalog
            .service
            .session_scope_page(&session, first.next, 10)
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
    assert_eq!(
        catalog
            .service
            .session_scope_page(&session, None, 0)
            .unwrap_err()
            .code,
        IssueCode::InvalidInput
    );
}

#[test]
#[cfg(target_os = "macos")]
fn locate_names_the_owning_root_and_current_write_access() {
    let environment = Environment::new();
    let catalog = catalog(&environment);
    let session = placed(
        &catalog.service,
        Placement::Directory(catalog.member),
        &environment.path("member"),
    );
    let inside = catalog
        .service
        .locate_path(&session, Path::new("nested/new-file.txt"))
        .unwrap();
    assert_eq!(inside.location.unwrap().id, catalog.member);
    assert!(inside.writable && inside.ordinary);
    let foreign = catalog
        .service
        .locate_path(&session, &environment.path("outside/file"))
        .unwrap();
    assert_eq!(foreign.location.unwrap().id, catalog.outside);
    assert!(!foreign.writable);
    let unregistered = catalog
        .service
        .locate_path(&session, &environment.path("elsewhere"))
        .unwrap();
    assert!(unregistered.location.is_none() && !unregistered.writable);
}
