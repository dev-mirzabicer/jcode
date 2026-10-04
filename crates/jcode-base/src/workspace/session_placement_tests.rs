use super::*;
use crate::session::Session;

struct Fixture {
    _temp: tempfile::TempDir,
    _env: Environment,
    root: PathBuf,
    service: WorkspaceService,
}

struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl Drop for Environment {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(value) => crate::env::set_var(key, value),
                None => crate::env::remove_var(key),
            }
        }
    }
}

fn fixture(initialize: bool) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let env = Environment(
        ["JCODE_HOME", "JCODE_RUNTIME_DIR", "HOME"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect(),
    );
    crate::env::set_var("JCODE_HOME", root.join("state"));
    crate::env::set_var("JCODE_RUNTIME_DIR", root.join("runtime"));
    std::fs::create_dir(root.join("home")).unwrap();
    crate::env::set_var("HOME", root.join("home"));
    let service = WorkspaceService::new(&crate::storage::durable_state_dir());
    if initialize {
        service.initialize(RequestId::new()).unwrap();
    }
    Fixture {
        _temp: temp,
        _env: env,
        root,
        service,
    }
}

fn unplaced(cwd: &Path) -> Session {
    let mut session = Session::create(None, None);
    session.working_dir = Some(cwd.display().to_string());
    session
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

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[test]
fn proposal_requires_an_initialized_catalog() {
    let _lock = crate::storage::lock_test_env();
    let fixture = fixture(false);
    let work = fixture.root.join("work");
    std::fs::create_dir(&work).unwrap();
    let error = fixture
        .service
        .propose_session_placement(&unplaced(&work))
        .unwrap_err();
    assert!(error.is_not_initialized(), "{error:?}");
}

#[test]
fn unregistered_directories_propose_their_git_root_or_themselves() {
    let _lock = crate::storage::lock_test_env();
    let fixture = fixture(true);
    let plain = fixture.root.join("plain");
    std::fs::create_dir(&plain).unwrap();
    let proposal = fixture
        .service
        .propose_session_placement(&unplaced(&plain))
        .unwrap();
    assert_eq!(proposal.default, Some(0));
    assert_eq!(proposal.candidates.len(), 1);
    assert_eq!(
        proposal.candidates[0].placement,
        PrimaryPlacement::Standalone {
            root: plain.clone()
        }
    );
    assert_eq!(proposal.candidates[0].name, "plain");
    assert!(!proposal.candidates[0].broad);

    let repo = fixture.root.join("repo");
    std::fs::create_dir_all(repo.join("src/inner")).unwrap();
    git(&repo, &["init", "-q"]);
    let proposal = fixture
        .service
        .propose_session_placement(&unplaced(&repo.join("src/inner")))
        .unwrap();
    assert_eq!(proposal.working_dir, repo.join("src/inner"));
    assert_eq!(
        proposal.candidates[0].placement,
        PrimaryPlacement::Standalone { root: repo.clone() }
    );
    assert_eq!(proposal.default, Some(0));
}

#[test]
fn home_is_offered_but_never_proposed() {
    let _lock = crate::storage::lock_test_env();
    let fixture = fixture(true);
    let home = fixture.root.join("home");
    let proposal = fixture
        .service
        .propose_session_placement(&unplaced(&home))
        .unwrap();
    assert_eq!(proposal.candidates.len(), 1);
    assert!(proposal.candidates[0].broad);
    assert_eq!(proposal.default, None);
}

#[test]
fn registered_roots_offer_their_own_area_and_project_placements() {
    let _lock = crate::storage::lock_test_env();
    let fixture = fixture(true);
    let service = &fixture.service;
    let EntityId::Project(project) = change(
        service,
        OrganizationChange::CreateProject {
            name: "Alpha".into(),
        },
    ) else {
        panic!("project")
    };
    let EntityId::WorkArea(area) = change(
        service,
        OrganizationChange::CreateWorkArea {
            project,
            name: "Spike".into(),
        },
    ) else {
        panic!("work area")
    };
    let notes = fixture.root.join("notes");
    std::fs::create_dir_all(notes.join("deep")).unwrap();
    let EntityId::Location(location) = change(
        service,
        OrganizationChange::RegisterLocation {
            name: "notes".into(),
            path: notes.clone(),
            registration: Registration::Directory {
                home: Home::WorkArea(area),
            },
        },
    ) else {
        panic!("location")
    };
    let proposal = service
        .propose_session_placement(&unplaced(&notes.join("deep")))
        .unwrap();
    let placements = proposal
        .candidates
        .iter()
        .map(|candidate| candidate.placement.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        placements,
        vec![
            PrimaryPlacement::Existing {
                placement: Placement::Directory(location)
            },
            PrimaryPlacement::Existing {
                placement: Placement::WorkArea(area)
            },
            PrimaryPlacement::Existing {
                placement: Placement::Project(project)
            },
        ]
    );
    assert!(
        proposal
            .candidates
            .iter()
            .all(|candidate| candidate.project.as_deref() == Some("Alpha")
                && candidate.root == notes)
    );
    assert_eq!(proposal.default, Some(0));
    // Resolving an existing placement registers nothing.
    let before = service.status().unwrap().revision;
    let request = SessionPlacementRequest {
        request: RequestId::new(),
        session: proposal.session.clone(),
        working_dir: proposal.working_dir.clone(),
        expected_catalog_revision: proposal.catalog_revision,
        placement: placements[1].clone(),
    };
    let (placement, revision) = service
        .resolve_session_placement(&request, &unplaced(&notes.join("deep")))
        .unwrap();
    assert_eq!(placement, Placement::WorkArea(area));
    assert_eq!(revision, before);
    assert_eq!(service.status().unwrap().revision, before);
}

#[test]
fn standalone_resolution_registers_once_and_rejects_stale_reviews() {
    let _lock = crate::storage::lock_test_env();
    let fixture = fixture(true);
    let service = &fixture.service;
    let work = fixture.root.join("work");
    std::fs::create_dir(&work).unwrap();
    let session = unplaced(&work);
    let proposal = service.propose_session_placement(&session).unwrap();
    let request = SessionPlacementRequest {
        request: RequestId::new(),
        session: session.id.clone(),
        working_dir: proposal.working_dir.clone(),
        expected_catalog_revision: proposal.catalog_revision,
        placement: proposal.candidates[0].placement.clone(),
    };

    let mut moved = request.clone();
    moved.working_dir = fixture.root.clone();
    assert_eq!(
        service
            .resolve_session_placement(&moved, &session)
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );

    let (first, revision) = service
        .resolve_session_placement(&request, &session)
        .unwrap();
    let Placement::Standalone(location) = first else {
        panic!("standalone placement")
    };
    // An unrelated change after registration does not make a retry register
    // a second root or fail; it converges on the original receipt.
    change(
        service,
        OrganizationChange::CreateProject {
            name: "Unrelated".into(),
        },
    );
    assert_eq!(
        service
            .resolve_session_placement(&request, &session)
            .unwrap(),
        (first, revision)
    );
    let Entity::Location(registered) = service.inspect(EntityId::Location(location)).unwrap()
    else {
        panic!("location")
    };
    assert_eq!(registered.observed_path, work);
    assert!(matches!(registered.kind, LocationKind::Standalone { .. }));
    assert!(registered.home.is_none());

    // The registered root now contains the cwd, so a new review offers it.
    let again = service.propose_session_placement(&session).unwrap();
    assert_eq!(
        again.candidates[0].placement,
        PrimaryPlacement::Existing {
            placement: Placement::Standalone(location)
        }
    );

    // A fresh request against an outdated review registers nothing.
    let stale = SessionPlacementRequest {
        request: RequestId::new(),
        session: session.id.clone(),
        working_dir: work.clone(),
        expected_catalog_revision: proposal.catalog_revision,
        placement: PrimaryPlacement::Standalone {
            root: fixture.root.join("work"),
        },
    };
    assert_eq!(
        service
            .resolve_session_placement(&stale, &session)
            .unwrap_err()
            .code,
        IssueCode::Conflict
    );
}

#[test]
fn sessions_without_a_recorded_cwd_need_explicit_adoption() {
    let _lock = crate::storage::lock_test_env();
    let fixture = fixture(true);
    let mut legacy = Session::create(None, None);
    legacy.working_dir = None;
    assert_eq!(
        fixture
            .service
            .propose_session_placement(&legacy)
            .unwrap_err()
            .code,
        IssueCode::NeedsCwd
    );
}

#[test]
fn control_lease_reports_an_uninitialized_catalog() {
    let _lock = crate::storage::lock_test_env();
    let fixture = fixture(false);
    let error = fixture
        .service
        .primary_control_lease("session_fixture")
        .err()
        .expect("no lease without a catalog");
    assert!(error.is_not_initialized(), "{error:?}");
}
