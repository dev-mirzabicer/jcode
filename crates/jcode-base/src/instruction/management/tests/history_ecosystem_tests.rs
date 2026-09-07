use super::*;
fn git(path: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

#[test]
fn ecosystem_save_is_private_reviewed_recoverable_and_never_commits_parent() {
    let _environment = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut fixture=Fixture::new();let project=fixture.context.working_dir.clone().unwrap();
        std::fs::write(project.join("AGENTS.md"),"ORIGINAL").unwrap();
        git(&project,&["init","--initial-branch","main"]);git(&project,&["add","AGENTS.md"]);git(&project,&["commit","-m","fixture"]);
        std::fs::write(project.join("unrelated.txt"),"STAGED").unwrap();git(&project,&["add","unrelated.txt"]);
        let head=git(&project,&["rev-parse","HEAD"]);let index=std::fs::read(project.join(".git/index")).unwrap();
        let draft=fixture.begin("AGENTS.md",InstructionEditAction::Edit).await;
        assert!(draft.working_file_only); assert!(draft.branch.is_none());
        let updated=super::draft(fixture.request(InstructionManagementRequest::Update{draft:draft.id.clone(),generation:draft.generation,change:InstructionDraftChange::Body{file:"AGENTS.md".into(),body:"NEW 合成".into()}}).await);
        assert_eq!(std::fs::read_to_string(project.join("AGENTS.md")).unwrap(),"ORIGINAL");
        assert!(matches!(fixture.request(InstructionManagementRequest::Save{draft:updated.id.clone(),generation:updated.generation}).await,InstructionManagementResult::Failed(_)));
        let review=fixture.request(InstructionManagementRequest::Review{draft:updated.id.clone(),generation:updated.generation}).await;
        assert!(matches!(review,InstructionManagementResult::Reviewed(ref value) if value.errors.is_empty()));
        let saved=fixture.request(InstructionManagementRequest::Save{draft:updated.id.clone(),generation:updated.generation}).await;
        assert!(matches!(saved,InstructionManagementResult::FileSaved{..}),"{saved:?}");
        assert_eq!(std::fs::read_to_string(project.join("AGENTS.md")).unwrap(),"NEW 合成");
        assert_eq!(git(&project,&["rev-parse","HEAD"]),head);assert_eq!(std::fs::read(project.join(".git/index")).unwrap(),index);
        fixture.request(InstructionManagementRequest::Close).await;
        let resumed=fixture.request(InstructionManagementRequest::Resume{scope:InstructionEditScope::Project,draft:updated.id.clone()}).await;
        assert!(matches!(resumed,InstructionManagementResult::Draft(ref draft) if draft.committed.is_some()));
        std::fs::write(project.join("AGENTS.md"),"LATER EXTERNAL").unwrap();
        assert!(matches!(fixture.request(InstructionManagementRequest::Save{draft:updated.id,generation:updated.generation}).await,InstructionManagementResult::FileSaved{..}));
        assert_eq!(std::fs::read_to_string(project.join("AGENTS.md")).unwrap(),"LATER EXTERNAL");
    });
}

#[test]
fn exact_project_legacy_import_is_reviewed_and_original_stays_inactive_on_disk() {
    let _environment = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut fixture=Fixture::new();let project=fixture.context.working_dir.clone().unwrap();
        let initialized=fixture.service.configure_non_git_project(&project,"setup-import",None,&InstructionStoreSeed::empty(),&[]).unwrap();
        let source=project.join(".jcode/prompt-overlay.md");let text="\nEXACT {{ literal }} 合成\r\n";std::fs::write(&source,text).unwrap();
        let draft=fixture.begin("prompt-overlay.md",InstructionEditAction::ImportLegacy).await;
        assert_eq!(draft.files.len(),2);assert!(!initialized.repository.root.join("system/common.md").exists());
        let review=fixture.request(InstructionManagementRequest::Review{draft:draft.id.clone(),generation:draft.generation}).await;
        assert!(matches!(review,InstructionManagementResult::Reviewed(ref value) if value.errors.is_empty()),"{review:?}");
        assert!(matches!(fixture.request(InstructionManagementRequest::Save{draft:draft.id,generation:draft.generation}).await,InstructionManagementResult::Saved{..}));
        let manifest=fixture.service.load_manifest(&initialized.repository).unwrap();assert_eq!(manifest.legacy_imports.len(),1);
        let content=fixture.service.read_file(&initialized.repository,"system/common.md",InstructionReadPolicy::WorkingTreeOnly).unwrap();
        assert_eq!(metadata::parse(&initialized.repository,Path::new("system/common.md"),&content.content).unwrap().body,text);
        assert_eq!(std::fs::read_to_string(source).unwrap(),text);
    });
}

#[test]
fn repository_history_restores_a_deleted_resource_and_exports_exact_bytes() {
    let _environment = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut fixture = Fixture::new();
            let root = fixture.repository.root.clone();
            let commit = fixture
                .service
                .inspect(&fixture.repository)
                .unwrap()
                .head
                .unwrap();
            let old = std::fs::read(root.join("modules/literal.md")).unwrap();
            std::fs::remove_file(root.join("modules/literal.md")).unwrap();
            git(&root, &["add", "modules/literal.md"]);
            git(&root, &["commit", "-m", "delete literal"]);
            let snapshot = fixture.open("").await;
            let target = InstructionInspectionTarget::Repository("global".into());
            let result = fixture
                .request(InstructionManagementRequest::Begin {
                    snapshot: snapshot.snapshot.clone(),
                    target: target.clone(),
                    action: InstructionEditAction::RestorePath {
                        revision: commit.clone(),
                        path: "modules/literal.md".into(),
                    },
                })
                .await;
            let draft = super::draft(result);
            assert!(!root.join("modules/literal.md").exists());
            fixture
                .request(InstructionManagementRequest::Review {
                    draft: draft.id.clone(),
                    generation: draft.generation,
                })
                .await;
            assert!(matches!(
                fixture
                    .request(InstructionManagementRequest::Save {
                        draft: draft.id,
                        generation: draft.generation
                    })
                    .await,
                InstructionManagementResult::Saved { .. }
            ));
            assert_eq!(std::fs::read(root.join("modules/literal.md")).unwrap(), old);
            let export = fixture
                .request(InstructionManagementRequest::ExportRevision {
                    snapshot: snapshot.snapshot,
                    target,
                    revision: commit,
                    path: Some("modules/literal.md".into()),
                })
                .await;
            let InstructionManagementResult::RevisionExport(export) = export else {
                panic!("{export:?}")
            };
            use base64::Engine;
            assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(&export.files[0].base64)
                    .unwrap(),
                old
            );
        });
}

#[test]
fn global_rename_cannot_silently_change_project_shadowed_references() {
    let _environment = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut fixture=Fixture::new();
        fixture.service.configure_non_git_project(fixture.context.working_dir.as_ref().unwrap(),"shadow-setup",None,&InstructionStoreSeed{manifest:InstructionStoreManifest::current(),files:vec![InstructionSeedFile{relative_path:"modules/shared.md".into(),content:b"---\nid: shared\nkind: module\n---\nPROJECT".to_vec()}]},&[]).unwrap();
        let snapshot=fixture.open("shared").await;let row=snapshot.resources.rows.iter().find(|row|row.id=="shared"&&row.scope=="global").unwrap();
        let result=fixture.request(InstructionManagementRequest::Begin{snapshot:snapshot.snapshot,target:InstructionInspectionTarget::Resource(row.key.clone()),action:InstructionEditAction::Rename{id:"new-shared".into()}}).await;
        assert!(matches!(result,InstructionManagementResult::Failed(ref error) if error.operation=="project shadow migration"),"{result:?}");
        assert!(fixture.repository.root.join("modules/shared.md").exists());
    });
}

#[test]
fn historical_skill_restore_includes_reference_bytes_and_removes_newer_package_files() {
    let _environment = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut fixture=Fixture::new();let root=fixture.repository.root.clone();let package=root.join("skills/history-skill");std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("SKILL.md"),"---\nid: history-skill\nkind: skill\nname: history-skill\ndescription: synthetic\n---\nOLD").unwrap();std::fs::write(package.join("reference.bin"),[0xff,1,2]).unwrap();
        git(&root,&["add","skills/history-skill"]);git(&root,&["commit","-m","old package"]);let revision=git(&root,&["rev-parse","HEAD"]);
        std::fs::write(package.join("reference.bin"),[0xfe,2,3]).unwrap();std::fs::write(package.join("newer.txt"),"NEW").unwrap();git(&root,&["add","skills/history-skill"]);git(&root,&["commit","-m","newer package"]);
        let draft=fixture.begin("history-skill",InstructionEditAction::Restore{revision}).await;
        assert!(draft.files.iter().any(|file|file.key.ends_with("newer.txt")&&file.deleted));
        let review=fixture.request(InstructionManagementRequest::Review{draft:draft.id.clone(),generation:draft.generation}).await;assert!(matches!(review,InstructionManagementResult::Reviewed(ref value) if value.errors.is_empty()),"{review:?}");
        let saved=fixture.request(InstructionManagementRequest::Save{draft:draft.id,generation:draft.generation}).await;assert!(matches!(saved,InstructionManagementResult::Saved{..}),"{saved:?}");
        assert_eq!(std::fs::read(package.join("reference.bin")).unwrap(),[0xff,1,2]);assert!(!package.join("newer.txt").exists());
    });
}

#[test]
fn invalid_roster_aliases_remain_in_the_complete_repair_source() {
    let fixture = Fixture::new();
    let source = "[aliases.valid]\ndescription='synthetic'\nmodels=['openai-oauth:fixture']\n[aliases.invalid]\ndescription='retained invalid entry'\nmodels=[]\n";
    let file = metadata::file(
        &fixture.repository,
        Path::new(crate::model_roster::ROSTER_PATH),
        Some(source),
    );
    assert!(matches!(
        file.metadata,
        InstructionEditMetadata::Damaged { .. }
    ));
    assert_eq!(file.body, source);
}

#[test]
fn metadata_noop_keeps_complete_original_formatting() {
    let fixture = Fixture::new();
    let source = "---\nid: shared\nkind: module\n# retained metadata comment\n---\nBODY";
    let metadata = metadata::file(
        &fixture.repository,
        Path::new("modules/shared.md"),
        Some(source),
    )
    .metadata;
    assert_eq!(
        metadata::apply(
            &fixture.repository,
            Path::new("modules/shared.md"),
            source,
            &InstructionDraftChange::Metadata {
                file: "modules/shared.md".into(),
                metadata
            }
        )
        .unwrap(),
        source
    );
}

#[test]
fn ecosystem_stale_save_and_duplicate_owner_preserve_external_work() {
    let fixture = Fixture::new();
    let project = fixture.context.working_dir.as_deref().unwrap();
    let path = project.join("AGENTS.md");
    std::fs::write(&path, "BASE").unwrap();
    let mut owner = fixture
        .service
        .open_ecosystem(
            "session",
            Some(project),
            InstructionEditScope::Project,
            &path,
        )
        .unwrap();
    let draft = owner.snapshot();
    assert!(
        fixture
            .service
            .resume_ecosystem("session", Some(project), &draft.id)
            .is_err()
    );
    owner
        .handle(
            &fixture.service,
            "session",
            Some(project),
            InstructionManagementRequest::Update {
                draft: draft.id.clone(),
                generation: 0,
                change: InstructionDraftChange::Body {
                    file: "AGENTS.md".into(),
                    body: "PROPOSED".into(),
                },
            },
        )
        .unwrap();
    owner
        .handle(
            &fixture.service,
            "session",
            Some(project),
            InstructionManagementRequest::Review {
                draft: draft.id.clone(),
                generation: 1,
            },
        )
        .unwrap();
    std::fs::write(&path, "EXTERNAL").unwrap();
    assert!(
        owner
            .handle(
                &fixture.service,
                "session",
                Some(project),
                InstructionManagementRequest::Save {
                    draft: draft.id,
                    generation: 1
                }
            )
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "EXTERNAL");
}

#[test]
fn manager_create_global_project_redefine_addendum_clear_and_external_commit_are_complete() {
    let _environment = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut fixture = Fixture::new();
            let project = fixture.context.working_dir.clone().unwrap();
            let project_repository = fixture
                .service
                .configure_non_git_project(
                    &project,
                    "create-journey",
                    None,
                    &InstructionStoreSeed::empty(),
                    &[],
                )
                .unwrap()
                .repository;
            for (scope, id, kind) in [
                (
                    InstructionEditScope::Global,
                    "created-module",
                    InstructionEditKind::Module,
                ),
                (
                    InstructionEditScope::Project,
                    "project-module",
                    InstructionEditKind::Module,
                ),
                (
                    InstructionEditScope::Global,
                    "created-agent",
                    InstructionEditKind::Agent,
                ),
            ] {
                let snapshot = fixture.open("").await;
                let fields = InstructionResourceFields {
                    id: id.into(),
                    kind,
                    name: (kind == InstructionEditKind::Agent).then(|| "Synthetic agent".into()),
                    description: (kind == InstructionEditKind::Agent)
                        .then(|| "Synthetic selection description".into()),
                    template: InstructionEditTemplate::Plain,
                    availability: (kind == InstructionEditKind::Agent)
                        .then_some(InstructionEditAvailability::Both),
                    target: None,
                    includes: Vec::new(),
                    allowed_tools: None,
                };
                let draft = super::draft(
                    fixture
                        .request(InstructionManagementRequest::Begin {
                            snapshot: snapshot.snapshot,
                            target: InstructionInspectionTarget::Session,
                            action: InstructionEditAction::Create { scope, fields },
                        })
                        .await,
                );
                super::package_tests::save(&fixture, &draft).await;
            }
            assert!(
                fixture
                    .repository
                    .root
                    .join("modules/created-module.md")
                    .is_file()
            );
            assert!(
                project_repository
                    .root
                    .join("modules/project-module.md")
                    .is_file()
            );
            let redefinition = fixture
                .begin("created-module", InstructionEditAction::RedefineInProject)
                .await;
            assert!(
                redefinition
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("Project redefinition"))
            );
            super::package_tests::save(&fixture, &redefinition).await;
            assert!(
                project_repository
                    .root
                    .join("modules/created-module.md")
                    .is_file()
            );
            let addendum = fixture
                .begin(
                    "created-agent",
                    InstructionEditAction::Addendum {
                        id: "created-addendum".into(),
                    },
                )
                .await;
            super::package_tests::save(&fixture, &addendum).await;
            assert!(
                project_repository
                    .root
                    .join("addenda/created-addendum.md")
                    .is_file()
            );
            let source = fixture.repository.root.join("modules/literal.md");
            let cleared = fixture.begin("literal", InstructionEditAction::Clear).await;
            super::package_tests::save(&fixture, &cleared).await;
            assert!(
                metadata::parse(
                    &fixture.repository,
                    Path::new("modules/literal.md"),
                    &std::fs::read_to_string(&source).unwrap()
                )
                .unwrap()
                .body
                .is_empty()
            );
            let external = "---\nid: literal\nkind: module\n---\nEXTERNAL SOURCE";
            std::fs::write(&source, external).unwrap();
            let draft = fixture
                .begin("literal", InstructionEditAction::CommitExternal)
                .await;
            super::package_tests::save(&fixture, &draft).await;
            let head = fixture
                .service
                .inspect(&fixture.repository)
                .unwrap()
                .head
                .unwrap();
            assert_eq!(
                fixture
                    .service
                    .content_at_revision(&fixture.repository, &head, "modules/literal.md")
                    .unwrap()
                    .content,
                external
            );
        });
}
