use super::*;
use crate::session::Session;
use crate::workspace::*;
use serde_json::json;
use std::path::{Path, PathBuf};

struct ScopeFixture {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
    temporary: tempfile::TempDir,
    workspace: WorkspaceService,
    session: Session,
    a: PathBuf,
    b: PathBuf,
    b_id: LocationId,
}
impl Drop for ScopeFixture {
    fn drop(&mut self) {
        for (key, value) in &self.previous {
            match value {
                Some(v) => crate::env::set_var(key, v),
                None => crate::env::remove_var(key),
            }
        }
        crate::config::invalidate_config_cache();
    }
}
impl ScopeFixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let mut previous = Vec::new();
        for (key, subdir) in [
            ("HOME", "home"),
            ("JCODE_HOME", "jcode"),
            ("JCODE_RUNTIME_DIR", "runtime"),
            ("XDG_CONFIG_HOME", "config"),
        ] {
            previous.push((key, std::env::var_os(key)));
            std::fs::create_dir_all(temporary.path().join(subdir)).unwrap();
            crate::env::set_var(key, temporary.path().join(subdir));
        }
        crate::config::invalidate_config_cache();
        let workspace = WorkspaceService::new(&crate::storage::durable_state_dir());
        workspace.initialize(RequestId::new()).unwrap();
        let a = temporary.path().join("a");
        let b = temporary.path().join("b");
        let a_id = register(&workspace, &a);
        let b_id = register(&workspace, &b);
        let prepared = workspace
            .prepare_primary_location(Placement::Standalone(a_id), Some(&a), OperationId::new())
            .unwrap();
        let mut session =
            Session::create_with_id(format!("session_native_{}", RequestId::new()), None, None);
        session.working_dir = Some(
            prepared
                .location
                .cwd
                .observed_path()
                .to_string_lossy()
                .into(),
        );
        session.location = Some(prepared.location);
        session.save().unwrap();
        Self {
            previous,
            temporary,
            workspace,
            session,
            a,
            b,
            b_id,
        }
    }
    fn ctx(&self) -> ToolContext {
        ToolContext {
            session_id: self.session.id.clone(),
            message_id: "native-message".into(),
            tool_call_id: uuid::Uuid::new_v4().to_string(),
            working_dir: Some(self.a.clone()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
            invocation: Default::default(),
        }
    }
    fn grant(&self, target: LocationId) -> GrantId {
        grant_change(
            &self.workspace,
            GrantChange::Issue {
                audience: Audience::Session(self.session.id.clone()),
                target: WriteTarget::Root(target),
                proposal: None,
            },
        )
        .grant
        .unwrap()
        .id
    }
}
fn register(workspace: &WorkspaceService, path: &Path) -> LocationId {
    std::fs::create_dir_all(path).unwrap();
    let review = workspace
        .review_organization_change(
            workspace.status().unwrap().revision,
            OrganizationChange::RegisterLocation {
                name: "fixture".into(),
                path: path.into(),
                registration: Registration::Standalone,
            },
        )
        .unwrap();
    match workspace
        .apply_organization_change(RequestId::new(), review.id)
        .unwrap()
        .targets[0]
    {
        EntityId::Location(id) => id,
        _ => panic!(),
    }
}
fn grant_change(workspace: &WorkspaceService, change: GrantChange) -> PermissionMutation {
    let review = workspace
        .review_grant_change(workspace.status().unwrap().revision, change)
        .unwrap();
    workspace
        .apply_grant_change(
            &WorkspaceClientAuthority::authenticated("native-fixture-human").unwrap(),
            RequestId::new(),
            review.id,
        )
        .unwrap()
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_scope_real_mutators_aliases_and_all_patch_destinations() {
    let _guard = crate::storage::lock_test_env();
    let f = ScopeFixture::new();
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let file = f.a.join("file");
    let other = f.b.join("other");
    let output = registry
        .execute(
            "write",
            json!({"file_path":file,"content":"one\n"}),
            f.ctx(),
        )
        .await
        .unwrap();
    assert!(!output.is_error);
    assert!(
        registry
            .execute(
                "functions.write",
                json!({"file_path":other,"content":"denied"}),
                f.ctx()
            )
            .await
            .is_err()
    );
    assert!(
        write::WriteTool
            .execute(
                json!({"file_path":other,"content":"direct denied"}),
                f.ctx()
            )
            .await
            .is_err()
    );
    assert!(!other.exists());
    assert!(
        !registry
            .execute(
                "edit",
                json!({"file_path":file,"old_string":"one","new_string":"two"}),
                f.ctx()
            )
            .await
            .unwrap()
            .is_error
    );
    assert!(
        !registry
            .execute(
                "multiedit",
                json!({"file_path":file,"edits":[{"old_string":"two","new_string":"three"}]}),
                f.ctx()
            )
            .await
            .unwrap()
            .is_error
    );
    assert!(!registry.execute("patch", json!({"patch_text":format!("--- {}\n+++ {}\n@@ -1 +1 @@\n-three\n+four\n",file.display(),file.display())}), f.ctx()).await.unwrap().is_error);
    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-four\n+should-not-land\n*** Add File: {}\n+denied\n*** End Patch\n",
        file.display(),
        other.display()
    );
    assert!(
        registry
            .execute("apply_patch", json!({"patch_text":patch}), f.ctx())
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "four\n");
    let patch = format!(
        "--- {}\n+++ {}\n@@ -1 +1 @@\n-four\n+should-not-land\n--- /dev/null\n+++ {}\n@@ -0,0 +1 @@\n+denied\n",
        file.display(),
        file.display(),
        other.display()
    );
    assert!(
        registry
            .execute("patch", json!({"patch_text":patch}), f.ctx())
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "four\n");
    let grant = f.grant(f.b_id);
    assert!(
        !registry
            .execute(
                "functions.write",
                json!({"file_path":other,"content":"granted\n"}),
                f.ctx()
            )
            .await
            .unwrap()
            .is_error
    );
    let moved = f.b.join("nested/moved");
    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n-four\n+five\n*** End Patch\n",
        file.display(),
        moved.display()
    );
    assert!(
        !registry
            .execute("apply_patch", json!({"patch_text":patch}), f.ctx())
            .await
            .unwrap()
            .is_error
    );
    assert!(!file.exists());
    assert_eq!(std::fs::read_to_string(&moved).unwrap(), "five\n");
    grant_change(&f.workspace, GrantChange::Revoke { grant });
    assert!(
        registry
            .execute(
                "write",
                json!({"file_path":other,"content":"revoked"}),
                f.ctx()
            )
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "granted\n");
    let batch = registry
        .execute(
            "batch",
            json!({"tool_calls":[
                {"tool":"write","file_path":f.a.join("batch1"),"content":"a"},
                {"tool":"functions.write","file_path":f.a.join("batch2"),"content":"b"}
            ]}),
            f.ctx(),
        )
        .await
        .unwrap();
    assert!(
        batch.output.contains("2 succeeded, 0 failed"),
        "{}",
        batch.output
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_scope_admitted_work_survives_revocation_and_nested_roots_remain_boundaries() {
    use jcode_tool_core::native_files::NativeFilePermit;
    let _guard = crate::storage::lock_test_env();
    let f = ScopeFixture::new();
    let grant = f.grant(f.b_id);
    let target = f.b.join("inflight");
    let mut admitted = f
        .workspace
        .acquire_native_mutation(&f.session, std::slice::from_ref(&target))
        .unwrap();
    assert!(f.workspace.acquire_root(f.b_id).is_err());
    grant_change(&f.workspace, GrantChange::Revoke { grant });
    admitted.write(&target, b"already admitted").unwrap();
    assert!(
        f.workspace
            .acquire_native_mutation(&f.session, &[f.b.join("future")])
            .is_err()
    );
    drop(admitted);
    assert!(f.workspace.acquire_root(f.b_id).is_ok());
    let nested = f.a.join("nested");
    std::fs::create_dir(&nested).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&nested)
            .status()
            .unwrap()
            .success()
    );
    let path = nested.join("file");
    assert!(
        write::WriteTool
            .execute(json!({"file_path":path,"content":"unregistered"}), f.ctx())
            .await
            .is_err()
    );
    let nested_id = register(&f.workspace, &nested);
    assert!(
        write::WriteTool
            .execute(json!({"file_path":path,"content":"ungranted"}), f.ctx())
            .await
            .is_err()
    );
    f.grant(nested_id);
    assert!(
        !write::WriteTool
            .execute(json!({"file_path":path,"content":"approved"}), f.ctx())
            .await
            .unwrap()
            .is_error
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "approved");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_scope_broad_roots_do_not_authorize_control_state_or_unknown_sessions() {
    let _guard = crate::storage::lock_test_env();
    let mut f = ScopeFixture::new();
    let broad = register(&f.workspace, f.temporary.path());
    let prepared = f
        .workspace
        .prepare_primary_location(
            Placement::Standalone(broad),
            Some(f.temporary.path()),
            OperationId::new(),
        )
        .unwrap();
    f.session.working_dir = Some(
        prepared
            .location
            .cwd
            .observed_path()
            .to_string_lossy()
            .into(),
    );
    f.session.location = Some(prepared.location.clone());
    drop(prepared);
    f.session.save().unwrap();
    let control = crate::storage::jcode_dir()
        .unwrap()
        .join("sessions")
        .join(format!("{}.json", f.session.id));
    let before = std::fs::read(&control).unwrap();
    assert!(
        write::WriteTool
            .execute(json!({"file_path":control,"content":"forged"}), f.ctx())
            .await
            .is_err()
    );
    let scratch = crate::storage::jcode_dir().unwrap().join("scratch");
    std::os::unix::fs::symlink(control.parent().unwrap(), &scratch).unwrap();
    let alias = scratch.join(control.file_name().unwrap());
    assert!(
        write::WriteTool
            .execute(json!({"file_path":alias,"content":"forged alias"}), f.ctx())
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&control).unwrap(), before);
    let mut context = f.ctx();
    context.session_id = "missing_session".into();
    assert!(
        write::WriteTool
            .execute(
                json!({"file_path":f.a.join("absent"),"content":"unknown"}),
                context
            )
            .await
            .is_err()
    );
    assert!(!f.a.join("absent").exists());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_scope_symlink_delete_preserves_its_referent() {
    let _guard = crate::storage::lock_test_env();
    let f = ScopeFixture::new();
    f.grant(f.b_id);
    let referent = f.b.join("retained");
    let link = f.a.join("link");
    std::fs::write(&referent, "must survive unlink").unwrap();
    std::os::unix::fs::symlink(&referent, &link).unwrap();
    let patch = format!(
        "*** Begin Patch\n*** Delete File: {}\n*** End Patch\n",
        link.display()
    );
    let output = apply_patch::ApplyPatchTool
        .execute(json!({"patch_text":patch}), f.ctx())
        .await
        .unwrap();
    assert!(!output.is_error, "{}", output.output);
    assert_eq!(
        std::fs::read_to_string(&referent).unwrap(),
        "must survive unlink"
    );
    assert!(std::fs::symlink_metadata(&link).is_err());
    let outside = f.temporary.path().join("unregistered-entry");
    std::fs::create_dir(&outside).unwrap();
    let external_link = outside.join("link");
    std::os::unix::fs::symlink(&referent, &external_link).unwrap();
    // Writing follows the authorized referent. Unlinking additionally mutates
    // the entry's directory, which is not authorized merely by that reference.
    assert!(
        !write::WriteTool
            .execute(
                json!({"file_path":external_link,"content":"allowed referent"}),
                f.ctx()
            )
            .await
            .unwrap()
            .is_error
    );
    let early = f.a.join("early");
    std::fs::write(&early, "unchanged\n").unwrap();
    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-unchanged\n+must-not-land\n*** Delete File: {}\n*** End Patch\n",
        early.display(),
        external_link.display()
    );
    assert!(
        apply_patch::ApplyPatchTool
            .execute(json!({"patch_text":patch}), f.ctx())
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&early).unwrap(), "unchanged\n");
    assert_eq!(
        std::fs::read_to_string(&referent).unwrap(),
        "allowed referent"
    );
    assert!(
        std::fs::symlink_metadata(&external_link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
