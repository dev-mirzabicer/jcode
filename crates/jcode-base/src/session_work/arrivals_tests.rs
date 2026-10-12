use super::*;
use crate::session_work::SessionWorkStore;
use crate::session_work::stop::{self, BackgroundWork, DeclareInput, DeclareOutcome};
use jcode_session_work_types::{
    ActivationOrigin, DeclarationState, ExhaustedCounter, FailureClass, ItemSource,
    SessionWorkActivation, SessionWorkRole, TimeoutSource, TurnOutcome, WaitCondition, WaitTimeout,
    WaitTimeoutDefaults, WithdrawCause,
};

fn at(seconds: i64) -> DateTime<Utc> {
    chrono::DateTime::from_timestamp(1_800_000_000 + seconds, 0).unwrap()
}

fn alias(text: &str) -> ItemAlias {
    text.parse().unwrap()
}

fn item(text: &str, policy: FinishPolicy) -> SessionItem {
    SessionItem {
        alias: alias(text),
        id: uuid::Uuid::nil(),
        label: format!("label-{text}"),
        policy,
        created_at: at(0),
        finished_at: Some(at(5)),
    }
}

fn source(kind: ItemKind, policy: FinishPolicy) -> ItemSource {
    ItemSource {
        request: "synthetic".into(),
        kind,
        label: "synthetic".into(),
        policy,
    }
}

fn stop_record(number: u32, declaration: Declaration, state: DeclarationState) -> StopRecord {
    StopRecord {
        number,
        turn: 1,
        declaration,
        state,
        declared_at: at(1),
        settled_at: None,
        final_text: None,
        workflow_revision: None,
    }
}

fn wait_on(condition: &str) -> Declaration {
    Declaration::Wait {
        condition: condition.parse().unwrap(),
        timeout: WaitTimeout::Never,
        timeout_source: TimeoutSource::Explicit,
        summary: "synthetic".into(),
    }
}

fn completion() -> Declaration {
    Declaration::Complete {
        summary: None,
        leave_running: Vec::new(),
        closeout: false,
        documents: Vec::new(),
        accepted: false,
    }
}

fn human(disposition: HumanDisposition) -> ArrivalClass {
    ArrivalClass::Human { disposition }
}

fn finished_item(kind: ItemKind, policy: FinishPolicy) -> ArrivalClass {
    ArrivalClass::ItemFinished {
        item: source(kind, policy),
    }
}

fn answer(policy: FinishPolicy) -> ArrivalClass {
    ArrivalClass::Answer {
        item: source(ItemKind::Question, policy),
    }
}

fn waiting(condition: &str) -> HeldWhile {
    HeldWhile::Waiting {
        condition: condition.parse().unwrap(),
    }
}

/// Every cell of the arrival table, with the arrival's own item where it has
/// one. Stop 4 waits on `any(q1, all(t1, t2))`; `t2` has already finished.
#[test]
fn every_cell_of_the_arrival_table() {
    let condition = "any(q1, all(t1, t2))";
    let pending_wait = SessionState::Declared(stop_record(
        4,
        wait_on(condition),
        DeclarationState::Pending,
    ));
    let effective_wait = SessionState::Declared(stop_record(
        4,
        wait_on(condition),
        DeclarationState::Effective,
    ));
    let pending_completion =
        SessionState::Declared(stop_record(4, completion(), DeclarationState::Pending));
    let finished = |candidate: ItemAlias| candidate == alias("t2");
    let held_waiting = ArrivalRoute::Held {
        held_while: waiting(condition),
    };
    let held_stopped = ArrivalRoute::Held {
        held_while: HeldWhile::Stopped,
    };
    let deliver = ArrivalRoute::Deliver;
    let wake = FinishPolicy::Wake;
    let hold = FinishPolicy::Hold;

    struct Cell {
        name: &'static str,
        class: ArrivalClass,
        item: Option<SessionItem>,
        working: ArrivalRoute,
        declared: ArrivalRoute,
        completed: ArrivalRoute,
        interrupted: ArrivalRoute,
    }
    let cells = vec![
        Cell {
            name: "message, queue",
            class: human(HumanDisposition::Queue),
            item: None,
            working: deliver.clone(),
            declared: held_waiting.clone(),
            completed: deliver.clone(),
            interrupted: deliver.clone(),
        },
        Cell {
            name: "message, send and keep the stop",
            class: human(HumanDisposition::SendKeepStop),
            item: None,
            working: deliver.clone(),
            declared: ArrivalRoute::KeepsStop { stop: 4 },
            completed: deliver.clone(),
            interrupted: deliver.clone(),
        },
        Cell {
            name: "message, revoke and send",
            class: human(HumanDisposition::RevokeAndSend),
            item: None,
            working: deliver.clone(),
            declared: ArrivalRoute::RevokesStop { stop: 4 },
            completed: deliver.clone(),
            interrupted: deliver.clone(),
        },
        Cell {
            name: "awaited item that meets the condition",
            class: answer(wake),
            item: Some(item("q1", wake)),
            working: deliver.clone(),
            declared: ArrivalRoute::EndsStop { stop: 4 },
            completed: deliver.clone(),
            interrupted: deliver.clone(),
        },
        Cell {
            name: "awaited item that does not meet the condition yet",
            class: finished_item(ItemKind::Task, hold),
            item: Some(SessionItem {
                finished_at: Some(at(6)),
                ..item("t3", hold)
            }),
            working: deliver.clone(),
            declared: held_waiting.clone(),
            completed: held_stopped.clone(),
            interrupted: held_stopped.clone(),
        },
        Cell {
            name: "answer not awaited, wake",
            class: answer(wake),
            item: Some(item("q2", wake)),
            working: deliver.clone(),
            declared: held_waiting.clone(),
            completed: deliver.clone(),
            interrupted: deliver.clone(),
        },
        Cell {
            name: "answer not awaited, hold",
            class: answer(hold),
            item: Some(item("q2", hold)),
            working: deliver.clone(),
            declared: held_waiting.clone(),
            completed: held_stopped.clone(),
            interrupted: held_stopped.clone(),
        },
        Cell {
            name: "background task not awaited, wake",
            class: finished_item(ItemKind::Task, wake),
            item: Some(item("t5", wake)),
            working: deliver.clone(),
            declared: held_waiting.clone(),
            completed: deliver.clone(),
            interrupted: held_stopped.clone(),
        },
        Cell {
            name: "sub-agent run not awaited, hold",
            class: finished_item(ItemKind::Child, hold),
            item: Some(item("c1", hold)),
            working: deliver.clone(),
            declared: held_waiting.clone(),
            completed: held_stopped.clone(),
            interrupted: held_stopped.clone(),
        },
        Cell {
            name: "timer of the current stop",
            class: ArrivalClass::TimerExpired { stop: 4 },
            item: None,
            working: ArrivalRoute::Obsolete,
            declared: ArrivalRoute::TimesOut { stop: 4 },
            completed: ArrivalRoute::Obsolete,
            interrupted: ArrivalRoute::Obsolete,
        },
        Cell {
            name: "timer of an earlier stop",
            class: ArrivalClass::TimerExpired { stop: 3 },
            item: None,
            working: ArrivalRoute::Obsolete,
            declared: ArrivalRoute::Obsolete,
            completed: ArrivalRoute::Obsolete,
            interrupted: ArrivalRoute::Obsolete,
        },
        Cell {
            name: "scheduled resume",
            class: ArrivalClass::ScheduledResume {
                item: Some(source(ItemKind::Resume, wake)),
            },
            item: Some(item("r1", wake)),
            working: deliver.clone(),
            declared: deliver.clone(),
            completed: deliver.clone(),
            interrupted: deliver.clone(),
        },
        Cell {
            name: "continuation",
            class: ArrivalClass::Continuation,
            item: None,
            working: deliver.clone(),
            declared: deliver.clone(),
            completed: deliver.clone(),
            interrupted: deliver.clone(),
        },
        Cell {
            name: "system notice",
            class: ArrivalClass::SystemNotice,
            item: None,
            working: deliver.clone(),
            declared: held_waiting.clone(),
            completed: deliver.clone(),
            interrupted: held_stopped.clone(),
        },
    ];
    for cell in cells {
        let decide_in =
            |state: &SessionState| decide(state, &cell.class, cell.item.as_ref(), &finished);
        assert_eq!(
            decide_in(&SessionState::Working),
            cell.working,
            "{}: working",
            cell.name
        );
        assert_eq!(
            decide_in(&pending_wait),
            cell.declared,
            "{}: pending wait",
            cell.name
        );
        assert_eq!(
            decide_in(&effective_wait),
            cell.declared,
            "{}: effective wait",
            cell.name
        );
        assert_eq!(
            decide_in(&SessionState::Completed),
            cell.completed,
            "{}: completed",
            cell.name
        );
        assert_eq!(
            decide_in(&SessionState::Interrupted),
            cell.interrupted,
            "{}: interrupted",
            cell.name
        );
        // A completion being declared holds what a wait would hold, while
        // you are "stopped"; it awaits nothing and has no timer.
        let completing = decide_in(&pending_completion);
        let expected = match &cell.declared {
            ArrivalRoute::Held { .. } | ArrivalRoute::EndsStop { .. } => held_stopped.clone(),
            ArrivalRoute::TimesOut { .. } => ArrivalRoute::Obsolete,
            other => other.clone(),
        };
        assert_eq!(
            completing, expected,
            "{}: completion being declared",
            cell.name
        );
    }
}

#[test]
fn a_nested_condition_needs_all_of_its_branch() {
    let state = SessionState::Declared(stop_record(
        1,
        wait_on("all(t1, t2)"),
        DeclarationState::Effective,
    ));
    let class = finished_item(ItemKind::Task, FinishPolicy::Wake);
    let nothing = |_: ItemAlias| false;
    assert_eq!(
        decide(
            &state,
            &class,
            Some(&item("t1", FinishPolicy::Wake)),
            &nothing
        ),
        ArrivalRoute::Held {
            held_while: waiting("all(t1, t2)")
        }
    );
    let t1_done = |candidate: ItemAlias| candidate == alias("t1");
    assert_eq!(
        decide(
            &state,
            &class,
            Some(&item("t2", FinishPolicy::Wake)),
            &t1_done
        ),
        ArrivalRoute::EndsStop { stop: 1 }
    );
}

#[test]
fn leads_name_what_was_held_and_how_a_stop_ended() {
    let record = |route, class, item| ArrivalRecord {
        input: "input".into(),
        class,
        arrived_at: at(9),
        route,
        item,
        released_at: Some(at(10)),
    };
    let held = ArrivalRoute::Held {
        held_while: waiting("q1"),
    };
    assert_eq!(
        lead(&record(
            held.clone(),
            finished_item(ItemKind::Task, FinishPolicy::Wake),
            Some(item("t2", FinishPolicy::Wake))
        )),
        Some(Lead::HeldItem {
            alias: alias("t2"),
            label: "label-t2".into(),
            event: ItemEvent::Finished,
            at: at(9),
            held_while: waiting("q1"),
        })
    );
    assert_eq!(
        lead(&record(held.clone(), human(HumanDisposition::Queue), None)),
        Some(Lead::HeldMessage {
            at: at(9),
            held_while: waiting("q1")
        })
    );
    assert_eq!(
        lead(&record(held, ArrivalClass::SystemNotice, None)),
        Some(Lead::HeldNotice {
            at: at(9),
            held_while: waiting("q1")
        })
    );
    assert_eq!(
        lead(&record(
            ArrivalRoute::EndsStop { stop: 1 },
            answer(FinishPolicy::Wake),
            Some(item("q1", FinishPolicy::Wake))
        )),
        Some(Lead::StopEnded {
            alias: alias("q1"),
            event: ItemEvent::WasAnswered
        })
    );
    assert_eq!(
        lead(&record(
            ArrivalRoute::KeepsStop { stop: 1 },
            human(HumanDisposition::SendKeepStop),
            None
        )),
        Some(Lead::KeptStop)
    );
    assert_eq!(
        lead(&record(
            ArrivalRoute::RevokesStop { stop: 1 },
            human(HumanDisposition::RevokeAndSend),
            None
        )),
        Some(Lead::RevokedStop)
    );
    assert_eq!(
        lead(&record(
            ArrivalRoute::Deliver,
            ArrivalClass::Continuation,
            None
        )),
        None
    );
    assert_eq!(ItemEvent::of(ItemKind::Resume).text(), "fired");
    assert!(!deliverable(&ArrivalRecord {
        released_at: None,
        ..record(
            ArrivalRoute::Held {
                held_while: HeldWhile::Stopped
            },
            ArrivalClass::SystemNotice,
            None
        )
    }));
    assert!(!deliverable(&record(
        ArrivalRoute::Obsolete,
        ArrivalClass::TimerExpired { stop: 1 },
        None
    )));
}

// Store-backed: the decision, its effect on the stop and replay converge in
// one transaction.

fn store() -> (tempfile::TempDir, SessionWorkStore) {
    let temp = tempfile::tempdir().unwrap();
    let store = SessionWorkStore::at(temp.path().join("session-work"));
    store
        .activate(
            &SessionWorkActivation {
                session: "s".into(),
                role: SessionWorkRole::Primary,
                origin: ActivationOrigin::Fresh {},
                activated_at: at(0),
                module_types: Vec::new(),
            },
            None,
        )
        .unwrap();
    (temp, store)
}

fn background(task: &str, wake: bool) -> ArrivalClass {
    ArrivalClass::ItemFinished {
        item: ItemSource {
            request: format!("background:{task}"),
            kind: ItemKind::Task,
            label: format!("label-{task}"),
            policy: if wake {
                FinishPolicy::Wake
            } else {
                FinishPolicy::Hold
            },
        },
    }
}

fn running(task: &str, wake: bool) -> BackgroundWork {
    BackgroundWork {
        task_id: task.into(),
        kind: ItemKind::Task,
        label: format!("label-{task}"),
        wake,
        running: true,
    }
}

/// Declare a wait while `work` runs, and end its turn so it takes effect.
fn effective_wait(
    store: &SessionWorkStore,
    request: &str,
    condition: &str,
    work: &[BackgroundWork],
) {
    let items = stop::observe_items(store, "s", work, at(1)).unwrap();
    let outcome = stop::declare(
        store,
        "s",
        SessionWorkRole::Primary,
        request,
        &DeclareInput {
            action: "wait".into(),
            condition: Some(condition.into()),
            summary: Some("synthetic".into()),
            ..DeclareInput::default()
        },
        &items,
        &WaitTimeoutDefaults::default(),
        at(2),
    )
    .unwrap();
    assert!(
        matches!(outcome, DeclareOutcome::Declared { .. }),
        "{outcome:?}"
    );
    let current = store.current_stop("s").unwrap().unwrap();
    store
        .settle_stop(
            "s",
            current.number,
            DeclarationState::Effective,
            Some("final"),
            None,
            at(3),
        )
        .unwrap();
    stop::record_outcome(
        store,
        "s",
        &format!("end:{request}"),
        &TurnOutcome::Waiting {
            stop: current.number,
        },
        at(3),
    )
    .unwrap();
}

#[test]
fn an_awaited_arrival_withdraws_the_stop_once_and_replays_its_record() {
    let (_temp, store) = store();
    effective_wait(
        &store,
        "w1",
        "t1",
        &[running("run-a", true), running("run-b", true)],
    );
    let other = store
        .arrive(
            "s",
            "in-b",
            &background("run-b", true),
            Activity::Idle,
            at(4),
        )
        .unwrap();
    assert_eq!(
        other.route,
        ArrivalRoute::Held {
            held_while: waiting("t1")
        }
    );
    assert_eq!(other.item.as_ref().unwrap().alias, alias("t2"));
    assert!(store.current_stop("s").unwrap().is_some());

    let awaited = store
        .arrive(
            "s",
            "in-a",
            &background("run-a", true),
            Activity::Idle,
            at(5),
        )
        .unwrap();
    assert_eq!(awaited.route, ArrivalRoute::EndsStop { stop: 1 });
    assert_eq!(awaited.item.as_ref().unwrap().finished_at, Some(at(5)));
    assert!(store.current_stop("s").unwrap().is_none());
    assert_eq!(
        store.stops("s").unwrap()[0].state,
        DeclarationState::Withdrawn {
            cause: WithdrawCause::AwaitedArrival
        }
    );
    // Replay converges on the original record and changes nothing.
    assert_eq!(
        store
            .arrive(
                "s",
                "in-a",
                &background("run-a", true),
                Activity::Idle,
                at(9)
            )
            .unwrap(),
        awaited
    );

    // The waking turn releases the held arrival, oldest first.
    let released = store.release_held("s", Activity::Working, at(6)).unwrap();
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].input, "in-b");
    assert_eq!(released[0].released_at, Some(at(6)));
    assert!(deliverable(&released[0]));
    assert!(
        store
            .release_held("s", Activity::Working, at(7))
            .unwrap()
            .is_empty()
    );
    let order: Vec<String> = store
        .arrivals("s")
        .unwrap()
        .into_iter()
        .map(|record| record.input)
        .collect();
    assert_eq!(order, vec!["in-b", "in-a"]);
}

#[test]
fn dispositions_keep_or_revoke_the_stop_and_queue_holds_until_woken() {
    let (_temp, store) = store();
    effective_wait(&store, "w1", "t1", &[running("run-a", true)]);
    let queued = store
        .arrive(
            "s",
            "m1",
            &human(HumanDisposition::Queue),
            Activity::Idle,
            at(4),
        )
        .unwrap();
    assert!(queued.is_held());
    // A kept stop's turn releases nothing: the stop still governs.
    assert!(
        store
            .release_held("s", Activity::Working, at(5))
            .unwrap()
            .is_empty()
    );
    let kept = store
        .arrive(
            "s",
            "m2",
            &human(HumanDisposition::SendKeepStop),
            Activity::Idle,
            at(5),
        )
        .unwrap();
    assert_eq!(kept.route, ArrivalRoute::KeepsStop { stop: 1 });
    assert!(store.current_stop("s").unwrap().is_some());

    // Mirza sends the queued message now, keeping the stop.
    let sent = store
        .redispose("s", "m1", HumanDisposition::SendKeepStop, at(6))
        .unwrap();
    assert_eq!(sent.route, ArrivalRoute::KeepsStop { stop: 1 });
    assert!(deliverable(&sent));

    let revoked = store
        .arrive(
            "s",
            "m3",
            &human(HumanDisposition::RevokeAndSend),
            Activity::Idle,
            at(7),
        )
        .unwrap();
    assert_eq!(revoked.route, ArrivalRoute::RevokesStop { stop: 1 });
    assert_eq!(
        store.stops("s").unwrap()[0].state,
        DeclarationState::Withdrawn {
            cause: WithdrawCause::Revoked
        }
    );
    // With no stop, queue is ordinary delivery.
    let plain = store
        .arrive(
            "s",
            "m4",
            &human(HumanDisposition::Queue),
            Activity::Idle,
            at(8),
        )
        .unwrap();
    assert_eq!(plain.route, ArrivalRoute::Deliver);
    assert!(
        store
            .redispose("s", "in-none", HumanDisposition::SendKeepStop, at(9))
            .is_err()
    );
}

#[test]
fn a_held_message_redisposed_to_revoke_withdraws_the_stop() {
    let (_temp, store) = store();
    effective_wait(&store, "w1", "t1", &[running("run-a", true)]);
    store
        .arrive(
            "s",
            "m1",
            &human(HumanDisposition::Queue),
            Activity::Idle,
            at(4),
        )
        .unwrap();
    let record = store
        .redispose("s", "m1", HumanDisposition::RevokeAndSend, at(5))
        .unwrap();
    assert_eq!(record.route, ArrivalRoute::RevokesStop { stop: 1 });
    assert!(store.current_stop("s").unwrap().is_none());
    // Not a message: refused.
    store
        .arrive(
            "s",
            "n1",
            &ArrivalClass::SystemNotice,
            Activity::Idle,
            at(6),
        )
        .unwrap();
    assert!(
        store
            .redispose("s", "n1", HumanDisposition::SendKeepStop, at(7))
            .is_err()
    );
}

#[test]
fn the_state_row_follows_the_last_turn() {
    let (_temp, store) = store();
    // No turn yet: as after completion.
    assert_eq!(
        store.session_state("s", Activity::Idle).unwrap(),
        SessionState::Completed
    );
    // A turn that never finished (a crash) is interrupted.
    store.begin_turn("s", at(1)).unwrap();
    assert_eq!(
        store.session_state("s", Activity::Idle).unwrap(),
        SessionState::Interrupted
    );
    assert_eq!(
        store.session_state("s", Activity::Working).unwrap(),
        SessionState::Working
    );
    for (outcome, expected) in [
        (
            TurnOutcome::Completed { declared: false },
            SessionState::Completed,
        ),
        (TurnOutcome::InterruptedByUser, SessionState::Interrupted),
        (
            TurnOutcome::Failed {
                class: FailureClass::Provider,
            },
            SessionState::Interrupted,
        ),
        (TurnOutcome::ProviderRefusal, SessionState::Interrupted),
        (TurnOutcome::Truncated, SessionState::Interrupted),
        (
            TurnOutcome::StoppedWithoutDeclaration {
                cause: ExhaustedCounter::Missing,
            },
            SessionState::Interrupted,
        ),
        (
            TurnOutcome::Completed { declared: true },
            SessionState::Completed,
        ),
    ] {
        store.begin_turn("s", at(2)).unwrap();
        stop::record_outcome(
            &store,
            "s",
            &uuid::Uuid::new_v4().to_string(),
            &outcome,
            at(2),
        )
        .unwrap();
        assert_eq!(
            store.session_state("s", Activity::Idle).unwrap(),
            expected,
            "{outcome:?}"
        );
    }
    // A reload leaves the turn open until its continuation runs.
    store.begin_turn("s", at(3)).unwrap();
    stop::record_outcome(
        &store,
        "s",
        "reload",
        &TurnOutcome::InterruptedByReload,
        at(3),
    )
    .unwrap();
    assert_eq!(
        store.session_state("s", Activity::Idle).unwrap(),
        SessionState::Interrupted
    );
}

#[test]
fn a_surviving_declaration_keeps_the_waiting_row_and_stop_revocation_interrupts() {
    let (_temp, store) = store();
    let work = [running("run-a", true), running("run-b", true)];
    // A wait declared in a turn that then fails survives the failure.
    let items = stop::observe_items(&store, "s", &work, at(1)).unwrap();
    stop::declare(
        &store,
        "s",
        SessionWorkRole::Primary,
        "w1",
        &DeclareInput {
            action: "wait".into(),
            condition: Some("t1".into()),
            summary: Some("synthetic".into()),
            ..DeclareInput::default()
        },
        &items,
        &WaitTimeoutDefaults::default(),
        at(2),
    )
    .unwrap();
    stop::survive_failure(&store, "s", &items, at(3)).unwrap();
    stop::record_outcome(
        &store,
        "s",
        "fail",
        &TurnOutcome::Failed {
            class: FailureClass::Provider,
        },
        at(3),
    )
    .unwrap();
    assert!(matches!(
        store.session_state("s", Activity::Idle).unwrap(),
        SessionState::Declared(_)
    ));
    let other = store
        .arrive(
            "s",
            "in-b",
            &background("run-b", true),
            Activity::Idle,
            at(4),
        )
        .unwrap();
    assert!(other.is_held());
    let awaited = store
        .arrive(
            "s",
            "in-a",
            &background("run-a", true),
            Activity::Idle,
            at(5),
        )
        .unwrap();
    assert_eq!(awaited.route, ArrivalRoute::EndsStop { stop: 1 });

    // Mirza's Stop during a declaring turn revokes the declaration: the
    // interrupted row holds automatic arrivals and lets his messages wake.
    let (_temp2, store) = self::store();
    let work = [running("run-c", true)];
    let items = stop::observe_items(&store, "s", &work, at(1)).unwrap();
    stop::declare(
        &store,
        "s",
        SessionWorkRole::Primary,
        "w2",
        &DeclareInput {
            action: "wait".into(),
            condition: Some("t1".into()),
            summary: Some("synthetic".into()),
            ..DeclareInput::default()
        },
        &items,
        &WaitTimeoutDefaults::default(),
        at(2),
    )
    .unwrap();
    stop::revoke_on_stop(&store, "s", at(3)).unwrap();
    stop::record_outcome(&store, "s", "stop", &TurnOutcome::InterruptedByUser, at(3)).unwrap();
    assert_eq!(
        store.session_state("s", Activity::Idle).unwrap(),
        SessionState::Interrupted
    );
    let finished = store
        .arrive(
            "s",
            "in-c",
            &background("run-c", true),
            Activity::Idle,
            at(4),
        )
        .unwrap();
    assert_eq!(
        finished.route,
        ArrivalRoute::Held {
            held_while: HeldWhile::Stopped
        }
    );
    let message = store
        .arrive(
            "s",
            "m1",
            &human(HumanDisposition::Queue),
            Activity::Idle,
            at(5),
        )
        .unwrap();
    assert_eq!(message.route, ArrivalRoute::Deliver);
}

#[test]
fn a_wait_cannot_commit_on_an_item_that_arrived_after_validation() {
    let (_temp, store) = store();
    let items = stop::observe_items(&store, "s", &[running("run-a", true)], at(1)).unwrap();
    // The arrival lands between validation (which saw t1 running) and the
    // declaration's commit.
    store
        .arrive(
            "s",
            "in-a",
            &background("run-a", true),
            Activity::Working,
            at(2),
        )
        .unwrap();
    let outcome = stop::declare(
        &store,
        "s",
        SessionWorkRole::Primary,
        "w1",
        &DeclareInput {
            action: "wait".into(),
            condition: Some("t1".into()),
            summary: Some("synthetic".into()),
            ..DeclareInput::default()
        },
        &items,
        &WaitTimeoutDefaults::default(),
        at(3),
    )
    .unwrap();
    match outcome {
        DeclareOutcome::Refused(stop::Refusal::Invalid(reason)) => {
            assert!(reason.starts_with("t1 "), "{reason}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(store.current_stop("s").unwrap().is_none());
}

#[test]
fn an_unseen_item_is_allocated_with_its_source_and_a_seen_one_keeps_its_policy() {
    let (_temp, store) = store();
    stop::observe_items(&store, "s", &[running("run-a", true)], at(1)).unwrap();
    // Allocated while running with wake; the arrival says hold.
    let seen = store
        .arrive(
            "s",
            "in-a",
            &background("run-a", false),
            Activity::Idle,
            at(2),
        )
        .unwrap();
    let item = seen.item.unwrap();
    assert_eq!((item.alias, item.policy), (alias("t1"), FinishPolicy::Wake));
    // Never seen running: allocated from the arrival.
    let unseen = store
        .arrive(
            "s",
            "in-b",
            &background("run-b", false),
            Activity::Idle,
            at(3),
        )
        .unwrap();
    let item = unseen.item.unwrap();
    assert_eq!((item.alias, item.policy), (alias("t2"), FinishPolicy::Hold));
    assert_eq!(item.finished_at, Some(at(3)));
    assert_eq!(
        unseen.route,
        ArrivalRoute::Held {
            held_while: HeldWhile::Stopped
        }
    );
    let _ = WaitCondition::Item(alias("t1"));
}
