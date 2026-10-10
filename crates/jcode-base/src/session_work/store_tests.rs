use super::*;
use jcode_session_work_types::{ActivationOrigin, SessionWorkRole};

fn fresh() -> (tempfile::TempDir, SessionWorkStore) {
    let temp = tempfile::tempdir().unwrap();
    let store = SessionWorkStore::at(temp.path().join("session-work"));
    (temp, store)
}

fn activation(session: &str) -> SessionWorkActivation {
    SessionWorkActivation {
        session: session.into(),
        role: SessionWorkRole::Primary,
        origin: ActivationOrigin::Fresh {},
        activated_at: chrono::DateTime::parse_from_rfc3339("2026-10-10T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        module_types: Vec::new(),
    }
}

fn at(seconds: i64) -> DateTime<Utc> {
    chrono::DateTime::from_timestamp(1_800_000_000 + seconds, 0).unwrap()
}

fn agent() -> RevisionSource {
    RevisionSource::Agent {}
}

#[test]
fn activation_creates_an_owner_only_wal_store_with_the_current_schema() {
    let (_temp, store) = fresh();
    assert!(!store.exists().unwrap());
    store.activate(&activation("s1"), None).unwrap();
    let connection = Connection::open(store.path()).unwrap();
    let mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let dir = std::fs::metadata(store.path().parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!((file, dir), (0o600, 0o700));
    }
    assert_eq!(store.activation("s1").unwrap(), Some(activation("s1")));
    assert_eq!(store.activation("other").unwrap(), None);
}

#[test]
fn a_missing_store_fails_closed_instead_of_reading_as_empty() {
    let (_temp, store) = fresh();
    assert!(matches!(
        store.workflow_head("s1"),
        Err(SessionWorkError::StoreMissing(_))
    ));
    assert!(matches!(
        store.commit_workflow("s1", "r1", "- [ ] a: A\n", &agent(), at(0)),
        Err(SessionWorkError::StoreMissing(_))
    ));
    assert!(!store.exists().unwrap(), "reads never create the store");
    store.remove_unpublished("s1").unwrap();
    assert!(!store.exists().unwrap());
}

#[test]
fn unreadable_and_unknown_schema_stores_fail_closed() {
    let (_temp, store) = fresh();
    std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    std::fs::write(
        store.path(),
        b"this is not a database at all, not even close",
    )
    .unwrap();
    assert!(matches!(
        store.workflow_head("s1"),
        Err(SessionWorkError::Corrupt(_))
    ));
    assert!(matches!(
        store.activate(&activation("s1"), None),
        Err(SessionWorkError::Corrupt(_))
    ));

    let (_temp, store) = fresh();
    store.activate(&activation("s1"), None).unwrap();
    Connection::open(store.path())
        .unwrap()
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    assert!(matches!(
        store.workflow_head("s1"),
        Err(SessionWorkError::UnknownSchema(_))
    ));
    assert!(matches!(
        store.activate(&activation("s2"), None),
        Err(SessionWorkError::UnknownSchema(_))
    ));

    let (_temp, store) = fresh();
    std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    Connection::open(store.path())
        .unwrap()
        .execute_batch("CREATE TABLE foreign_table(x)")
        .unwrap();
    assert!(matches!(
        store.activate(&activation("s1"), None),
        Err(SessionWorkError::UnknownSchema(_))
    ));

    let (_temp, store) = fresh();
    store.activate(&activation("s1"), None).unwrap();
    Connection::open(store.path())
        .unwrap()
        .execute_batch("DROP TABLE journal")
        .unwrap();
    assert!(matches!(
        store.activation("s1"),
        Err(SessionWorkError::UnknownSchema(_))
    ));
}

#[test]
fn corrupt_and_unknown_records_fail_closed() {
    let (_temp, store) = fresh();
    store.activate(&activation("s1"), None).unwrap();
    store
        .commit_workflow("s1", "r1", "- [ ] a: A\n", &agent(), at(0))
        .unwrap();
    let connection = Connection::open(store.path()).unwrap();
    connection
        .execute(
            "UPDATE activation SET body=json_set(body, '$.future', 1) WHERE session='s1'",
            [],
        )
        .unwrap();
    assert!(matches!(
        store.activation("s1"),
        Err(SessionWorkError::Corrupt(_))
    ));
    connection
        .execute(
            "UPDATE workflow_revisions SET source='{\"kind\":\"robot\"}' WHERE session='s1'",
            [],
        )
        .unwrap();
    assert!(matches!(
        store.workflow_head("s1"),
        Err(SessionWorkError::Corrupt(_))
    ));
}

#[test]
fn commits_validate_first_and_converge_on_replay() {
    let (_temp, store) = fresh();
    assert!(matches!(
        {
            store.activate(&activation("other"), None).unwrap();
            store.commit_workflow("s1", "r1", "- [ ] a: A\n", &agent(), at(0))
        },
        Err(SessionWorkError::NotActivated(_))
    ));
    store.activate(&activation("s1"), None).unwrap();
    let invalid = store.commit_workflow("s1", "bad", "- [>] a: A\n- [>] b: B\n", &agent(), at(0));
    let Err(SessionWorkError::InvalidWorkflow(errors)) = invalid else {
        panic!("{invalid:?}");
    };
    assert_eq!(errors.0[0].line, Some(2));
    assert_eq!(store.workflow_head("s1").unwrap(), None);

    let first = store
        .commit_workflow("s1", "r1", "- [>] a: A\n", &agent(), at(1))
        .unwrap();
    assert_eq!(
        first,
        CommitReceipt {
            revision: 1,
            replayed: false
        }
    );
    let replay = store
        .commit_workflow("s1", "r1", "- [>] a: A\n", &agent(), at(2))
        .unwrap();
    assert_eq!(
        replay,
        CommitReceipt {
            revision: 1,
            replayed: true
        }
    );
    assert!(matches!(
        store.commit_workflow("s1", "r1", "- [x] a: A\n", &agent(), at(3)),
        Err(SessionWorkError::Conflict(_))
    ));
    // The failed attempt with a used identity added nothing.
    let second = store
        .commit_workflow("s1", "r2", "- [x] a: A\n", &agent(), at(4))
        .unwrap();
    assert_eq!(second.revision, 2);
    let head = store.workflow_head("s1").unwrap().unwrap();
    assert_eq!((head.revision.revision, head.synced), (2, 0));
    assert_eq!(head.revision.created_at, at(4));
    assert_eq!(store.recent_revisions("s1", 10).unwrap().len(), 2);
}

#[test]
fn the_written_mark_never_moves_backwards_or_past_the_head() {
    let (_temp, store) = fresh();
    store.activate(&activation("s1"), None).unwrap();
    for (request, text) in [("a", "- [ ] a: A\n"), ("b", "- [>] a: A\n")] {
        store
            .commit_workflow("s1", request, text, &agent(), at(0))
            .unwrap();
    }
    store.mark_synced("s1", 2).unwrap();
    store.mark_synced("s1", 1).unwrap();
    assert_eq!(store.workflow_head("s1").unwrap().unwrap().synced, 2);
    assert!(store.mark_synced("s1", 3).is_err());
    assert!(store.mark_synced("nobody", 1).is_err());
}

#[test]
fn module_times_follow_status_transitions() {
    let (_temp, store) = fresh();
    store.activate(&activation("s1"), None).unwrap();
    let commit = |request: &str, text: &str, second: i64| {
        store
            .commit_workflow("s1", request, text, &agent(), at(second))
            .unwrap();
    };
    commit("1", "- [ ] a: A\n- [ ] b: B\n", 0);
    assert!(store.module_times("s1").unwrap().is_empty());
    commit("2", "- [>] a: A\n- [ ] b: B\n", 10);
    commit("3", "- [x] a: A\n- [>] b: B\n", 20);
    commit("4", "- [x] a: A\n- [-] b: B\n  > skipped: not needed\n", 30);
    commit("5", "- [>] a: A\n- [-] b: B\n  > skipped: not needed\n", 40);
    let times = store.module_times("s1").unwrap();
    let a = times
        .iter()
        .find(|time| time.module.as_str() == "a")
        .unwrap();
    assert_eq!(a.first_active_at, Some(at(10)), "first activation is kept");
    assert_eq!(
        (a.finished_at, a.finished_as),
        (None, None),
        "reopening clears"
    );
    let b = times
        .iter()
        .find(|time| time.module.as_str() == "b")
        .unwrap();
    assert_eq!(b.first_active_at, Some(at(20)));
    assert_eq!(
        (b.finished_at, b.finished_as),
        (Some(at(30)), Some(ModuleStatus::Skipped))
    );
    commit("6", "- [x] a: A\n- [-] b: B\n  > skipped: not needed\n", 50);
    let times = store.module_times("s1").unwrap();
    let a = times
        .iter()
        .find(|time| time.module.as_str() == "a")
        .unwrap();
    assert_eq!(
        (a.first_active_at, a.finished_at),
        (Some(at(10)), Some(at(50)))
    );
}

#[test]
fn copied_revisions_do_not_claim_finishes_from_the_source_session() {
    let (_temp, store) = fresh();
    let mut copied = activation("copy");
    copied.origin = ActivationOrigin::Transfer {
        source_session: "source".into(),
    };
    store
        .activate(
            &copied,
            Some(&InitialRevision {
                text: "- [x] done-there: D\n- [>] now: N\n".into(),
                source: RevisionSource::Transfer {
                    source_session: "source".into(),
                    source_revision: 7,
                },
            }),
        )
        .unwrap();
    let times = store.module_times("copy").unwrap();
    assert_eq!(times.len(), 1);
    assert_eq!(times[0].module.as_str(), "now");
    assert_eq!(times[0].first_active_at, Some(copied.activated_at));
    let head = store.workflow_head("copy").unwrap().unwrap();
    assert_eq!((head.revision.revision, head.synced), (1, 0));
    assert_eq!(
        head.revision.source,
        RevisionSource::Transfer {
            source_session: "source".into(),
            source_revision: 7
        }
    );
}

#[test]
fn activation_is_idempotent_and_rejects_a_different_repeat() {
    let (_temp, store) = fresh();
    let template = InitialRevision {
        text: "- [>] a: A\n".into(),
        source: RevisionSource::Template {
            preset: "global:task-preset.fixture".into(),
        },
    };
    store.activate(&activation("s1"), Some(&template)).unwrap();
    store.activate(&activation("s1"), Some(&template)).unwrap();
    assert_eq!(store.recent_revisions("s1", 10).unwrap().len(), 1);
    let mut other = activation("s1");
    other.role = SessionWorkRole::Child;
    assert!(matches!(
        store.activate(&other, None),
        Err(SessionWorkError::Conflict(_))
    ));
    let invalid = InitialRevision {
        text: "not a workflow".into(),
        source: template.source.clone(),
    };
    assert!(matches!(
        store.activate(&activation("s2"), Some(&invalid)),
        Err(SessionWorkError::InvalidWorkflow(_))
    ));
    assert_eq!(
        store.activation("s2").unwrap(),
        None,
        "the failed activation left nothing"
    );
}

#[test]
fn items_get_per_kind_aliases_once_per_request() {
    let (_temp, store) = fresh();
    store.activate(&activation("s1"), None).unwrap();
    store.activate(&activation("s2"), None).unwrap();
    let allocate = |session: &str, request: &str, kind: ItemKind| {
        store
            .allocate_item(session, request, kind, "label", FinishPolicy::Wake, at(0))
            .unwrap()
            .alias
            .to_string()
    };
    assert_eq!(allocate("s1", "a", ItemKind::Task), "t1");
    assert_eq!(allocate("s1", "b", ItemKind::Task), "t2");
    assert_eq!(allocate("s1", "c", ItemKind::Question), "q1");
    assert_eq!(allocate("s2", "a", ItemKind::Task), "t1");
    assert_eq!(
        allocate("s1", "a", ItemKind::Task),
        "t1",
        "replay converges"
    );
    assert!(matches!(
        store.allocate_item(
            "s1",
            "a",
            ItemKind::Task,
            "other",
            FinishPolicy::Wake,
            at(0)
        ),
        Err(SessionWorkError::Conflict(_))
    ));
    assert!(matches!(
        store.allocate_item(
            "s1",
            "d",
            ItemKind::ProposalDone,
            "x",
            FinishPolicy::Wake,
            at(0)
        ),
        Err(SessionWorkError::Invalid(_))
    ));
    assert!(matches!(
        store.allocate_item(
            "nobody",
            "d",
            ItemKind::Task,
            "x",
            FinishPolicy::Hold,
            at(0)
        ),
        Err(SessionWorkError::NotActivated(_))
    ));
    let item = store.item("s1", "q1".parse().unwrap()).unwrap().unwrap();
    assert_eq!(
        (item.label.as_str(), item.policy),
        ("label", FinishPolicy::Wake)
    );
    assert_eq!(store.item("s1", "q2".parse().unwrap()).unwrap(), None);
}

#[test]
fn unpublished_removal_keeps_permanent_journal_and_events() {
    let (_temp, store) = fresh();
    store.activate(&activation("s1"), None).unwrap();
    store
        .commit_workflow("s1", "r1", "- [>] a: A\n", &agent(), at(0))
        .unwrap();
    store
        .allocate_item("s1", "i", ItemKind::Task, "t", FinishPolicy::Wake, at(0))
        .unwrap();
    let journal = store
        .append_journal(
            "s1",
            "turn_start",
            &serde_json::json!({"cause": "fixture"}),
            at(0),
        )
        .unwrap();
    let first = store
        .append_event("s1", "attention", "fixture", &serde_json::json!({}), at(0))
        .unwrap();
    let second = store
        .append_event("s2", "progress", "fixture", &serde_json::json!({}), at(1))
        .unwrap();
    assert!(journal > 0 && second > first);
    assert_eq!(
        store.events_after(first, 10).unwrap(),
        vec![(second, "s2".to_string(), "fixture".to_string())]
    );
    store.remove_unpublished("s1").unwrap();
    assert_eq!(store.activation("s1").unwrap(), None);
    assert_eq!(store.workflow_head("s1").unwrap(), None);
    assert!(store.module_times("s1").unwrap().is_empty());
    assert_eq!(store.events_after(0, 10).unwrap().len(), 2);
    let journal_rows: i64 = Connection::open(store.path())
        .unwrap()
        .query_row("SELECT count(*) FROM journal", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_rows, 1);
}

#[test]
fn concurrent_writers_never_lose_or_duplicate_revisions() {
    let (_temp, store) = fresh();
    store.activate(&activation("s1"), None).unwrap();
    let threads: Vec<_> = (0..8)
        .map(|writer| {
            let store = store.clone();
            std::thread::spawn(move || {
                for step in 0..10 {
                    let text = format!("- [>] w{writer}-{step}: Writer {writer}\n");
                    store
                        .commit_workflow("s1", &format!("{writer}:{step}"), &text, &agent(), at(0))
                        .unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let revisions = store.recent_revisions("s1", 1000).unwrap();
    let mut numbers: Vec<u32> = revisions.iter().map(|revision| revision.revision).collect();
    numbers.sort();
    assert_eq!(numbers, (1..=80).collect::<Vec<_>>());
    assert_eq!(
        store
            .workflow_head("s1")
            .unwrap()
            .unwrap()
            .revision
            .revision,
        80
    );
}
