use super::*;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use tempfile::TempDir;

fn context(temp: &TempDir) -> StartupContext {
    StartupContext::from_durable_state_dir(temp.path().join("state"))
}

fn resolve_non_git(context: &StartupContext, root: &Path) -> ActiveProject {
    fs::create_dir_all(root).expect("create project root");
    context.resolve_project(root).expect("resolve project")
}

fn write_file(path: &Path, bytes: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create file parent");
    }
    fs::write(path, bytes).expect("write fixture");
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_git(root: &Path) {
    fs::create_dir_all(root).expect("create Git root");
    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.name", "Jcode Test"]);
    git(root, &["config", "user.email", "jcode@example.invalid"]);
    write_file(&root.join("README.md"), "initial\n");
    git(root, &["add", "README.md"]);
    git(root, &["commit", "--quiet", "-m", "initial"]);
}

fn selected_paths(preview: &StartupSelectionPreview) -> Vec<PathBuf> {
    preview
        .selected()
        .map(|selected| selected.spec().path().as_path().to_path_buf())
        .collect()
}

fn issue_kinds(preparation: &StartupPreparation) -> Vec<&StartupFileIssueKind> {
    preparation.issues().map(StartupFileIssue::kind).collect()
}

#[test]
fn non_git_identity_uses_canonical_launch_directory() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let nested = root.join("nested");
    fs::create_dir_all(&nested).expect("create nested directory");
    let context = context(&temp);

    let root_project = context.resolve_project(&root).expect("resolve root");
    let nested_project = context.resolve_project(&nested).expect("resolve nested");

    assert_eq!(root_project.active_root(), root.canonicalize().unwrap());
    assert_eq!(nested_project.active_root(), nested.canonicalize().unwrap());
    assert!(!root_project.key().is_git());
    assert_ne!(root_project.key(), nested_project.key());
}

#[test]
fn git_subdirectories_share_identity_and_worktrees_share_plan_key() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_git(&root);
    fs::create_dir_all(root.join("nested/deeper")).expect("create nested Git directory");
    git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "startup-context-test-worktree",
            worktree.to_str().unwrap(),
        ],
    );
    let context = context(&temp);

    let root_project = context.resolve_project(&root).expect("resolve root");
    let nested_project = context
        .resolve_project(root.join("nested/deeper"))
        .expect("resolve subdirectory");
    let worktree_project = context
        .resolve_project(&worktree)
        .expect("resolve worktree");

    assert!(root_project.key().is_git());
    assert_eq!(root_project.key(), nested_project.key());
    assert_eq!(root_project.active_root(), nested_project.active_root());
    assert_eq!(root_project.key(), worktree_project.key());
    assert_ne!(root_project.active_root(), worktree_project.active_root());
}

#[test]
fn one_saved_relative_plan_captures_active_worktree_contents() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_git(&root);
    write_file(&root.join("PLAN.md"), "main worktree\n");
    git(&root, &["add", "PLAN.md"]);
    git(&root, &["commit", "--quiet", "-m", "add plan"]);
    git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "startup-context-capture-worktree",
            worktree.to_str().unwrap(),
        ],
    );
    write_file(&worktree.join("PLAN.md"), "linked worktree\n");

    let context = context(&temp);
    let main = context.resolve_project(&root).expect("resolve main");
    let linked = context.resolve_project(&worktree).expect("resolve linked");
    let preview = context.preview_selection(&main, [StartupSelectionInput::new("PLAN.md")]);
    let saved = context
        .save_project_plan(&main, 0, &preview)
        .expect("save shared plan");
    let linked_plan = context
        .load_project_plan(&linked)
        .expect("load shared worktree plan")
        .into_plan();

    assert_eq!(saved.entries(), linked_plan.entries());
    let main_capture = context
        .prepare_project_plan(&main, &saved, StartupFailurePolicy::Block)
        .expect("capture main")
        .into_preparation();
    let linked_capture = context
        .prepare_project_plan(&linked, &linked_plan, StartupFailurePolicy::Block)
        .expect("capture linked")
        .into_preparation();
    assert_eq!(
        main_capture.captured_files().next().unwrap().text(),
        "main worktree\n"
    );
    assert_eq!(
        linked_capture.captured_files().next().unwrap().text(),
        "linked worktree\n"
    );
}

#[test]
fn missing_plan_is_empty_and_saved_plan_contains_no_file_contents() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let context = context(&temp);
    let project = resolve_non_git(&context, &root);
    let missing = context
        .load_project_plan(&project)
        .expect("load missing plan");
    assert_eq!(missing.source(), StartupPlanLoadSource::Missing);
    assert_eq!(missing.plan().revision(), 0);
    assert!(missing.plan().entries().is_empty());

    let marker = "PRIVATE_FILE_CONTENT_MUST_NOT_ENTER_PLAN";
    write_file(&root.join("PLAN.md"), marker);
    let preview = context.preview_selection(&project, [StartupSelectionInput::new("PLAN.md")]);
    assert!(preview.is_valid());
    let saved = context
        .save_project_plan(&project, 0, &preview)
        .expect("save project plan");
    assert_eq!(saved.revision(), 1);
    assert_eq!(saved.entries().len(), 1);

    let plan_path = plan::stored_plan_path(&context.plan_store, project.key());
    let raw = fs::read_to_string(&plan_path).expect("read stored plan");
    assert!(!raw.contains(marker));
    assert!(!root.join(".jcode").exists());

    let loaded = context.load_project_plan(&project).expect("reload plan");
    assert_eq!(loaded.source(), StartupPlanLoadSource::Primary);
    assert_eq!(loaded.plan(), &saved);
}

#[test]
fn external_approval_round_trips_in_the_private_plan() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let external = temp.path().join("shared-plan.md");
    write_file(&external, "shared plan");
    let context = context(&temp);
    let project = resolve_non_git(&context, &root);
    let preview = context.preview_selection(
        &project,
        [StartupSelectionInput::new(&external).with_external_approval(&external)],
    );
    let saved = context
        .save_project_plan(&project, 0, &preview)
        .expect("save external plan");
    let loaded = context
        .load_project_plan(&project)
        .expect("load external plan")
        .into_plan();

    assert_eq!(loaded.entries(), saved.entries());
    assert_eq!(
        loaded.entries()[0]
            .external_approval()
            .unwrap()
            .approved_resolved_target(),
        external.canonicalize().unwrap()
    );
}

#[test]
fn plan_revision_changes_only_for_ordered_default_changes() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let context = context(&temp);
    let project = resolve_non_git(&context, &root);
    write_file(&root.join("one.md"), "one");
    write_file(&root.join("two.md"), "two");

    let first_preview = context.preview_selection(
        &project,
        [
            StartupSelectionInput::new("one.md"),
            StartupSelectionInput::new("two.md"),
        ],
    );
    let first = context
        .save_project_plan(&project, 0, &first_preview)
        .expect("save first plan");
    let unchanged = context
        .save_project_plan(&project, first.revision(), &first_preview)
        .expect("save unchanged plan");
    assert_eq!(unchanged.revision(), first.revision());
    assert_eq!(unchanged.updated_at(), first.updated_at());

    let reordered_preview = context.preview_selection(
        &project,
        [
            StartupSelectionInput::existing(first.entries()[1].id().clone(), "two.md"),
            StartupSelectionInput::existing(first.entries()[0].id().clone(), "one.md"),
        ],
    );
    let reordered = context
        .save_project_plan(&project, first.revision(), &reordered_preview)
        .expect("save reordered plan");
    assert_eq!(reordered.revision(), first.revision() + 1);
    assert!(matches!(
        context.save_project_plan(&project, first.revision(), &reordered_preview),
        Err(StartupContextError::StalePlanRevision { .. })
    ));
}

#[test]
fn corrupt_primary_recovers_backup_and_double_corruption_fails() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let context = context(&temp);
    let project = resolve_non_git(&context, &root);
    write_file(&root.join("one.md"), "one");
    write_file(&root.join("two.md"), "two");

    let first_preview = context.preview_selection(&project, [StartupSelectionInput::new("one.md")]);
    let first = context
        .save_project_plan(&project, 0, &first_preview)
        .expect("save first plan");
    let second_preview =
        context.preview_selection(&project, [StartupSelectionInput::new("two.md")]);
    let _second = context
        .save_project_plan(&project, first.revision(), &second_preview)
        .expect("save second plan");
    let plan_path = plan::stored_plan_path(&context.plan_store, project.key());
    let backup_path = plan_path.with_extension("bak");
    assert!(backup_path.exists());

    write_file(&plan_path, "{broken json");
    let recovered = context.load_project_plan(&project).expect("recover backup");
    assert_eq!(recovered.source(), StartupPlanLoadSource::RecoveredBackup);
    assert_eq!(recovered.plan().revision(), first.revision());

    write_file(&plan_path, "{still broken");
    write_file(&backup_path, "{also broken");
    assert!(matches!(
        context.load_project_plan(&project),
        Err(StartupContextError::PlanStorage { .. })
    ));
}

#[test]
fn semantically_invalid_primary_recovers_valid_backup() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let context = context(&temp);
    let project = resolve_non_git(&context, &root);
    write_file(&root.join("one.md"), "one");
    write_file(&root.join("two.md"), "two");
    let first_preview = context.preview_selection(&project, [StartupSelectionInput::new("one.md")]);
    let first = context
        .save_project_plan(&project, 0, &first_preview)
        .expect("save first plan");
    let second_preview =
        context.preview_selection(&project, [StartupSelectionInput::new("two.md")]);
    context
        .save_project_plan(&project, first.revision(), &second_preview)
        .expect("save second plan");
    let path = plan::stored_plan_path(&context.plan_store, project.key());
    let mut primary: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).expect("parse primary plan");
    primary["entries"][0]["id"] = serde_json::json!("not-a-hex-id");
    plan::write_raw_plan(&path, &primary);

    let recovered = context
        .load_project_plan(&project)
        .expect("recover semantic corruption");
    assert_eq!(recovered.source(), StartupPlanLoadSource::RecoveredBackup);
    assert_eq!(recovered.plan().revision(), first.revision());
}

#[test]
fn unknown_plan_schema_fails_without_falling_back_to_older_backup() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let context = context(&temp);
    let project = resolve_non_git(&context, &root);
    write_file(&root.join("one.md"), "one");
    let preview = context.preview_selection(&project, [StartupSelectionInput::new("one.md")]);
    context
        .save_project_plan(&project, 0, &preview)
        .expect("save plan");
    let path = plan::stored_plan_path(&context.plan_store, project.key());
    fs::copy(&path, path.with_extension("bak")).expect("copy valid backup");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).expect("parse plan");
    value["schema_version"] = serde_json::json!(99);
    plan::write_raw_plan(&path, &value);

    assert!(matches!(
        context.load_project_plan(&project),
        Err(StartupContextError::UnsupportedPlanSchema {
            schema_version: 99,
            ..
        })
    ));
}

#[test]
fn stored_project_identity_mismatch_is_reported_as_corrupt_state() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let other = temp.path().join("other-project");
    let context = context(&temp);
    let project = resolve_non_git(&context, &root);
    fs::create_dir_all(&other).expect("create other project");
    write_file(&root.join("one.md"), "one");
    let preview = context.preview_selection(&project, [StartupSelectionInput::new("one.md")]);
    context
        .save_project_plan(&project, 0, &preview)
        .expect("save plan");
    let path = plan::stored_plan_path(&context.plan_store, project.key());
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).expect("parse plan");
    value["project"] = serde_json::json!({
        "kind": "directory",
        "canonical_root": other.canonicalize().unwrap().to_str().unwrap(),
    });
    plan::write_raw_plan(&path, &value);

    assert!(matches!(
        context.load_project_plan(&project),
        Err(StartupContextError::PlanStorage { .. })
    ));
}

#[cfg(unix)]
#[test]
fn stored_plan_and_namespace_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let context = context(&temp);
    let project = resolve_non_git(&context, &root);
    write_file(&root.join("one.md"), "one");
    let preview = context.preview_selection(&project, [StartupSelectionInput::new("one.md")]);
    context
        .save_project_plan(&project, 0, &preview)
        .expect("save plan");
    let path = plan::stored_plan_path(&context.plan_store, project.key());

    assert_eq!(
        fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn selection_normalizes_absolute_project_paths_and_rejects_traversal() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let context = context(&temp);
    let project = resolve_non_git(&context, &root);
    write_file(&root.join("docs/PLAN.md"), "plan");

    let absolute = context.preview_selection(
        &project,
        [StartupSelectionInput::new(root.join("docs/PLAN.md"))],
    );
    assert!(absolute.is_valid());
    assert_eq!(selected_paths(&absolute), [PathBuf::from("docs/PLAN.md")]);
    assert!(
        absolute
            .selected()
            .next()
            .unwrap()
            .spec()
            .path()
            .is_project_relative()
    );

    let traversal = context.preview_selection(
        &project,
        [StartupSelectionInput::new("docs/../docs/PLAN.md")],
    );
    assert!(matches!(
        traversal.issues().next().unwrap().kind(),
        StartupFileIssueKind::PathTraversal
    ));
}

#[cfg(unix)]
#[test]
fn selection_rejects_non_utf8_paths() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let context = context(&temp);
    let project = resolve_non_git(&context, &root);
    let invalid = PathBuf::from(OsString::from_vec(vec![0xff, b'.', b'm', b'd']));
    let preview = context.preview_selection(&project, [StartupSelectionInput::new(invalid)]);
    assert!(matches!(
        preview.issues().next().unwrap().kind(),
        StartupFileIssueKind::InvalidPathEncoding
    ));
}

#[cfg(unix)]
#[test]
fn external_targets_require_bound_approval_and_retargeting_requires_confirmation() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).expect("create outside");
    let first = outside.join("first.md");
    let second = outside.join("second.md");
    write_file(&first, "first");
    write_file(&second, "second");
    let link = root.join("external-plan.md");
    fs::create_dir_all(&root).expect("create root");
    symlink(&first, &link).expect("create symlink");
    let context = context(&temp);
    let project = context.resolve_project(&root).expect("resolve project");

    let unapproved =
        context.preview_selection(&project, [StartupSelectionInput::new("external-plan.md")]);
    let required_target = match unapproved.issues().next().unwrap().kind() {
        StartupFileIssueKind::ExternalApprovalRequired { resolved_target } => {
            resolved_target.clone()
        }
        other => panic!("unexpected issue: {other:?}"),
    };
    assert_eq!(required_target, first.canonicalize().unwrap());

    let approved = context.preview_selection(
        &project,
        [StartupSelectionInput::new("external-plan.md")
            .with_external_approval(required_target.clone())],
    );
    assert!(approved.is_valid());
    let selected = approved.selected().next().unwrap();
    assert_eq!(
        selected.classification(),
        StartupPathClassification::External
    );
    assert!(selected.spec().path().is_project_relative());

    fs::remove_file(&link).expect("remove old symlink");
    symlink(&second, &link).expect("retarget symlink");
    let outcome = context
        .prepare_selection(&project, 0, &approved, StartupFailurePolicy::Block)
        .expect("prepare retargeted selection");
    assert!(matches!(outcome, StartupPreparationOutcome::Blocked(_)));
    assert!(matches!(
        outcome.preparation().issues().next().unwrap().kind(),
        StartupFileIssueKind::ExternalTargetChanged { .. }
    ));
}

#[test]
fn explicit_external_absolute_path_requires_and_accepts_confirmation() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let outside = temp.path().join("outside.md");
    write_file(&outside, "outside");
    let context = context(&temp);
    let project = resolve_non_git(&context, &root);

    let unapproved = context.preview_selection(&project, [StartupSelectionInput::new(&outside)]);
    assert!(matches!(
        unapproved.issues().next().unwrap().kind(),
        StartupFileIssueKind::ExternalApprovalRequired { .. }
    ));
    let approved = context.preview_selection(
        &project,
        [StartupSelectionInput::new(&outside).with_external_approval(&outside)],
    );
    assert!(approved.is_valid());
    assert!(matches!(
        approved.selected().next().unwrap().spec().path(),
        SelectedStartupPath::ExternalAbsolute(path) if path == &outside
    ));
}

#[test]
fn directory_expansion_is_direct_concrete_and_naturally_ordered() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let docs = root.join("docs");
    write_file(&docs.join("file10.md"), "10");
    write_file(&docs.join("file2.md"), "2");
    write_file(&docs.join("file1.md"), "1");
    write_file(&docs.join("nested/ignored.md"), "ignored");
    let context = context(&temp);
    let project = context.resolve_project(&root).expect("resolve project");

    let preview = context.expand_directory(&project, "docs");
    assert!(preview.is_valid());
    assert_eq!(
        selected_paths(&preview),
        [
            PathBuf::from("docs/file1.md"),
            PathBuf::from("docs/file2.md"),
            PathBuf::from("docs/file10.md"),
        ]
    );
    assert_eq!(
        selection::natural_name_cmp("phase2.md", "phase10.md"),
        std::cmp::Ordering::Less
    );
}

#[test]
fn complete_capture_preserves_exact_empty_and_unicode_bytes() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    write_file(&root.join("empty.md"), []);
    let unicode = "Plan 🌍\nİstanbul\n";
    write_file(&root.join("unicode.md"), unicode.as_bytes());
    let context = context(&temp);
    let project = context.resolve_project(&root).expect("resolve project");
    let preview = context.preview_selection(
        &project,
        [
            StartupSelectionInput::new("empty.md"),
            StartupSelectionInput::new("unicode.md"),
        ],
    );

    let outcome = context
        .prepare_selection(&project, 7, &preview, StartupFailurePolicy::Block)
        .expect("prepare capture");
    assert!(matches!(outcome, StartupPreparationOutcome::Ready(_)));
    let preparation = outcome.preparation();
    assert_eq!(preparation.plan_revision(), 7);
    let files = preparation.captured_files().collect::<Vec<_>>();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].text(), "");
    assert_eq!(files[0].bytes(), 0);
    assert_eq!(files[0].sha256(), format!("{:x}", Sha256::digest(b"")));
    assert_eq!(files[1].text().as_bytes(), unicode.as_bytes());
    assert_eq!(files[1].bytes(), unicode.len() as u64);
    assert_eq!(files[1].estimated_tokens(), (unicode.len() / 4) as u64);
    assert_eq!(
        files[1].sha256(),
        format!("{:x}", Sha256::digest(unicode.as_bytes()))
    );
}

#[test]
fn complete_capture_handles_large_bounded_text_without_truncation() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let text = "0123456789abcdef\n".repeat(65_536);
    write_file(&root.join("large.md"), text.as_bytes());
    let context = context(&temp);
    let project = context.resolve_project(&root).expect("resolve project");
    let preview = context.preview_selection(&project, [StartupSelectionInput::new("large.md")]);
    let outcome = context
        .prepare_selection(&project, 0, &preview, StartupFailurePolicy::Block)
        .expect("capture large bounded text");
    let captured = outcome.preparation().captured_files().next().unwrap();

    assert_eq!(captured.bytes(), text.len() as u64);
    assert_eq!(captured.text().as_bytes(), text.as_bytes());
    assert_eq!(
        captured.sha256(),
        format!("{:x}", Sha256::digest(text.as_bytes()))
    );
}

#[test]
fn invalid_and_unsupported_sources_are_typed_and_never_captured() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    fs::create_dir_all(root.join("directory")).expect("create directory target");
    write_file(&root.join("document.pdf"), b"plain text despite extension");
    write_file(&root.join("image.bin"), b"\x89PNG\r\n\x1a\nrest");
    write_file(&root.join("binary.bin"), b"abc\0def");
    write_file(&root.join("control.txt"), b"abc\x1bdef");
    write_file(&root.join("invalid-utf8.txt"), [0xf0, 0x28, 0x8c, 0x28]);
    let context = context(&temp);
    let project = context.resolve_project(&root).expect("resolve project");
    let preview = context.preview_selection(
        &project,
        [
            StartupSelectionInput::new("missing.md"),
            StartupSelectionInput::new("directory"),
            StartupSelectionInput::new("document.pdf"),
            StartupSelectionInput::new("image.bin"),
            StartupSelectionInput::new("binary.bin"),
            StartupSelectionInput::new("control.txt"),
            StartupSelectionInput::new("invalid-utf8.txt"),
        ],
    );
    let outcome = context
        .prepare_selection(&project, 0, &preview, StartupFailurePolicy::Block)
        .expect("prepare invalid selection");
    assert!(matches!(outcome, StartupPreparationOutcome::Blocked(_)));
    let preparation = outcome.preparation();
    assert_eq!(preparation.captured_files().count(), 0);
    let kinds = issue_kinds(preparation);
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, StartupFileIssueKind::Missing))
    );
    assert!(kinds.iter().any(|kind| matches!(
        kind,
        StartupFileIssueKind::UnsupportedTarget {
            target_type: StartupTargetType::Directory
        }
    )));
    assert!(kinds.iter().any(|kind| matches!(
        kind,
        StartupFileIssueKind::UnsupportedContent {
            content: StartupUnsupportedContent::Pdf
        }
    )));
    assert!(kinds.iter().any(|kind| matches!(
        kind,
        StartupFileIssueKind::UnsupportedContent {
            content: StartupUnsupportedContent::Image
        }
    )));
    assert!(kinds.iter().any(|kind| matches!(
        kind,
        StartupFileIssueKind::UnsupportedContent {
            content: StartupUnsupportedContent::Binary
        }
    )));
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| matches!(
                kind,
                StartupFileIssueKind::UnsupportedContent {
                    content: StartupUnsupportedContent::Binary
                }
            ))
            .count(),
        2
    );
    assert!(
        kinds
            .iter()
            .any(|kind| matches!(kind, StartupFileIssueKind::NonUtf8))
    );
}

#[cfg(unix)]
#[test]
fn broken_links_and_devices_are_rejected() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    fs::create_dir_all(&root).expect("create root");
    symlink(root.join("missing-target"), root.join("broken.md")).expect("create broken link");
    let context = context(&temp);
    let project = context.resolve_project(&root).expect("resolve project");
    let broken = context.preview_selection(&project, [StartupSelectionInput::new("broken.md")]);
    assert!(matches!(
        broken.issues().next().unwrap().kind(),
        StartupFileIssueKind::BrokenSymlink
    ));

    if Path::new("/dev/null").exists() {
        let device = context.preview_selection(
            &project,
            [StartupSelectionInput::new("/dev/null").with_external_approval("/dev/null")],
        );
        assert!(matches!(
            device.issues().next().unwrap().kind(),
            StartupFileIssueKind::UnsupportedTarget {
                target_type: StartupTargetType::DeviceOrSpecial
            }
        ));
    }
}

#[cfg(unix)]
#[test]
fn unreadable_regular_file_is_reported_where_permissions_are_enforced() {
    use std::os::unix::fs::PermissionsExt;

    if Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|uid| uid.trim() == "0")
    {
        return;
    }

    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let path = root.join("private.md");
    write_file(&path, "private");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("remove permissions");
    let context = context(&temp);
    let project = context.resolve_project(&root).expect("resolve project");
    let preview = context.preview_selection(&project, [StartupSelectionInput::new("private.md")]);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("restore permissions");
    assert!(matches!(
        preview.issues().next().unwrap().kind(),
        StartupFileIssueKind::Unreadable { .. }
    ));
}

#[test]
fn complete_source_bounds_are_typed_and_never_truncate() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    write_file(&root.join("large.md"), b"123456789");
    write_file(&root.join("five-a.md"), b"12345");
    write_file(&root.join("five-b.md"), b"abcde");
    write_file(&root.join("third.md"), b"x");
    let context = context(&temp).with_limits(2, 8);
    let project = context.resolve_project(&root).expect("resolve project");

    let file_too_large =
        context.preview_selection(&project, [StartupSelectionInput::new("large.md")]);
    assert!(matches!(
        file_too_large.issues().next().unwrap().kind(),
        StartupFileIssueKind::FileTooLarge { bytes: 9, limit: 8 }
    ));

    let batch_too_large = context.preview_selection(
        &project,
        [
            StartupSelectionInput::new("five-a.md"),
            StartupSelectionInput::new("five-b.md"),
        ],
    );
    assert!(matches!(
        batch_too_large.batch_issues()[0].kind(),
        StartupFileIssueKind::BatchTooLarge {
            bytes: 10,
            limit: 8
        }
    ));
    let prepared = context
        .prepare_selection(
            &project,
            0,
            &batch_too_large,
            StartupFailurePolicy::InjectDiagnostic,
        )
        .expect("prepare oversized batch");
    assert!(matches!(prepared, StartupPreparationOutcome::Diagnostic(_)));
    assert_eq!(prepared.preparation().captured_files().count(), 0);

    let too_many = context.preview_selection(
        &project,
        [
            StartupSelectionInput::new("five-a.md"),
            StartupSelectionInput::new("five-b.md"),
            StartupSelectionInput::new("third.md"),
        ],
    );
    assert!(matches!(
        too_many.batch_issues()[0].kind(),
        StartupFileIssueKind::TooManyEntries { count: 3, limit: 2 }
    ));
    let too_many_prepared = context
        .prepare_selection(
            &project,
            0,
            &too_many,
            StartupFailurePolicy::InjectDiagnostic,
        )
        .expect("prepare over-entry-limit selection");
    assert!(matches!(
        too_many_prepared,
        StartupPreparationOutcome::Diagnostic(_)
    ));
    assert!(matches!(
        too_many_prepared
            .preparation()
            .batch_issues()
            .first()
            .unwrap()
            .kind(),
        StartupFileIssueKind::TooManyEntries { count: 3, limit: 2 }
    ));
}

#[test]
fn duplicate_resolved_targets_are_rejected() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    write_file(&root.join("one.md"), "one");
    let context = context(&temp);
    let project = context.resolve_project(&root).expect("resolve project");
    let preview = context.preview_selection(
        &project,
        [
            StartupSelectionInput::new("one.md"),
            StartupSelectionInput::new(root.join("one.md")),
        ],
    );
    assert_eq!(preview.selected().count(), 1);
    assert!(matches!(
        preview.issues().next().unwrap().kind(),
        StartupFileIssueKind::DuplicateSelection {
            first_input_index: 0
        }
    ));
}

#[test]
fn block_and_diagnostic_policies_share_one_honest_issue_model() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    write_file(&root.join("valid.md"), "valid");
    let context = context(&temp);
    let project = context.resolve_project(&root).expect("resolve project");
    let preview = context.preview_selection(
        &project,
        [
            StartupSelectionInput::new("valid.md"),
            StartupSelectionInput::new("missing.md"),
        ],
    );

    let blocked = context
        .prepare_selection(&project, 0, &preview, StartupFailurePolicy::Block)
        .expect("prepare blocking policy");
    let diagnostic = context
        .prepare_selection(
            &project,
            0,
            &preview,
            StartupFailurePolicy::InjectDiagnostic,
        )
        .expect("prepare diagnostic policy");
    assert!(matches!(blocked, StartupPreparationOutcome::Blocked(_)));
    assert!(matches!(
        diagnostic,
        StartupPreparationOutcome::Diagnostic(_)
    ));
    assert_eq!(blocked.preparation(), diagnostic.preparation());
    assert_eq!(blocked.preparation().captured_files().count(), 1);
    assert_eq!(blocked.preparation().issues().count(), 1);
    assert!(matches!(
        blocked.preparation().issues().next().unwrap().kind(),
        StartupFileIssueKind::Missing
    ));
}

#[test]
fn changed_during_capture_retries_once_then_succeeds_when_source_stabilizes() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let path = root.join("changing.md");
    write_file(&path, "first-version");
    let context = context(&temp);
    let project = context.resolve_project(&root).expect("resolve project");
    let preview = context.preview_selection(&project, [StartupSelectionInput::new("changing.md")]);
    let calls = AtomicUsize::new(0);
    let hook = |path: &Path, _attempt: usize| {
        if calls.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
            write_file(path, "second-version");
        }
    };

    let outcome = capture::prepare_preview_with_hook(
        &project,
        0,
        &preview,
        StartupFailurePolicy::Block,
        DEFAULT_MAX_STARTUP_BATCH_BYTES,
        2,
        &hook,
    )
    .expect("prepare changing file");
    assert!(matches!(outcome, StartupPreparationOutcome::Ready(_)));
    assert_eq!(
        outcome
            .preparation()
            .captured_files()
            .next()
            .unwrap()
            .text(),
        "second-version"
    );
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);
}

#[test]
fn repeated_mutation_returns_changed_during_capture_without_claiming_a_read() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().join("project");
    let path = root.join("changing.md");
    write_file(&path, "version-0000");
    let context = context(&temp);
    let project = context.resolve_project(&root).expect("resolve project");
    let preview = context.preview_selection(&project, [StartupSelectionInput::new("changing.md")]);
    let calls = AtomicUsize::new(0);
    let hook = |path: &Path, attempt: usize| {
        calls.fetch_add(1, AtomicOrdering::SeqCst);
        write_file(path, format!("version-{attempt:04}"));
    };

    let outcome = capture::prepare_preview_with_hook(
        &project,
        0,
        &preview,
        StartupFailurePolicy::Block,
        DEFAULT_MAX_STARTUP_BATCH_BYTES,
        2,
        &hook,
    )
    .expect("prepare unstable file");
    assert!(matches!(outcome, StartupPreparationOutcome::Blocked(_)));
    assert_eq!(outcome.preparation().captured_files().count(), 0);
    assert!(matches!(
        outcome.preparation().issues().next().unwrap().kind(),
        StartupFileIssueKind::ChangedDuringCapture
    ));
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);
}
