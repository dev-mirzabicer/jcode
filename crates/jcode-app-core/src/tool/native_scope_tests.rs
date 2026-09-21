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
    let readable = f.b.join("read-without-grant");
    std::fs::write(&readable, "READABLE WITHOUT WRITE AUTHORITY").unwrap();
    let read = registry
        .execute("read", json!({"file_path":readable}), f.ctx())
        .await
        .unwrap();
    assert!(read.output.contains("READABLE WITHOUT WRITE AUTHORITY"));
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

#[tokio::test]
async fn native_scope_directory_symlinks_are_entries_not_directory_mutations() {
    let _guard = crate::storage::lock_test_env();
    let f = ScopeFixture::new();
    f.grant(f.b_id);
    let retained = f.b.join("retained");
    std::fs::write(&retained, "directory contents survive").unwrap();
    let link = f.a.join("directory-link");
    std::os::unix::fs::symlink(&f.b, &link).unwrap();
    let patch = format!(
        "*** Begin Patch\n*** Delete File: {}\n*** End Patch\n",
        link.display()
    );
    let output = apply_patch::ApplyPatchTool
        .execute(json!({"patch_text":patch}), f.ctx())
        .await
        .unwrap();
    assert!(!output.is_error, "{}", output.output);
    assert!(std::fs::symlink_metadata(&link).is_err());
    assert_eq!(
        std::fs::read_to_string(&retained).unwrap(),
        "directory contents survive"
    );
    std::os::unix::fs::symlink(&f.b, &link).unwrap();
    let early = f.a.join("early-directory-case");
    std::fs::write(&early, "unchanged\n").unwrap();
    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-unchanged\n+must-not-land\n*** Add File: {}\n+invalid directory write\n*** End Patch\n",
        early.display(),
        link.display()
    );
    assert!(
        apply_patch::ApplyPatchTool
            .execute(json!({"patch_text":patch}), f.ctx())
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&early).unwrap(), "unchanged\n");
    let patch = format!(
        "--- {}\n+++ /dev/null\n@@ -1 +0,0 @@\n-unreadable directory\n",
        link.display()
    );
    let output = patch::PatchTool
        .execute(json!({"patch_text":patch}), f.ctx())
        .await
        .unwrap();
    assert!(!output.is_error, "{}", output.output);
    assert!(std::fs::symlink_metadata(&link).is_err());
    assert_eq!(
        std::fs::read_to_string(&retained).unwrap(),
        "directory contents survive"
    );
}

#[tokio::test]
async fn native_scope_child_permissions_remain_frozen_but_parent_scope_is_live() {
    use crate::instruction::{
        InstructionId, InstructionKind, InstructionResourceRef, InstructionScope,
    };
    use jcode_tool_types::delegation::Permission;
    let _guard = crate::storage::lock_test_env();
    let mut f = ScopeFixture::new();
    let id = format!("session_scoped_child_{}", RequestId::new());
    let artifact_root = crate::storage::jcode_dir()
        .unwrap()
        .join("artifacts")
        .join(&id);
    std::fs::create_dir_all(&artifact_root).unwrap();
    let artifact_root = artifact_root.canonicalize().unwrap();
    let mut child = Session::create_with_id(id.clone(), None, None);
    let profile = crate::session::StoredAgentReference {
        scope: InstructionScope::Global,
        id: "fixture".into(),
        display_name: "Fixture".into(),
    };
    child.install_system_prompt(crate::session::StoredSystemPromptState {
        text: "SYNTHETIC SYSTEM".into(),
        active_agent: profile.clone(),
        first_provider_dispatch_at: None,
        active_transition_message_id: None,
    });
    let resolution = serde_json::from_value(json!({"requested_alias":"fixture","selection":jcode_provider_core::RouteSelection {model:"synthetic".into(),runtime_key:jcode_provider_core::RuntimeKey::OpenAIOAuth,api_method:"openai-oauth".into(),provider_label:"OpenAI".into(),detail:String::new()},"selected_effort":"high","used_model_override":false,"used_effort_override":false})).unwrap();
    child
        .install_isolated_child(
            crate::session::IsolatedChildIdentity {
                blocked_mcps: Default::default(),
                profile,
                original_parent: f.session.id.clone(),
                creation_run: "fixture-child-run".into(),
                working_dir: f.a.canonicalize().unwrap(),
                artifact_dir: artifact_root.clone(),
                resolution,
            },
            Permission::ReadOnly,
            crate::instruction::TaskPresetActivation {
                resource: InstructionResourceRef {
                    scope: InstructionScope::Global,
                    kind: InstructionKind::Notification,
                    id: InstructionId::parse("task-preset.general").unwrap(),
                },
                text: "SYNTHETIC PRESET".into(),
            },
        )
        .unwrap();
    child.save().unwrap();
    set_session_tool_policy(&id, None, Default::default());
    let mut registry = Registry::new(Arc::new(MockProvider)).await;
    registry.bind_child_policy(&child).unwrap();
    let context = || {
        let mut ctx = f.ctx();
        ctx.session_id = id.clone();
        ctx
    };
    let mut readonly_context = context();
    native_files::bind(&mut readonly_context, registry.child_policy.clone()).unwrap();
    let readonly_registry = registry.clone();
    let artifact = artifact_root.join("report");
    registry
        .execute(
            "write",
            json!({"file_path":artifact,"content":"artifact"}),
            context(),
        )
        .await
        .unwrap();
    let a = f.a.join("child-a");
    let b = f.b.join("child-b");
    assert!(
        registry
            .execute(
                "write",
                json!({"file_path":a,"content":"readonly denied"}),
                context()
            )
            .await
            .is_err()
    );
    child
        .stage_child_settings(Some(Permission::ReadWrite), None)
        .unwrap();
    child.save().unwrap();
    registry.bind_child_policy(&child).unwrap();
    let writable_registry = registry.clone();
    assert!(
        registry
            .execute(
                "write",
                json!({"file_path":a,"content":"old snapshot denied"}),
                readonly_context
            )
            .await
            .is_err()
    );
    assert!(
        readonly_registry
            .execute(
                "write",
                json!({"file_path":a,"content":"old registry denied"}),
                context()
            )
            .await
            .is_err()
    );
    registry
        .execute(
            "write",
            json!({"file_path":a,"content":"parent ordinary"}),
            context(),
        )
        .await
        .unwrap();
    assert!(
        registry
            .execute(
                "write",
                json!({"file_path":b,"content":"missing parent grant"}),
                context()
            )
            .await
            .is_err()
    );
    let grant = f.grant(f.b_id);
    registry
        .execute(
            "write",
            json!({"file_path":b,"content":"parent granted"}),
            context(),
        )
        .await
        .unwrap();
    child
        .stage_child_settings(Some(Permission::ReadOnly), None)
        .unwrap();
    child.save().unwrap();
    registry.bind_child_policy(&child).unwrap();
    assert!(
        registry
            .execute(
                "write",
                json!({"file_path":b,"content":"new readonly denied"}),
                context()
            )
            .await
            .is_err()
    );
    writable_registry
        .execute(
            "write",
            json!({"file_path":b,"content":"originating writable"}),
            context(),
        )
        .await
        .unwrap();
    grant_change(&f.workspace, GrantChange::Revoke { grant });
    assert!(
        writable_registry
            .execute(
                "write",
                json!({"file_path":b,"content":"revoked parent denied"}),
                context()
            )
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "originating writable");
    assert!(
        writable_registry
            .execute("selfdev", json!({"action":"status"}), context())
            .await
            .is_err()
    );
    let old_context = context(); // Invocation cwd remains the child's original directory.
    let next_context = || {
        let mut ctx = old_context.clone();
        ctx.tool_call_id = uuid::Uuid::new_v4().to_string();
        ctx
    };
    let prepared = f
        .workspace
        .prepare_primary_location(
            Placement::Standalone(f.b_id),
            Some(&f.b),
            OperationId::new(),
        )
        .unwrap();
    let mut next = prepared.location.clone();
    next.revision = f.session.location.as_ref().unwrap().revision + 1;
    f.session.working_dir = Some(next.cwd.observed_path().to_string_lossy().into());
    f.session.location = Some(next);
    drop(prepared);
    f.session.save().unwrap();
    writable_registry
        .execute(
            "write",
            json!({"file_path":b,"content":"current parent placement"}),
            next_context(),
        )
        .await
        .unwrap();
    assert!(
        writable_registry
            .execute(
                "write",
                json!({"file_path":a,"content":"child cwd is not authority"}),
                next_context()
            )
            .await
            .is_err()
    );
    let parent_path = crate::storage::jcode_dir()
        .unwrap()
        .join("sessions")
        .join(format!("{}.json", f.session.id));
    std::fs::rename(&parent_path, parent_path.with_extension("retained-fixture")).unwrap();
    assert!(
        writable_registry
            .execute(
                "write",
                json!({"file_path":b,"content":"missing parent denied"}),
                next_context()
            )
            .await
            .is_err()
    );
    writable_registry
        .execute(
            "write",
            json!({"file_path":artifact,"content":"independent artifact"}),
            next_context(),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&b).unwrap(),
        "current parent placement"
    );
    clear_session_tool_policy(&id);
}

#[tokio::test]
async fn native_scope_real_recursive_submodules_and_git_indirection_are_verified() {
    use jcode_tool_core::native_files::NativeFilePermit;
    let _guard = crate::storage::lock_test_env();
    let f = ScopeFixture::new();
    fn git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn seed(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        git(path, &["init", "-q"]);
        std::fs::write(path.join("data"), "original\n").unwrap();
        git(path, &["add", "data"]);
        git(
            path,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "seed",
            ],
        );
    }
    let leaf = f.temporary.path().join("leaf-source");
    seed(&leaf);
    let middle = f.temporary.path().join("middle-source");
    seed(&middle);
    git(
        &middle,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            leaf.to_str().unwrap(),
            "nested",
        ],
    );
    git(
        &middle,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qam",
            "nested",
        ],
    );
    seed(&f.a);
    git(
        &f.a,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            middle.to_str().unwrap(),
            "sub",
        ],
    );
    git(
        &f.a,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
            "--recursive",
            "-q",
        ],
    );
    let target = f.a.join("sub/nested/data");
    assert!(
        !write::WriteTool
            .execute(
                json!({"file_path":target,"content":"ordinary submodule"}),
                f.ctx()
            )
            .await
            .unwrap()
            .is_error
    );
    let sub_id = register(&f.workspace, &f.a.join("sub"));
    assert!(
        write::WriteTool
            .execute(
                json!({"file_path":target,"content":"registered boundary denied"}),
                f.ctx()
            )
            .await
            .is_err()
    );
    f.grant(sub_id);
    write::WriteTool
        .execute(
            json!({"file_path":target,"content":"explicit nested grant"}),
            f.ctx(),
        )
        .await
        .unwrap();
    let file = f.a.join("data");
    let mut admitted = f
        .workspace
        .acquire_native_mutation(&f.session, std::slice::from_ref(&file))
        .unwrap();
    let original = std::fs::read(&file).unwrap();
    std::fs::rename(f.a.join(".git"), f.a.join("retained-git")).unwrap();
    std::fs::write(
        f.a.join(".git"),
        format!("gitdir: {}\n", leaf.join(".git").display()),
    )
    .unwrap();
    assert!(admitted.write(&file, b"retargeted git denied").is_err());
    assert_eq!(std::fs::read(&file).unwrap(), original);
}

#[tokio::test]
async fn native_scope_revocation_during_real_pre_tool_hook_prevents_the_effect() {
    let _guard = crate::storage::lock_test_env();
    let mut f = ScopeFixture::new();
    let grant = f.grant(f.b_id);
    for key in [
        "JCODE_HOOKS_DISABLED",
        "JCODE_HOOK_PRE_TOOL",
        "JCODE_HOOK_PRE_TOOL_TIMEOUT_MS",
    ] {
        f.previous.push((key, std::env::var_os(key)));
    }
    let script = f.temporary.path().join("gate.py");
    let entered = f.temporary.path().join("entered");
    let release = f.temporary.path().join("release");
    let allowed = f.temporary.path().join("allowed");
    std::fs::write(&script,"import pathlib,sys,time\nsys.stdin.read()\nentered,release,allowed=map(pathlib.Path,sys.argv[1:])\nentered.write_text('entered')\nend=time.monotonic()+120\nwhile not release.exists():\n if time.monotonic()>end: sys.exit(2)\n time.sleep(0.02)\nallowed.write_text('allowed')\n").unwrap();
    let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    crate::env::remove_var("JCODE_HOOKS_DISABLED");
    crate::env::set_var(
        "JCODE_HOOK_PRE_TOOL",
        format!(
            "python3 {} {} {} {}",
            quote(&script),
            quote(&entered),
            quote(&release),
            quote(&allowed)
        ),
    );
    crate::env::set_var("JCODE_HOOK_PRE_TOOL_TIMEOUT_MS", "150000");
    crate::config::invalidate_config_cache();
    let registry = Registry::new(Arc::new(MockProvider)).await;
    let target = f.b.join("must-not-land");
    let invocation = registry.execute(
        "write",
        json!({"file_path":target,"content":"stale permission"}),
        f.ctx(),
    );
    let revoke = async {
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            while !entered.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        grant_change(&f.workspace, GrantChange::Revoke { grant });
        std::fs::write(&release, "release").unwrap();
    };
    let (result, ()) = tokio::join!(invocation, revoke);
    assert!(
        allowed.exists(),
        "The real hook must allow the operation before native scope rejects it"
    );
    assert!(result.is_err());
    assert!(!target.exists());
}
