use super::*;
use jcode_session_work_types::{ActivationOrigin, RevisionSource, SessionWorkActivation};

fn at(seconds: i64) -> DateTime<Utc> {
    chrono::DateTime::from_timestamp(1_800_000_000 + seconds, 0).unwrap()
}

fn store_with(session: &str, role: SessionWorkRole) -> (tempfile::TempDir, SessionWorkStore) {
    let temp = tempfile::tempdir().unwrap();
    let store = SessionWorkStore::at(temp.path().join("session-work"));
    store
        .activate(
            &SessionWorkActivation {
                session: session.into(),
                role,
                origin: ActivationOrigin::Fresh {},
                activated_at: at(0),
                module_types: Vec::new(),
            },
            None,
        )
        .unwrap();
    (temp, store)
}

fn workflow(store: &SessionWorkStore, request: &str, text: &str) {
    store
        .commit_workflow("s", request, text, &RevisionSource::Agent {}, at(1))
        .unwrap();
}

fn input(action: &str) -> DeclareInput {
    DeclareInput {
        action: action.into(),
        ..DeclareInput::default()
    }
}

fn wait(condition: &str) -> DeclareInput {
    DeclareInput {
        condition: Some(condition.into()),
        summary: Some("synthetic wait".into()),
        ..input("wait")
    }
}

fn task(id: &str, kind: ItemKind, running: bool, wake: bool) -> BackgroundWork {
    BackgroundWork {
        task_id: id.into(),
        kind,
        label: format!("label-{id}"),
        wake,
        running,
    }
}

fn declare_ok(
    store: &SessionWorkStore,
    request: &str,
    input: &DeclareInput,
    items: &[ObservedItem],
) -> DeclareOutcome {
    declare(
        store,
        "s",
        SessionWorkRole::Primary,
        request,
        input,
        items,
        &WaitTimeoutDefaults::default(),
        at(2),
    )
    .unwrap()
}

fn end(final_text: &str, occurrence: &str) -> NaturalEnd<'static> {
    NaturalEnd {
        final_text: Box::leak(final_text.to_string().into_boxed_str()),
        occurrence: Box::leak(occurrence.to_string().into_boxed_str()),
        nonvoluntary: None,
        empty: false,
    }
}

fn natural(store: &SessionWorkStore, end: &NaturalEnd<'_>, items: &[ObservedItem]) -> EndDecision {
    natural_end(store, "s", end, items, &CorrectionLimits::default(), at(3)).unwrap()
}

#[test]
fn background_work_gets_stable_aliases_and_policies_when_first_seen() {
    let (_temp, store) = store_with("s", SessionWorkRole::Primary);
    let work = [
        task("bg-a", ItemKind::Task, true, true),
        task("bg-b", ItemKind::Child, true, false),
        task("bg-c", ItemKind::Task, false, true),
    ];
    let items = observe_items(&store, "s", &work, at(1)).unwrap();
    let aliases: Vec<String> = items.iter().map(|i| i.item.alias.to_string()).collect();
    assert_eq!(
        aliases,
        ["t1", "c1"],
        "finished work never seen gets no alias"
    );
    assert_eq!(items[1].item.policy, FinishPolicy::Hold);
    // Later observations keep the alias and policy; finishing shows as not running.
    let later = [
        task("bg-a", ItemKind::Task, false, false),
        task("bg-b", ItemKind::Child, true, true),
    ];
    let items = observe_items(&store, "s", &later, at(2)).unwrap();
    assert!(!items[0].running && items[1].running);
    assert_eq!(items[0].item.policy, FinishPolicy::Wake);
    assert_eq!(items[1].item.policy, FinishPolicy::Hold);
}

#[test]
fn wait_declarations_validate_aliases_conditions_and_summaries() {
    let (_temp, store) = store_with("s", SessionWorkRole::Primary);
    let items = observe_items(
        &store,
        "s",
        &[
            task("a", ItemKind::Task, true, true),
            task("b", ItemKind::Task, true, true),
        ],
        at(1),
    )
    .unwrap();
    let refused = |outcome: DeclareOutcome| match outcome {
        DeclareOutcome::Refused(Refusal::Invalid(reason)) => reason,
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert_eq!(
        refused(declare_ok(&store, "r1", &wait("t9"), &items)),
        "t9 is not one of this session's items; running items are t1, t2."
    );
    assert!(
        refused(declare_ok(&store, "r2", &wait("any(t1"), &items))
            .starts_with("condition syntax at position 7")
    );
    let mut no_summary = wait("t1");
    no_summary.summary = None;
    assert!(refused(declare_ok(&store, "r3", &no_summary, &items)).contains("summary"));
    let mut two_lines = wait("t1");
    two_lines.summary = Some("one\ntwo".into());
    assert!(refused(declare_ok(&store, "r4", &two_lines, &items)).contains("one line"));
    let mut bad_timeout = wait("t1");
    bad_timeout.timeout = Some("soon".into());
    assert!(refused(declare_ok(&store, "r5", &bad_timeout, &items)).contains("not a duration"));
    assert!(
        store.stops("s").unwrap().is_empty(),
        "refusals store nothing"
    );

    // A valid wait lists running items outside its condition and resolves
    // the default limit through the condition.
    let DeclareOutcome::Declared {
        record,
        also_running,
    } = declare_ok(&store, "r6", &wait("any(t1)"), &items)
    else {
        panic!("declared");
    };
    assert_eq!(record.state, DeclarationState::Pending);
    assert!(matches!(
        record.declaration,
        Declaration::Wait {
            timeout: WaitTimeout::After(1800),
            timeout_source: TimeoutSource::Default,
            ..
        }
    ));
    assert_eq!(item_list_and(&also_running), "t2 (label-b, wake)");

    // A replay converges; a new declaration replaces the current one.
    let DeclareOutcome::Declared { record: replay, .. } =
        declare_ok(&store, "r6", &wait("any(t1)"), &items)
    else {
        panic!("replayed");
    };
    assert_eq!(replay.number, record.number);
    let mut explicit = wait("all(t1, t2)");
    explicit.timeout = Some("none".into());
    declare_ok(&store, "r7", &explicit, &items);
    let stops = store.stops("s").unwrap();
    assert_eq!(stops.len(), 2);
    assert_eq!(
        stops[0].state,
        DeclarationState::Withdrawn {
            cause: jcode_session_work_types::WithdrawCause::Replaced
        }
    );
    assert_eq!(store.current_stop("s").unwrap().unwrap().number, 2);

    // A condition already met by finished work is refused.
    let finished = observe_items(
        &store,
        "s",
        &[
            task("a", ItemKind::Task, false, true),
            task("b", ItemKind::Task, true, true),
        ],
        at(4),
    )
    .unwrap();
    assert_eq!(
        refused(declare_ok(&store, "r8", &wait("any(t1, t2)"), &finished)),
        "t1 already finished, and its result is in your conversation."
    );
    assert!(matches!(
        declare_ok(&store, "r9", &wait("all(t1, t2)"), &finished),
        DeclareOutcome::Declared { .. }
    ));
}

#[test]
fn completion_requires_a_finished_workflow_and_no_unnamed_running_work() {
    let (_temp, store) = store_with("s", SessionWorkRole::Primary);
    // No workflow and nothing running: completion is allowed.
    assert!(matches!(
        declare_ok(&store, "c0", &input("complete"), &[]),
        DeclareOutcome::Declared { .. }
    ));
    workflow(&store, "w1", "- [x] a: A\n- [>] b: B\n- [?] c: C\n");
    assert_eq!(
        match declare_ok(&store, "c1", &input("complete"), &[]) {
            DeclareOutcome::Refused(Refusal::Unfinished(modules)) => module_list(&modules),
            other => panic!("{other:?}"),
        },
        "b (active), c (undetermined)"
    );
    workflow(
        &store,
        "w2",
        "- [x] a: A\n- [x] b: B\n- [-] c: C\n  > skipped: synthetic\n",
    );
    let items = observe_items(
        &store,
        "s",
        &[
            task("x", ItemKind::Task, true, true),
            task("y", ItemKind::Child, true, false),
        ],
        at(2),
    )
    .unwrap();
    match declare_ok(&store, "c2", &input("complete"), &items) {
        DeclareOutcome::Refused(Refusal::StillRunning(running)) => {
            assert_eq!(
                item_list(&running),
                "t1 (label-x, wake), c1 (label-y, hold)"
            )
        }
        other => panic!("{other:?}"),
    }
    let mut leave = input("complete");
    leave.leave_running = vec!["t1".into()];
    assert!(matches!(
        declare_ok(&store, "c3", &leave, &items),
        DeclareOutcome::Refused(Refusal::StillRunning(_))
    ));
    leave.leave_running = vec!["t1".into(), "c1".into()];
    assert!(matches!(
        declare_ok(&store, "c4", &leave, &items),
        DeclareOutcome::Declared { .. }
    ));
    leave.leave_running = vec!["t7".into()];
    assert!(matches!(
        declare_ok(&store, "c5", &leave, &items),
        DeclareOutcome::Refused(Refusal::Invalid(reason)) if reason.contains("not running")
    ));
}

#[test]
fn children_declare_only_completion() {
    let (_temp, store) = store_with("s", SessionWorkRole::Child);
    for action in ["wait", "withdraw"] {
        let mut request = wait("t1");
        request.action = action.into();
        let outcome = declare(
            &store,
            "s",
            SessionWorkRole::Child,
            action,
            &request,
            &[],
            &WaitTimeoutDefaults::default(),
            at(1),
        )
        .unwrap();
        assert!(matches!(
            outcome,
            DeclareOutcome::Refused(Refusal::Invalid(_))
        ));
    }
    let outcome = declare(
        &store,
        "s",
        SessionWorkRole::Child,
        "complete",
        &input("complete"),
        &[],
        &WaitTimeoutDefaults::default(),
        at(1),
    )
    .unwrap();
    assert!(matches!(outcome, DeclareOutcome::Declared { .. }));
}

#[test]
fn withdraw_clears_the_declaration_and_returns_the_warned_call() {
    let (_temp, store) = store_with("s", SessionWorkRole::Primary);
    assert!(matches!(
        declare_ok(&store, "w0", &input("withdraw"), &[]),
        DeclareOutcome::Refused(Refusal::Invalid(_))
    ));
    declare_ok(&store, "d1", &input("complete"), &[]);
    let limits = CorrectionLimits::default();
    assert_eq!(
        gate(&store, "s", "read", "call-1", &limits).unwrap(),
        Gate::RunWarned
    );
    let DeclareOutcome::Withdrawn(record) = declare_ok(&store, "w1", &input("withdraw"), &[])
    else {
        panic!("withdrawn");
    };
    assert_eq!(record.number, 1);
    // A replayed withdraw converges on the same record.
    assert!(matches!(
        declare_ok(&store, "w1", &input("withdraw"), &[]),
        DeclareOutcome::Withdrawn(again) if again.number == 1
    ));
    assert_eq!(
        gate(&store, "s", "read", "call-2", &limits).unwrap(),
        Gate::Run
    );
    declare_ok(&store, "d2", &wait_on_nothing_running(&store), &[]);
}

fn wait_on_nothing_running(_store: &SessionWorkStore) -> DeclareInput {
    // There are no items in this fixture, so a completion stands in for a
    // second declaration after a withdraw.
    input("complete")
}

#[test]
fn the_gate_warns_once_refuses_later_calls_and_batch_and_ends_the_turn_after_its_limit() {
    let (_temp, store) = store_with("s", SessionWorkRole::Primary);
    let limits = CorrectionLimits::default();
    assert_eq!(
        gate(&store, "s", "bash", "c0", &limits).unwrap(),
        Gate::Run,
        "no declaration"
    );
    declare_ok(&store, "d", &input("complete"), &[]);
    assert_eq!(
        gate(&store, "s", "declare_stop", "c1", &limits).unwrap(),
        Gate::Run
    );
    assert_eq!(
        gate(&store, "s", "bash", "c2", &limits).unwrap(),
        Gate::RunWarned
    );
    // The decision for a call is stable when replayed.
    assert_eq!(
        gate(&store, "s", "bash", "c2", &limits).unwrap(),
        Gate::RunWarned
    );
    let refuse = |batch, end_turn| Gate::Refuse {
        wait: false,
        batch,
        end_turn,
    };
    assert_eq!(
        gate(&store, "s", "read", "c3", &limits).unwrap(),
        refuse(false, false)
    );
    assert_eq!(
        gate(&store, "s", "batch", "c4", &limits).unwrap(),
        refuse(true, false)
    );
    assert_eq!(
        gate(&store, "s", "read", "c5", &limits).unwrap(),
        refuse(false, true)
    );
    assert_eq!(
        gate(&store, "s", "read", "c3", &limits).unwrap(),
        refuse(false, false)
    );
}

#[test]
fn batch_is_refused_even_before_the_warned_call() {
    let (_temp, store) = store_with("s", SessionWorkRole::Primary);
    declare_ok(&store, "d", &input("complete"), &[]);
    let limits = CorrectionLimits::default();
    assert!(matches!(
        gate(&store, "s", "batch", "b1", &limits).unwrap(),
        Gate::Refuse { batch: true, .. }
    ));
    assert_eq!(
        gate(&store, "s", "read", "r1", &limits).unwrap(),
        Gate::RunWarned
    );
}

#[test]
fn natural_ends_settle_declarations_or_correct_until_the_counter_runs_out() {
    let (_temp, store) = store_with("s", SessionWorkRole::Primary);
    // No workflow: implicit completion.
    assert_eq!(
        natural(&store, &end("hi", "m1"), &[]),
        EndDecision::End(TurnOutcome::Completed { declared: false })
    );
    workflow(&store, "w1", "- [>] a: A\n");
    // Unfinished workflow without a declaration: two corrections, then the
    // turn stops without one.
    for occurrence in ["m2", "m3"] {
        assert_eq!(
            natural(&store, &end("stop", occurrence), &[]),
            EndDecision::Correct(Correction::Missing {
                unfinished: vec![("a".into(), ModuleStatus::Active)]
            })
        );
    }
    // The same occurrence counts once.
    assert!(matches!(
        natural(&store, &end("stop", "m3"), &[]),
        EndDecision::Correct(_)
    ));
    assert_eq!(
        natural(&store, &end("stop", "m4"), &[]),
        EndDecision::End(TurnOutcome::StoppedWithoutDeclaration {
            cause: ExhaustedCounter::Missing
        })
    );
    record_outcome(
        &store,
        "s",
        "o1",
        &TurnOutcome::StoppedWithoutDeclaration {
            cause: ExhaustedCounter::Missing,
        },
        at(4),
    )
    .unwrap();
    // A new turn has fresh counters; a pending completion becomes effective
    // with the final message and covers the workflow's head.
    workflow(&store, "w2", "- [x] a: A\n");
    declare_ok(&store, "d1", &input("complete"), &[]);
    assert_eq!(
        natural(&store, &end("all done", "m5"), &[]),
        EndDecision::End(TurnOutcome::Completed { declared: true })
    );
    let completion = store.latest_completion("s").unwrap().unwrap();
    assert_eq!(completion.final_text.as_deref(), Some("all done"));
    assert_eq!(completion.workflow_revision, Some(2));
    record_outcome(
        &store,
        "s",
        "o2",
        &TurnOutcome::Completed { declared: true },
        at(5),
    )
    .unwrap();
    // A follow-up without workflow changes completes implicitly.
    assert_eq!(
        natural(&store, &end("answer", "m6"), &[]),
        EndDecision::End(TurnOutcome::Completed { declared: false })
    );
    record_outcome(
        &store,
        "s",
        "o3",
        &TurnOutcome::Completed { declared: false },
        at(6),
    )
    .unwrap();
    // Editing the workflow afterwards needs a new declaration.
    workflow(&store, "w3", "- [x] a: A\n- [ ] b: B\n");
    assert!(matches!(
        natural(&store, &end("more", "m7"), &[]),
        EndDecision::Correct(Correction::Missing { .. })
    ));
}

#[test]
fn a_declaration_that_no_longer_holds_is_invalidated_and_corrected() {
    let (_temp, store) = store_with("s", SessionWorkRole::Primary);
    workflow(&store, "w1", "- [x] a: A\n");
    declare_ok(&store, "d1", &input("complete"), &[]);
    // The warned call reopened the workflow before the final message.
    workflow(&store, "w2", "- [x] a: A\n- [>] b: B\n");
    let decision = natural(&store, &end("done", "m1"), &[]);
    assert_eq!(
        decision,
        EndDecision::Correct(Correction::Invalid(Refusal::Unfinished(vec![(
            "b".into(),
            ModuleStatus::Active
        )])))
    );
    assert!(matches!(
        store.stops("s").unwrap()[0].state,
        DeclarationState::Invalidated { .. }
    ));
    assert!(store.current_stop("s").unwrap().is_none());
    // The next stop without a declaration is a missing one.
    assert!(matches!(
        natural(&store, &end("done", "m2"), &[]),
        EndDecision::Correct(Correction::Missing { .. })
    ));
}

#[test]
fn the_invalid_counter_runs_out_after_its_limit() {
    let (_temp, store) = store_with("s", SessionWorkRole::Primary);
    workflow(&store, "w0", "- [x] a: A\n");
    for round in 1..=4 {
        workflow(&store, &format!("w{round}a"), "- [x] a: A\n");
        declare_ok(&store, &format!("d{round}"), &input("complete"), &[]);
        workflow(&store, &format!("w{round}b"), "- [x] a: A\n- [ ] b: B\n");
        let decision = natural(&store, &end("x", &format!("m{round}")), &[]);
        if round <= 3 {
            assert!(
                matches!(decision, EndDecision::Correct(Correction::Invalid(_))),
                "{round}"
            );
        } else {
            assert_eq!(
                decision,
                EndDecision::End(TurnOutcome::StoppedWithoutDeclaration {
                    cause: ExhaustedCounter::Invalid
                })
            );
        }
    }
}

#[test]
fn nonvoluntary_ends_never_correct_and_leave_declarations_alone() {
    let (_temp, store) = store_with("s", SessionWorkRole::Primary);
    workflow(&store, "w1", "- [>] a: A\n");
    for outcome in [TurnOutcome::ProviderRefusal, TurnOutcome::Truncated] {
        let mut nonvoluntary = end("", "m");
        nonvoluntary.nonvoluntary = Some(outcome.clone());
        assert_eq!(
            natural(&store, &nonvoluntary, &[]),
            EndDecision::End(outcome)
        );
    }
    let mut empty = end("", "m-empty");
    empty.empty = true;
    assert_eq!(
        natural(&store, &empty, &[]),
        EndDecision::End(TurnOutcome::Truncated)
    );
    assert_eq!(
        store.open_turn("s").unwrap().unwrap().1.corrections.len(),
        0
    );
    // A valid declaration survives a refusal and takes effect.
    let items =
        observe_items(&store, "s", &[task("x", ItemKind::Task, true, true)], at(1)).unwrap();
    declare_ok(&store, "d1", &wait("t1"), &items);
    let mut refusal = end("", "m-r");
    refusal.nonvoluntary = Some(TurnOutcome::ProviderRefusal);
    assert_eq!(
        natural(&store, &refusal, &items),
        EndDecision::End(TurnOutcome::ProviderRefusal)
    );
    let current = store.current_stop("s").unwrap().unwrap();
    assert_eq!(current.state, DeclarationState::Effective);
    assert_eq!(current.final_text, None);
    // An empty final message after a valid declaration settles it too.
    let (_temp, store) = store_with("s", SessionWorkRole::Primary);
    let items =
        observe_items(&store, "s", &[task("x", ItemKind::Task, true, true)], at(1)).unwrap();
    declare_ok(&store, "d1", &wait("t1"), &items);
    assert_eq!(
        natural(&store, &empty, &items),
        EndDecision::End(TurnOutcome::Waiting { stop: 1 })
    );
}

#[test]
fn failures_keep_a_valid_declaration_and_stop_revokes_one_made_in_the_turn() {
    let (_temp, store) = store_with("s", SessionWorkRole::Primary);
    let items =
        observe_items(&store, "s", &[task("x", ItemKind::Task, true, true)], at(1)).unwrap();
    declare_ok(&store, "d1", &wait("t1"), &items);
    survive_failure(&store, "s", &items, at(2)).unwrap();
    assert_eq!(
        store.current_stop("s").unwrap().unwrap().state,
        DeclarationState::Effective
    );
    // A wait already in effect stays when Mirza stops a later turn.
    revoke_on_stop(&store, "s", at(3)).unwrap();
    assert_eq!(
        store.current_stop("s").unwrap().unwrap().state,
        DeclarationState::Effective
    );
    // A declaration made in the stopped turn is revoked.
    declare_ok(&store, "d2", &wait("t1"), &items);
    revoke_on_stop(&store, "s", at(4)).unwrap();
    assert!(store.current_stop("s").unwrap().is_none());
    assert_eq!(
        store.stops("s").unwrap()[1].state,
        DeclarationState::Withdrawn {
            cause: jcode_session_work_types::WithdrawCause::Revoked
        }
    );
    // A declaration that no longer holds does not survive a failure.
    declare_ok(&store, "d3", &wait("t1"), &items);
    let finished = observe_items(
        &store,
        "s",
        &[task("x", ItemKind::Task, false, true)],
        at(5),
    )
    .unwrap();
    survive_failure(&store, "s", &finished, at(6)).unwrap();
    assert!(store.current_stop("s").unwrap().is_none());
    assert!(matches!(
        store.stops("s").unwrap()[2].state,
        DeclarationState::Invalidated { .. }
    ));
}

#[test]
fn refused_call_exhaustion_keeps_a_valid_declaration() {
    let (_temp, store) = store_with("s", SessionWorkRole::Primary);
    let items =
        observe_items(&store, "s", &[task("x", ItemKind::Task, true, true)], at(1)).unwrap();
    declare_ok(&store, "d1", &wait("t1"), &items);
    assert_eq!(
        end_after_refusals(&store, "s", &items, at(2)).unwrap(),
        TurnOutcome::Waiting { stop: 1 }
    );
    assert_eq!(
        store.current_stop("s").unwrap().unwrap().state,
        DeclarationState::Effective
    );
    let finished = observe_items(
        &store,
        "s",
        &[task("x", ItemKind::Task, false, true)],
        at(3),
    )
    .unwrap();
    assert_eq!(
        end_after_refusals(&store, "s", &finished, at(4)).unwrap(),
        TurnOutcome::StoppedWithoutDeclaration {
            cause: ExhaustedCounter::Refused
        }
    );
}

#[test]
fn a_reload_keeps_the_turn_open_and_its_counters() {
    let (_temp, store) = store_with("s", SessionWorkRole::Primary);
    workflow(&store, "w1", "- [>] a: A\n");
    assert!(matches!(
        natural(&store, &end("x", "m1"), &[]),
        EndDecision::Correct(_)
    ));
    record_outcome(
        &store,
        "s",
        "reload",
        &TurnOutcome::InterruptedByReload,
        at(4),
    )
    .unwrap();
    let (turn, counters) = store.begin_turn("s", at(5)).unwrap();
    assert_eq!(
        (turn, counters.corrections(ExhaustedCounter::Missing)),
        (1, 1)
    );
    record_outcome(&store, "s", "user", &TurnOutcome::InterruptedByUser, at(6)).unwrap();
    let (turn, counters) = store.begin_turn("s", at(7)).unwrap();
    assert_eq!(
        (turn, counters.corrections(ExhaustedCounter::Missing)),
        (2, 0)
    );
    assert_eq!(
        store.turn_outcomes("s").unwrap(),
        vec![
            (1, TurnOutcome::InterruptedByReload),
            (1, TurnOutcome::InterruptedByUser)
        ]
    );
}
