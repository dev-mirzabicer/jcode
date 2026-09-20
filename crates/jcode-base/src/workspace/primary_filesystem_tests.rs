use super::*;

#[test]
#[cfg(target_os = "macos")]
fn empty_primary_cwd_recovers_witnessed_publication_and_never_reuses_foreign_targets() {
    let temp = tempfile::tempdir().unwrap();
    let service = WorkspaceService::new(&temp.path().join("state"));
    service.initialize(RequestId::new()).unwrap();
    let model = PrimaryModel {
        model: "fixture".into(),
        provider: "fixture".into(),
        api_method: "fixture".into(),
        effort: None,
    };
    for stage in ["primary_directory_staged", "primary_directory_published"] {
        let path = temp.path().join(stage);
        let request = RequestId::new();
        let input = PrimaryLaunchInput {
            placement: PrimaryPlacement::Standalone { root: path.clone() },
            cwd: Some(PrimaryCwd::CreateEmpty {
                path: path.clone(),
                home: None,
            }),
            agent: None,
            model: None,
            selfdev: false,
        };
        service
            .reserve_primary_launch(
                request,
                service.status().unwrap().revision,
                input,
                model.clone(),
            )
            .unwrap();
        let mut faulted = service.clone();
        faulted.fault = Some(std::sync::Arc::new(move |name| {
            if name == stage {
                Err(io("synthetic directory interruption"))
            } else {
                Ok(())
            }
        }));
        assert!(faulted.prepare_launch_filesystem(request).is_err());
        if path.exists() {
            std::fs::write(
                path.join("arrived-after-publication"),
                b"preserve this user data",
            )
            .unwrap();
        }
        let placement = service.prepare_launch_filesystem(request).unwrap();
        assert_eq!(
            service.prepare_launch_filesystem(request).unwrap(),
            placement
        );
        if stage == "primary_directory_published" {
            assert_eq!(
                std::fs::read(path.join("arrived-after-publication")).unwrap(),
                b"preserve this user data"
            );
        }
        assert!(matches!(placement, Placement::Standalone(_)));
        assert!(!path.join(".git").exists());
    }
    let foreign = temp.path().join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("sentinel"), b"untouched").unwrap();
    let request = RequestId::new();
    service
        .reserve_primary_launch(
            request,
            service.status().unwrap().revision,
            PrimaryLaunchInput {
                placement: PrimaryPlacement::Standalone {
                    root: foreign.clone(),
                },
                cwd: Some(PrimaryCwd::CreateEmpty {
                    path: foreign.clone(),
                    home: None,
                }),
                agent: None,
                model: None,
                selfdev: false,
            },
            model.clone(),
        )
        .unwrap();
    assert_eq!(
        service.prepare_launch_filesystem(request).unwrap_err().code,
        IssueCode::Conflict
    );
    assert_eq!(
        std::fs::read(foreign.join("sentinel")).unwrap(),
        b"untouched"
    );

    // A destination arriving after staging cannot be overwritten, even when empty.
    let collision = temp.path().join("collision");
    let request = RequestId::new();
    service
        .reserve_primary_launch(
            request,
            service.status().unwrap().revision,
            PrimaryLaunchInput {
                placement: PrimaryPlacement::Standalone {
                    root: collision.clone(),
                },
                cwd: Some(PrimaryCwd::CreateEmpty {
                    path: collision.clone(),
                    home: None,
                }),
                agent: None,
                model: None,
                selfdev: false,
            },
            model.clone(),
        )
        .unwrap();
    let mut faulted = service.clone();
    faulted.fault = Some(std::sync::Arc::new(|name| {
        if name == "primary_directory_staged" {
            Err(io("synthetic interruption"))
        } else {
            Ok(())
        }
    }));
    assert!(faulted.prepare_launch_filesystem(request).is_err());
    std::fs::create_dir(&collision).unwrap();
    std::fs::write(collision.join("sentinel"), b"foreign").unwrap();
    assert_eq!(
        service.prepare_launch_filesystem(request).unwrap_err().code,
        IssueCode::Conflict
    );
    assert_eq!(
        std::fs::read(collision.join("sentinel")).unwrap(),
        b"foreign"
    );

    let alias = temp.path().join("alias-target");
    let request = RequestId::new();
    let record = service
        .reserve_primary_launch(
            request,
            service.status().unwrap().revision,
            PrimaryLaunchInput {
                placement: PrimaryPlacement::Standalone {
                    root: alias.clone(),
                },
                cwd: Some(PrimaryCwd::CreateEmpty {
                    path: alias.clone(),
                    home: None,
                }),
                agent: None,
                model: None,
                selfdev: false,
            },
            model.clone(),
        )
        .unwrap();
    assert!(faulted.prepare_launch_filesystem(request).is_err());
    let stage = temp
        .path()
        .join(format!(".jcode-primary-{}", record.operation));
    std::os::unix::fs::symlink(&stage, &alias).unwrap();
    assert_eq!(
        service.prepare_launch_filesystem(request).unwrap_err().code,
        IssueCode::Conflict
    );
    assert!(stage.is_dir());
    assert!(
        std::fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let receipt = service
        .review_organization_change(
            service.status().unwrap().revision,
            OrganizationChange::CreateProject {
                name: "explicit home".into(),
            },
        )
        .unwrap();
    let EntityId::Project(project) = service
        .apply_organization_change(RequestId::new(), receipt.id)
        .unwrap()
        .targets[0]
    else {
        panic!("project")
    };
    let member = temp.path().join("new-member");
    let request = RequestId::new();
    service
        .reserve_primary_launch(
            request,
            service.status().unwrap().revision,
            PrimaryLaunchInput {
                placement: PrimaryPlacement::Existing {
                    placement: Placement::Project(project),
                },
                cwd: Some(PrimaryCwd::CreateEmpty {
                    path: member.clone(),
                    home: Some(Home::Project(project)),
                }),
                agent: None,
                model: None,
                selfdev: false,
            },
            model.clone(),
        )
        .unwrap();
    assert_eq!(
        service.prepare_launch_filesystem(request).unwrap(),
        Placement::Project(project)
    );
    assert!(member.is_dir());
    assert_eq!(std::fs::read_dir(member).unwrap().count(), 0);
    let review = service
        .review_organization_change(
            service.status().unwrap().revision,
            OrganizationChange::Retire {
                target: EntityId::Project(project),
            },
        )
        .unwrap();
    service
        .apply_organization_change(RequestId::new(), review.id)
        .unwrap();
    let rejected = temp.path().join("retired-must-not-create");
    let request = RequestId::new();
    service
        .reserve_primary_launch(
            request,
            service.status().unwrap().revision,
            PrimaryLaunchInput {
                placement: PrimaryPlacement::Existing {
                    placement: Placement::Project(project),
                },
                cwd: Some(PrimaryCwd::CreateEmpty {
                    path: rejected.clone(),
                    home: Some(Home::Project(project)),
                }),
                agent: None,
                model: None,
                selfdev: false,
            },
            model,
        )
        .unwrap();
    assert_eq!(
        service.prepare_launch_filesystem(request).unwrap_err().code,
        IssueCode::InvalidIdentity
    );
    assert!(!rejected.exists());
}
