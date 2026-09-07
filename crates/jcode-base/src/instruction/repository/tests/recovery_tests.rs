use super::*;

#[test]
fn retained_draft_listing_and_explicit_comparison_preserve_all_versions() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let mut workspace = InstructionDraftWorkspace::default();
    let initial = workspace
        .begin(
            &fixture.service,
            &repository,
            "recovery-session",
            &["modules/common.md".into()],
            "instruction: recover common",
        )
        .unwrap()
        .clone();
    let proposed = managed("common", "module", "PROPOSED");
    workspace
        .revise(
            &fixture.service,
            &initial.id,
            0,
            vec![InstructionFileMutation::Write {
                relative_path: "modules/common.md".into(),
                content: proposed.as_bytes().to_vec(),
            }],
        )
        .unwrap();
    let external = managed("common", "module", "EXTERNAL");
    std::fs::write(repository.root.join("modules/common.md"), &external).unwrap();
    let comparison = workspace
        .compare_current(&fixture.service, &initial.id, 1)
        .unwrap();
    assert_eq!(comparison.files[0].base, initial.bases[0].content);
    assert_eq!(
        comparison.files[0].working.as_deref(),
        Some(external.as_str())
    );
    assert_eq!(
        comparison.files[0].proposed.as_deref(),
        Some(proposed.as_str())
    );
    assert!(
        workspace
            .reconcile_current(&fixture.service, &initial.id, 1, "wrong", false)
            .is_err()
    );
    let replacement = workspace
        .reconcile_current(
            &fixture.service,
            &initial.id,
            1,
            &comparison.comparison,
            false,
        )
        .unwrap()
        .clone();
    assert_ne!(replacement.id, initial.id);
    assert_eq!(
        replacement.bases[0].content.as_deref(),
        Some(external.as_str())
    );
    assert_eq!(
        std::fs::read_to_string(repository.root.join("modules/common.md")).unwrap(),
        external
    );
    assert!(
        fixture
            .service
            .read_editing_draft(&repository, "recovery-session", &initial.id)
            .is_ok()
    );
    let (drafts, errors) = fixture
        .service
        .retained_drafts(&repository, "recovery-session")
        .unwrap();
    assert_eq!(drafts.len(), 2);
    assert!(errors.is_empty());
    assert!(!serde_json::to_string(&drafts).unwrap().contains("PROPOSED"));
    assert!(
        fixture
            .service
            .retained_drafts(&repository, "another-session")
            .unwrap()
            .0
            .is_empty()
    );
    workspace
        .review(&fixture.service, &replacement.id, replacement.generation)
        .unwrap();
    workspace
        .save(&fixture.service, &replacement.id, replacement.generation)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(repository.root.join("modules/common.md")).unwrap(),
        proposed
    );
}

#[test]
fn comparison_rejects_later_external_changes_and_can_start_from_working_content() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let mut workspace = InstructionDraftWorkspace::default();
    let initial = workspace
        .begin(
            &fixture.service,
            &repository,
            "recovery-session",
            &["modules/common.md".into()],
            "instruction: compare",
        )
        .unwrap()
        .clone();
    let first = workspace
        .compare_current(&fixture.service, &initial.id, 0)
        .unwrap();
    let latest = managed("common", "module", "LATEST");
    std::fs::write(repository.root.join("modules/common.md"), &latest).unwrap();
    assert!(
        workspace
            .reconcile_current(&fixture.service, &initial.id, 0, &first.comparison, true)
            .is_err()
    );
    let current = workspace
        .compare_current(&fixture.service, &initial.id, 0)
        .unwrap();
    let replacement = workspace
        .reconcile_current(&fixture.service, &initial.id, 0, &current.comparison, true)
        .unwrap();
    assert!(
        matches!(&replacement.request.mutations[0], InstructionFileMutation::Write { content, .. } if content == latest.as_bytes())
    );
    assert_eq!(
        std::fs::read_to_string(repository.root.join("modules/common.md")).unwrap(),
        latest
    );
}

#[test]
fn malformed_recovery_record_does_not_hide_valid_drafts_or_get_deleted() {
    let fixture = Fixture::new();
    let repository = fixture.initialize().repository;
    let mut workspace = InstructionDraftWorkspace::default();
    workspace
        .begin(
            &fixture.service,
            &repository,
            "recovery-session",
            &["modules/common.md".into()],
            "instruction: retained",
        )
        .unwrap();
    let path = fixture
        .state
        .join("instruction-repositories/drafts/global/broken.json");
    std::fs::write(&path, "{broken").unwrap();
    let (drafts, errors) = fixture
        .service
        .retained_drafts(&repository, "recovery-session")
        .unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(errors.len(), 1);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "{broken");
}
