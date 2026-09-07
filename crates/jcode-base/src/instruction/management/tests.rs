use super::*;
use crate::instruction::inspection::InspectionWorker;

struct Fixture {
    _root: tempfile::TempDir,
    service: InstructionRepositoryService,
    context: InspectionContext,
    inspector: InspectionWorker,
    manager: InstructionManagementWorker,
    repository: InstructionRepositoryRef,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let project = root.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let service = InstructionRepositoryService::from_paths(&home, root.path().join("state"));
        let files = [
            (
                "modules/shared.md",
                "---\nid: shared\nkind: module\n---\nOLD",
            ),
            (
                "modules/consumer.md",
                "---\nid: consumer\nkind: module\ntemplate: handlebars\n---\nBEFORE {{>  shared  }} AFTER",
            ),
            (
                "modules/literal.md",
                "---\nid: literal\nkind: module\n---\n{{> shared}}",
            ),
            (
                "notifications/todo-auto-poke.md",
                "---\nid: todo-auto-poke\nkind: notification\ntemplate: handlebars\n---\nSYNTHETIC {{count}} {{plural}}",
            ),
        ];
        let repository = service
            .initialize_global(
                &InstructionStoreSeed {
                    manifest: InstructionStoreManifest::current(),
                    files: files
                        .into_iter()
                        .map(|(path, content)| InstructionSeedFile {
                            relative_path: path.into(),
                            content: content.as_bytes().to_vec(),
                        })
                        .collect(),
                },
                &[],
            )
            .unwrap()
            .repository;
        let context = InspectionContext {
            session_id: "manager-test".into(),
            working_dir: Some(project),
            active_agent: None,
            stored_system: None,
            is_selfdev: false,
            capabilities: crate::prompt::PromptCapabilities { mermaid: false },
            roster_catalog: None,
        };
        Self {
            _root: root,
            service,
            context,
            inspector: InspectionWorker::default(),
            manager: InstructionManagementWorker::default(),
            repository,
        }
    }
    async fn open(&mut self, search: &str) -> InstructionInspectionSnapshot {
        let context = self.context.clone();
        let reply = self
            .inspector
            .submit(
                self.service.clone(),
                context.session_id.clone(),
                move || Ok(context),
                InstructionInspectionRequest::Open {
                    filter: InstructionFilter {
                        search: search.into(),
                        ..Default::default()
                    },
                },
            )
            .await
            .unwrap();
        let InstructionInspectionResult::Opened(snapshot) = reply.result else {
            panic!("{reply:?}")
        };
        snapshot
    }
    async fn request(&self, request: InstructionManagementRequest) -> InstructionManagementResult {
        self.manager
            .submit(
                self.service.clone(),
                self.context.clone(),
                self.inspector.target_resolver(),
                request,
            )
            .await
            .unwrap()
            .result
    }
    async fn begin(&mut self, search: &str, action: InstructionEditAction) -> InstructionEditDraft {
        let snapshot = self.open(search).await;
        let row = snapshot
            .resources
            .rows
            .iter()
            .find(|row| row.id == search)
            .unwrap();
        let result = self
            .request(InstructionManagementRequest::Begin {
                snapshot: snapshot.snapshot,
                target: InstructionInspectionTarget::Resource(row.key.clone()),
                action,
            })
            .await;
        let InstructionManagementResult::Draft(draft) = result else {
            panic!("{result:?}")
        };
        draft
    }
}
fn draft(result: InstructionManagementResult) -> InstructionEditDraft {
    let InstructionManagementResult::Draft(draft) = result else {
        panic!("{result:?}")
    };
    draft
}

#[test]
fn valid_resource_identity_cannot_change_through_raw_repair() {
    let fixture = Fixture::new();
    let original =
        std::fs::read_to_string(fixture.repository.root.join("modules/shared.md")).unwrap();
    let result = metadata::apply(
        &fixture.repository,
        Path::new("modules/shared.md"),
        &original,
        &InstructionDraftChange::RepairSource {
            file: "modules/shared.md".into(),
            source: "---\nid: renamed\nkind: module\n---\nBODY".into(),
        },
    );
    assert!(result.is_err());
    assert_eq!(
        std::fs::read_to_string(fixture.repository.root.join("modules/shared.md")).unwrap(),
        original
    );
}

#[test]
fn management_edit_review_save_uses_server_source_and_exact_generations() {
    let _lock = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut fixture = Fixture::new();
            let opened = fixture.begin("shared", InstructionEditAction::Edit).await;
            assert_eq!(opened.files[0].body, "OLD");
            let current = draft(
                fixture
                    .request(InstructionManagementRequest::Update {
                        draft: opened.id.clone(),
                        generation: opened.generation,
                        change: InstructionDraftChange::Body {
                            file: opened.files[0].key.clone(),
                            body: "NEW 合成 {{literal}}".into(),
                        },
                    })
                    .await,
            );
            assert!(
                std::fs::read_to_string(fixture.repository.root.join("modules/shared.md"))
                    .unwrap()
                    .ends_with("OLD")
            );
            assert!(matches!(
                fixture
                    .request(InstructionManagementRequest::Save {
                        draft: current.id.clone(),
                        generation: current.generation
                    })
                    .await,
                InstructionManagementResult::Failed(_)
            ));
            let reviewed = fixture
                .request(InstructionManagementRequest::Review {
                    draft: current.id.clone(),
                    generation: current.generation,
                })
                .await;
            let InstructionManagementResult::Reviewed(reviewed) = reviewed else {
                panic!("{reviewed:?}")
            };
            assert!(reviewed.errors.is_empty(), "{:?}", reviewed.errors);
            assert!(
                reviewed
                    .previews
                    .iter()
                    .any(|preview| preview.content.contains("NEW 合成 {{literal}}"))
            );
            assert!(
                reviewed
                    .previews
                    .iter()
                    .any(|preview| preview.title.contains("consumer"))
            );
            let saved = fixture
                .request(InstructionManagementRequest::Save {
                    draft: current.id.clone(),
                    generation: current.generation,
                })
                .await;
            assert!(
                matches!(
                    saved,
                    InstructionManagementResult::Saved {
                        no_change: false,
                        ..
                    }
                ),
                "{saved:?}"
            );
            assert!(
                std::fs::read_to_string(fixture.repository.root.join("modules/shared.md"))
                    .unwrap()
                    .ends_with("NEW 合成 {{literal}}")
            );
            assert!(matches!(
                fixture
                    .request(InstructionManagementRequest::Update {
                        draft: current.id,
                        generation: current.generation - 1,
                        change: InstructionDraftChange::Body {
                            file: "modules/shared.md".into(),
                            body: "STALE".into()
                        }
                    })
                    .await,
                InstructionManagementResult::Failed(_)
            ));
        });
}

#[test]
fn management_registered_preview_blocks_unknown_variables_and_empty_controls() {
    let _lock = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut fixture = Fixture::new();
            let opened = fixture
                .begin("todo-auto-poke", InstructionEditAction::Edit)
                .await;
            for body in ["{{not_a_typed_field}}", ""] {
                let current = draft(
                    fixture
                        .request(InstructionManagementRequest::Update {
                            draft: opened.id.clone(),
                            generation: if body.is_empty() {
                                opened.generation + 1
                            } else {
                                opened.generation
                            },
                            change: InstructionDraftChange::Body {
                                file: opened.files[0].key.clone(),
                                body: body.into(),
                            },
                        })
                        .await,
                );
                let reviewed = fixture
                    .request(InstructionManagementRequest::Review {
                        draft: current.id.clone(),
                        generation: current.generation,
                    })
                    .await;
                let InstructionManagementResult::Reviewed(reviewed) = reviewed else {
                    panic!("{reviewed:?}")
                };
                assert!(!reviewed.errors.is_empty());
                assert!(!reviewed.draft.reviewed);
                assert!(matches!(
                    fixture
                        .request(InstructionManagementRequest::Save {
                            draft: current.id,
                            generation: current.generation
                        })
                        .await,
                    InstructionManagementResult::Failed(_)
                ));
            }
            assert!(
                std::fs::read_to_string(
                    fixture
                        .repository
                        .root
                        .join("notifications/todo-auto-poke.md")
                )
                .unwrap()
                .contains("SYNTHETIC {{count}}")
            );
        });
}

#[test]
fn management_rename_repairs_only_semantic_template_references() {
    let _lock = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut fixture = Fixture::new();
            let renamed = fixture
                .begin(
                    "shared",
                    InstructionEditAction::Rename {
                        id: "renamed".into(),
                    },
                )
                .await;
            assert_eq!(renamed.files.len(), 3);
            let InstructionManagementResult::Reviewed(review) = fixture
                .request(InstructionManagementRequest::Review {
                    draft: renamed.id.clone(),
                    generation: renamed.generation,
                })
                .await
            else {
                panic!("review")
            };
            assert!(review.errors.is_empty(), "{:?}", review.errors);
            assert!(matches!(
                fixture
                    .request(InstructionManagementRequest::Save {
                        draft: renamed.id,
                        generation: renamed.generation
                    })
                    .await,
                InstructionManagementResult::Saved { .. }
            ));
            assert!(!fixture.repository.root.join("modules/shared.md").exists());
            assert!(
                std::fs::read_to_string(fixture.repository.root.join("modules/consumer.md"))
                    .unwrap()
                    .contains("{{>  renamed  }}")
            );
            assert!(
                std::fs::read_to_string(fixture.repository.root.join("modules/literal.md"))
                    .unwrap()
                    .contains("{{> shared}}")
            );
        });
}

#[test]
fn management_rejects_expired_snapshot_and_project_context_changes() {
    let _lock = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut fixture = Fixture::new();
            let snapshot = fixture.open("shared").await;
            let key = snapshot
                .resources
                .rows
                .iter()
                .find(|row| row.id == "shared")
                .unwrap()
                .key
                .clone();
            fixture.open("shared").await;
            assert!(matches!(
                fixture
                    .request(InstructionManagementRequest::Begin {
                        snapshot: snapshot.snapshot,
                        target: InstructionInspectionTarget::Resource(key),
                        action: InstructionEditAction::Edit
                    })
                    .await,
                InstructionManagementResult::Failed(_)
            ));
        });
}

mod package_tests;
