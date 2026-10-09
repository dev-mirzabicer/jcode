use super::*;

#[test]
fn create_and_resume_goal_persists_project_goal() {
    let _guard = crate::storage::lock_test_env();
    // Dormant enabled path: both gates are explicitly on for the mirror.
    let _legacy =
        crate::config::feature_override::ScopedFeatureOverride::legacy_work_tracking(true);
    let _memory = crate::config::feature_override::ScopedFeatureOverride::memory(true);
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("repo");
    std::fs::create_dir_all(&project).expect("project dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let goal = create_goal(
        GoalCreateInput {
            title: "Ship mobile MVP".to_string(),
            scope: GoalScope::Project,
            next_steps: vec!["finish reconnect flow".to_string()],
            progress_percent: Some(40),
            ..GoalCreateInput::default()
        },
        Some(&project),
    )
    .expect("create goal");
    assert_eq!(goal.id, "ship-mobile-mvp");

    let loaded = load_goal(&goal.id, Some(GoalScope::Project), Some(&project))
        .expect("load")
        .expect("goal exists");
    assert_eq!(loaded.title, "Ship mobile MVP");

    let manager = crate::memory::MemoryManager::new().with_project_dir(&project);
    let graph = manager.load_project_graph().expect("load graph");
    let goal_mem = graph
        .get_memory(&format!("goal:{}", goal.id))
        .expect("goal memory mirror");
    assert!(goal_mem.tags.iter().any(|tag| tag == "goal"));
    assert!(goal_mem.content.contains("Ship mobile MVP"));

    let session_id = "ses_goal_test";
    attach_goal_to_session(session_id, &goal, Some(&project)).expect("attach");
    let resumed = resume_goal(session_id, Some(&project))
        .expect("resume")
        .expect("goal resumed");
    assert_eq!(resumed.id, goal.id);

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn write_goal_page_auto_focuses_first_goal_only() {
    let _guard = crate::storage::lock_test_env();
    let _legacy =
        crate::config::feature_override::ScopedFeatureOverride::legacy_work_tracking(true);
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("repo");
    std::fs::create_dir_all(&project).expect("project dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let session_id = "ses_goal_panel";
    let goal = create_goal(
        GoalCreateInput {
            title: "Ship mobile MVP".to_string(),
            scope: GoalScope::Project,
            ..GoalCreateInput::default()
        },
        Some(&project),
    )
    .expect("create goal");

    let first = write_goal_page(session_id, Some(&project), &goal, GoalDisplayMode::Auto)
        .expect("first write");
    assert_eq!(
        first.focused_page_id.as_deref(),
        Some("goal.ship-mobile-mvp")
    );

    crate::side_panel::write_markdown_page(session_id, "notes", Some("Notes"), "# Notes", true)
        .expect("notes");
    let second = write_goal_page(session_id, Some(&project), &goal, GoalDisplayMode::Auto)
        .expect("second write");
    assert_eq!(second.focused_page_id.as_deref(), Some("notes"));

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

fn tree_fingerprint(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let relative = path.strip_prefix(root).unwrap().display().to_string();
                out.push((relative, std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn legacy_work_tracking_goal_store_fails_closed_and_retains_every_file() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("repo");
    std::fs::create_dir_all(&project).expect("project dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    // Synthetic retained data, written while the store was enabled.
    let goal = {
        let _on =
            crate::config::feature_override::ScopedFeatureOverride::legacy_work_tracking(true);
        let goal = create_goal(
            GoalCreateInput {
                title: "Synthetic retained goal".into(),
                scope: GoalScope::Project,
                ..GoalCreateInput::default()
            },
            Some(&project),
        )
        .expect("create retained goal");
        attach_goal_to_session("ses_retained", &goal, Some(&project)).expect("attach");
        goal
    };
    let goals = temp.path().join("goals");
    std::fs::write(goals.join("global-extra.json.bak"), b"{\"backup\":true}").unwrap();
    let before = tree_fingerprint(&goals);
    assert!(before.len() >= 3, "{before:?}");

    let _off = crate::config::feature_override::ScopedFeatureOverride::legacy_work_tracking(false);
    let unavailable = |error: anyhow::Error| {
        assert_eq!(
            error.to_string(),
            crate::config::LEGACY_WORK_TRACKING_UNAVAILABLE
        )
    };
    unavailable(
        create_goal(
            GoalCreateInput {
                title: "New".into(),
                scope: GoalScope::Global,
                ..GoalCreateInput::default()
            },
            Some(&project),
        )
        .unwrap_err(),
    );
    unavailable(
        update_goal(&goal.id, None, Some(&project), GoalUpdateInput::default()).unwrap_err(),
    );
    unavailable(load_goal(&goal.id, None, Some(&project)).unwrap_err());
    unavailable(list_relevant_goals(Some(&project)).unwrap_err());
    unavailable(resume_goal("ses_retained", Some(&project)).unwrap_err());
    unavailable(attach_goal_to_session("ses_new", &goal, Some(&project)).unwrap_err());
    unavailable(load_attached_goal("ses_retained", Some(&project)).unwrap_err());
    unavailable(open_goals_overview_for_session("ses_new", Some(&project), true).unwrap_err());
    unavailable(refresh_goals_overview_for_session("ses_new", Some(&project)).unwrap_err());
    unavailable(open_goal_for_session("ses_new", Some(&project), &goal.id, true).unwrap_err());
    unavailable(resume_goal_for_session("ses_retained", Some(&project), true).unwrap_err());
    unavailable(
        write_goal_page("ses_new", Some(&project), &goal, GoalDisplayMode::Focus).unwrap_err(),
    );
    let snapshot = crate::side_panel::SidePanelSnapshot::default();
    assert_eq!(header_badge(Some(&project), &snapshot), None);

    assert_eq!(tree_fingerprint(&goals), before);
    assert!(
        crate::side_panel::snapshot_for_session("ses_new")
            .expect("side panel snapshot")
            .pages
            .is_empty()
    );
    assert!(!temp.path().join("memory").exists());

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn legacy_work_tracking_enabled_goal_save_never_reaches_disabled_memory() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("repo");
    std::fs::create_dir_all(&project).expect("project dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());
    let _on = crate::config::feature_override::ScopedFeatureOverride::legacy_work_tracking(true);
    let _memory = crate::config::feature_override::ScopedFeatureOverride::memory(false);

    let goal = create_goal(
        GoalCreateInput {
            title: "Memory stays off".into(),
            scope: GoalScope::Project,
            ..GoalCreateInput::default()
        },
        Some(&project),
    )
    .expect("goal saves without the memory mirror");
    update_goal(
        &goal.id,
        None,
        Some(&project),
        GoalUpdateInput {
            checkpoint_summary: Some("synthetic".into()),
            ..GoalUpdateInput::default()
        },
    )
    .expect("update")
    .expect("goal exists");
    assert!(load_goal(&goal.id, None, Some(&project)).unwrap().is_some());
    assert!(!temp.path().join("memory").exists());

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}
