use super::*;
use crate::execution::{
    ControlOperation, ControlReply, RunState, control_transport::control_in_store,
};
use std::time::Duration;

struct Fixture {
    temporary: Option<tempfile::TempDir>,
    state: PathBuf,
    home: PathBuf,
    checkout: PathBuf,
    location: LocationId,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let state = temporary.path().join("state");
        let home = temporary.path().join("home");
        let checkout = temporary.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "base",
            ],
        ] {
            assert!(
                std::process::Command::new("git")
                    .current_dir(&checkout)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        std::fs::write(checkout.join("payload"), b"retained fixture bytes").unwrap();
        let service = WorkspaceService::new(&state);
        service.initialize(RequestId::new()).unwrap();
        let EntityId::Project(project) = change(
            &service,
            OrganizationChange::CreateProject {
                name: "fixture".into(),
            },
        ) else {
            panic!()
        };
        let EntityId::Repository(repository) = change(
            &service,
            OrganizationChange::CreateRepository {
                name: "repo".into(),
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
        let EntityId::Location(location) = change(
            &service,
            OrganizationChange::RegisterLocation {
                name: "checkout".into(),
                path: checkout.clone(),
                registration: Registration::Checkout {
                    home: Home::Project(project),
                    repository,
                },
            },
        ) else {
            panic!()
        };
        Self {
            temporary: Some(temporary),
            state,
            home,
            checkout,
            location,
        }
    }
    async fn call(&self, request: CloseoutRequest) -> CloseoutResponse {
        match dispatch_at(
            self.state.clone(),
            self.home.clone(),
            request,
            "fixture-human-client".into(),
        )
        .await
        {
            WorkspaceResponse::Closeout(value) => *value,
            response => panic!("Unexpected response: {response:?}"),
        }
    }
    async fn begin(&self) -> CloseoutRecord {
        let service = WorkspaceService::new(&self.state);
        let CloseoutResponse::Record(record) = self
            .call(CloseoutRequest::Begin {
                request: RequestId::new(),
                expected_revision: service.status().unwrap().revision,
                spec: CloseoutSpec {
                    location: self.location,
                    expected_generation: 1,
                    preservation_directory: None,
                    conditional_no_loss: false,
                    full_archive: true,
                },
            })
            .await
        else {
            panic!()
        };
        *record
    }
    async fn submit(
        &self,
        record: &CloseoutRecord,
        action: CloseoutAction,
    ) -> CloseoutActionRecord {
        let CloseoutResponse::Action(action) = self
            .call(CloseoutRequest::Execute {
                request: RequestId::new(),
                spec: CloseoutActionSpec {
                    operation: record.operation,
                    expected_revision: record.revision,
                    action,
                },
            })
            .await
        else {
            panic!()
        };
        *action
    }
    async fn wait(&self, action: &CloseoutActionRecord) -> CloseoutActionRecord {
        let store = ExecutionStore::open(&self.home).unwrap();
        let reply = tokio::time::timeout(
            Duration::from_secs(240),
            control_in_store(&store, &action.run_id, ControlOperation::Wait),
        )
        .await
        .unwrap()
        .unwrap();
        let ControlReply::Snapshot { record } = reply else {
            panic!("{reply:?}")
        };
        assert!(record.state.terminal());
        assert!(record.complete, "{record:?}");
        let CloseoutResponse::Action(action) = self
            .call(CloseoutRequest::InspectAction {
                request: action.request,
            })
            .await
        else {
            panic!()
        };
        *action
    }
    async fn perform(
        &self,
        record: &CloseoutRecord,
        action: CloseoutAction,
    ) -> CloseoutActionResult {
        let receipt = self.submit(record, action).await;
        let receipt = self.wait(&receipt).await;
        assert!(receipt.issue.is_none(), "{receipt:?}");
        receipt.result.unwrap()
    }
    async fn cleanup(&self) {
        let store = ExecutionStore::open(&self.home).unwrap();
        for run in store.unresolved_runs().unwrap() {
            assert_eq!(run.tool, CLOSEOUT_EXECUTION_TOOL);
            let _ = control_in_store(
                &store,
                &run.id,
                ControlOperation::Stop {
                    cause: jcode_tool_types::StopCause::HumanCancellation,
                },
            )
            .await;
            tokio::time::timeout(
                Duration::from_secs(60),
                control_in_store(&store, &run.id, ControlOperation::Wait),
            )
            .await
            .unwrap()
            .unwrap();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.home.join("execution").exists()
            && ExecutionStore::open(&self.home)
                .and_then(|store| store.unresolved_runs())
                .map_or(true, |runs| !runs.is_empty())
        {
            eprintln!(
                "Retained unresolved closeout fixture: {}",
                self.temporary.take().unwrap().keep().display()
            );
        }
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

#[test]
fn owned_closeout_adapter_preserves_approves_removes_and_replays_without_a_session() {
    let _environment = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let fixture = Fixture::new();
            let mut record = fixture.begin().await;
            let action = fixture.submit(&record, CloseoutAction::Refresh).await;
            let CloseoutResponse::Action(replayed) = fixture
                .call(CloseoutRequest::Execute {
                    request: action.request,
                    spec: action.spec.clone(),
                })
                .await
            else {
                panic!()
            };
            assert_eq!(replayed.run_id, action.run_id);
            let complete = fixture.wait(&action).await;
            assert!(complete.issue.is_none(), "{complete:?}");
            let CloseoutActionResult::Record(refreshed) = complete.result.clone().unwrap() else {
                panic!()
            };
            record = *refreshed;
            let conflict = dispatch_at(
                fixture.state.clone(),
                fixture.home.clone(),
                CloseoutRequest::Execute {
                    request: action.request,
                    spec: CloseoutActionSpec {
                        action: CloseoutAction::Preserve,
                        ..action.spec.clone()
                    },
                },
                "fixture-peer".into(),
            )
            .await;
            assert!(matches!(
                conflict,
                WorkspaceResponse::Error(Issue {
                    code: IssueCode::Conflict,
                    ..
                })
            ));
            let foreign = fixture
                .call(CloseoutRequest::Execution {
                    request: action.request,
                    control: ExecutionRequest::Inspect {
                        run_id: action.run_id.clone(),
                    },
                })
                .await;
            assert!(matches!(foreign, CloseoutResponse::Execution(_)));
            let denied = dispatch_at(
                fixture.state.clone(),
                fixture.home.clone(),
                CloseoutRequest::Execution {
                    request: action.request,
                    control: ExecutionRequest::Inspect {
                        run_id: format!("run-{}", "0".repeat(64)),
                    },
                },
                "fixture-peer".into(),
            )
            .await;
            assert!(matches!(
                denied,
                WorkspaceResponse::Error(Issue {
                    code: IssueCode::InvalidIdentity,
                    ..
                })
            ));
            let CloseoutActionResult::Record(preserved) =
                fixture.perform(&record, CloseoutAction::Preserve).await
            else {
                panic!()
            };
            record = *preserved;
            let CloseoutActionResult::Review(review) = fixture
                .perform(&record, CloseoutAction::ReviewRemoval)
                .await
            else {
                panic!()
            };
            assert!(review.issues.is_empty(), "{review:?}");
            let CloseoutResponse::Record(current) = fixture
                .call(CloseoutRequest::Inspect {
                    operation: record.operation,
                })
                .await
            else {
                panic!()
            };
            record = *current;
            let CloseoutActionResult::Record(approved) = fixture
                .perform(
                    &record,
                    CloseoutAction::ApproveRemoval { review: review.id },
                )
                .await
            else {
                panic!()
            };
            record = *approved;
            let CloseoutActionResult::Record(closed) =
                fixture.perform(&record, CloseoutAction::Finish).await
            else {
                panic!()
            };
            assert_eq!(closed.stage, CloseoutStage::Closed);
            assert!(!fixture.checkout.exists());
            let CloseoutResponse::History(history) = fixture
                .call(CloseoutRequest::History {
                    location: fixture.location,
                })
                .await
            else {
                panic!()
            };
            assert!(history.report.unwrap().is_file());
            let mut captures = std::fs::read_dir(&closed.preservation_directory)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.join("verified-restore/payload").is_file());
            assert_eq!(
                std::fs::read(captures.next().unwrap().join("verified-restore/payload")).unwrap(),
                b"retained fixture bytes"
            );
            assert!(!fixture.home.join("sessions").exists());
            let store = ExecutionStore::open(&fixture.home).unwrap();
            assert_eq!(
                store
                    .list(CLOSEOUT_EXECUTION_SESSION, None, 100)
                    .unwrap()
                    .len(),
                5
            );
            assert_eq!(
                WorkspaceService::new(&fixture.state)
                    .inspect_closeout_action(action.request)
                    .unwrap(),
                complete
            );
            fixture.cleanup().await;
        });
}

#[test]
fn owned_closeout_adapter_stop_is_terminal_and_new_attempt_is_explicit() {
    let _environment = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let fixture = Fixture::new();
            for i in 0..100 {
                std::fs::write(
                    fixture.checkout.join(format!("data-{i}")),
                    vec![7u8; 64 * 1024],
                )
                .unwrap();
            }
            let record = fixture.begin().await;
            let action = fixture.submit(&record, CloseoutAction::Refresh).await;
            let stopped = fixture
                .call(CloseoutRequest::Execution {
                    request: action.request,
                    control: ExecutionRequest::Stop {
                        run_id: action.run_id.clone(),
                    },
                })
                .await;
            assert!(matches!(
                stopped,
                CloseoutResponse::Execution(
                    jcode_tool_types::execution::ExecutionResponse::Control { accepted: true, .. }
                )
            ));
            let complete = fixture.wait(&action).await;
            let store = ExecutionStore::open(&fixture.home).unwrap();
            assert_eq!(
                store.inspect(&action.run_id).unwrap().unwrap().state,
                RunState::Cancelled
            );
            let CloseoutResponse::Action(replay) = fixture
                .call(CloseoutRequest::Execute {
                    request: action.request,
                    spec: action.spec,
                })
                .await
            else {
                panic!()
            };
            assert_eq!(*replay, complete);
            let CloseoutResponse::Record(current) = fixture
                .call(CloseoutRequest::Inspect {
                    operation: record.operation,
                })
                .await
            else {
                panic!()
            };
            let retried = fixture.submit(&current, CloseoutAction::Refresh).await;
            assert_ne!(retried.run_id, action.run_id);
            assert!(fixture.wait(&retried).await.issue.is_none());
            assert!(fixture.checkout.join("payload").is_file());
            fixture.cleanup().await;
        });
}

impl Fixture {
    async fn begin_conditional(&self, conditional: bool) -> CloseoutRecord {
        let service = WorkspaceService::new(&self.state);
        let CloseoutResponse::Record(record) = self
            .call(CloseoutRequest::Begin {
                request: RequestId::new(),
                expected_revision: service.status().unwrap().revision,
                spec: CloseoutSpec {
                    location: self.location,
                    expected_generation: 1,
                    preservation_directory: None,
                    conditional_no_loss: conditional,
                    full_archive: true,
                },
            })
            .await
        else {
            panic!()
        };
        *record
    }
    /// A primary placed in a registered location of the checkout's project,
    /// or a standalone root outside it. Never a cwd inside the checkout.
    fn placed_session(&self, name: &str, in_project: bool) -> crate::session::Session {
        let service = WorkspaceService::new(&self.state);
        let root = self.temporary.as_ref().unwrap().path().join(name);
        std::fs::create_dir(&root).unwrap();
        let registration = if in_project {
            let Entity::Location(checkout) =
                service.inspect(EntityId::Location(self.location)).unwrap()
            else {
                panic!()
            };
            Registration::Directory {
                home: checkout.home.unwrap(),
            }
        } else {
            Registration::Standalone
        };
        let EntityId::Location(location) = change(
            &service,
            OrganizationChange::RegisterLocation {
                name: name.into(),
                path: root.clone(),
                registration,
            },
        ) else {
            panic!()
        };
        let placement = if in_project {
            let Entity::Location(checkout) =
                service.inspect(EntityId::Location(self.location)).unwrap()
            else {
                panic!()
            };
            match checkout.home.unwrap() {
                Home::Project(project) => Placement::Project(project),
                Home::WorkArea(area) => Placement::WorkArea(area),
            }
        } else {
            Placement::Standalone(location)
        };
        let prepared = service
            .prepare_primary_location(placement, Some(&root), OperationId::new())
            .unwrap();
        let mut session = crate::session::Session::create_with_id(
            format!("session_agent_closeout_{name}_{}", RequestId::new()),
            None,
            None,
        );
        session.working_dir = Some(root.to_string_lossy().into());
        session.location = Some(prepared.location.clone());
        session
    }
    async fn agent(
        &self,
        session: &crate::session::Session,
        record: &CloseoutRecord,
        action: CloseoutAction,
    ) -> Result<CloseoutActionRecord> {
        agent_action_at(
            self.state.clone(),
            self.home.clone(),
            session.clone(),
            RequestId::new(),
            CloseoutActionSpec {
                operation: record.operation,
                expected_revision: record.revision,
                action,
            },
        )
        .await
    }
    async fn current(&self, operation: OperationId) -> CloseoutRecord {
        WorkspaceService::new(&self.state)
            .inspect_closeout(operation)
            .unwrap()
    }
}

fn settled(record: CloseoutActionRecord) -> CloseoutActionResult {
    assert!(record.issue.is_none(), "{record:?}");
    record.result.unwrap()
}

#[test]
fn agent_closeout_needs_scope_never_approves_and_finishes_only_its_conditional_declaration() {
    let _environment = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let fixture = Fixture::new();
            let agent = fixture.placed_session("control", true);
            let peer = fixture.placed_session("peer", true);
            let outsider = fixture.placed_session("outside", false);
            let mut record = fixture.begin_conditional(true).await;

            // Read-only discovery is not write scope.
            let denied = fixture
                .agent(&outsider, &record, CloseoutAction::Refresh)
                .await
                .unwrap_err();
            assert_eq!(denied.code, IssueCode::PermissionRequired);

            let refreshed = fixture
                .agent(&agent, &record, CloseoutAction::Refresh)
                .await
                .unwrap();
            assert_eq!(refreshed.actor, CloseoutActor::Agent);
            assert_eq!(refreshed.initiated_by, agent.id);
            let CloseoutActionResult::Record(next) = settled(refreshed) else {
                panic!()
            };
            record = *next;
            let CloseoutActionResult::Record(next) = settled(
                fixture
                    .agent(&agent, &record, CloseoutAction::Preserve)
                    .await
                    .unwrap(),
            ) else {
                panic!()
            };
            record = *next;
            let CloseoutActionResult::Review(review) = settled(
                fixture
                    .agent(&agent, &record, CloseoutAction::ReviewRemoval)
                    .await
                    .unwrap(),
            ) else {
                panic!()
            };
            assert!(review.issues.is_empty(), "{review:?}");
            record = fixture.current(record.operation).await;

            // Agents never approve, recover or finish someone else's removal.
            for action in [
                CloseoutAction::ApproveRemoval { review: review.id },
                CloseoutAction::ReviewRecovery {
                    choice: CloseoutRecoveryAction::RestartPreparation,
                },
                CloseoutAction::Finish,
            ] {
                let error = fixture.agent(&agent, &record, action).await.unwrap_err();
                assert_eq!(error.code, IssueCode::PermissionRequired, "{error:?}");
            }
            // A trusted client cannot impersonate the agent's declaration.
            let impersonated = dispatch_at(
                fixture.state.clone(),
                fixture.home.clone(),
                CloseoutRequest::Execute {
                    request: RequestId::new(),
                    spec: CloseoutActionSpec {
                        operation: record.operation,
                        expected_revision: record.revision,
                        action: CloseoutAction::DeclareNoLoss {
                            review: review.id,
                            assessment: "synthetic".into(),
                        },
                    },
                },
                "fixture-human-client".into(),
            )
            .await;
            assert!(matches!(
                impersonated,
                WorkspaceResponse::Error(Issue {
                    code: IssueCode::PermissionRequired,
                    ..
                })
            ));

            let CloseoutActionResult::Record(authorized) = settled(
                fixture
                    .agent(
                        &agent,
                        &record,
                        CloseoutAction::DeclareNoLoss {
                            review: review.id,
                            assessment: "synthetic no-loss assessment".into(),
                        },
                    )
                    .await
                    .unwrap(),
            ) else {
                panic!()
            };
            assert_eq!(authorized.stage, CloseoutStage::Authorized);
            assert!(matches!(
                &authorized.authorization.as_ref().unwrap().source,
                CloseoutAuthorizationSource::Conditional { session, .. } if *session == agent.id
            ));
            record = *authorized;
            let error = fixture
                .agent(&peer, &record, CloseoutAction::Finish)
                .await
                .unwrap_err();
            assert_eq!(error.code, IssueCode::PermissionRequired);
            let CloseoutActionResult::Record(closed) = settled(
                fixture
                    .agent(&agent, &record, CloseoutAction::Finish)
                    .await
                    .unwrap(),
            ) else {
                panic!()
            };
            assert_eq!(closed.stage, CloseoutStage::Closed);
            assert!(!fixture.checkout.exists());
            fixture.cleanup().await;
        });
}

#[test]
fn agent_declaration_without_human_conditional_authority_is_refused_and_recorded() {
    let _environment = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let fixture = Fixture::new();
            let agent = fixture.placed_session("control", true);
            let mut record = fixture.begin_conditional(false).await;
            for action in [CloseoutAction::Refresh, CloseoutAction::Preserve] {
                let CloseoutActionResult::Record(next) =
                    settled(fixture.agent(&agent, &record, action).await.unwrap())
                else {
                    panic!()
                };
                record = *next;
            }
            let CloseoutActionResult::Review(review) = settled(
                fixture
                    .agent(&agent, &record, CloseoutAction::ReviewRemoval)
                    .await
                    .unwrap(),
            ) else {
                panic!()
            };
            record = fixture.current(record.operation).await;
            let refused = fixture
                .agent(
                    &agent,
                    &record,
                    CloseoutAction::DeclareNoLoss {
                        review: review.id,
                        assessment: "synthetic".into(),
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                refused.issue.as_ref().map(|issue| issue.code),
                Some(IssueCode::PermissionRequired),
                "{refused:?}"
            );
            let current = fixture.current(record.operation).await;
            assert_eq!(current.stage, CloseoutStage::ReadyForApproval);
            assert!(current.authorization.is_none());
            assert!(fixture.checkout.join("payload").is_file());
            fixture.cleanup().await;
        });
}
