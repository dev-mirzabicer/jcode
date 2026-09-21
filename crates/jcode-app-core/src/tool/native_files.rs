//! Invocation-bound native mutation policy. Only code installs it, and every
//! concrete native file tool acquires it after queued work and pre-tool hooks.
use super::{ToolContext, child_policy::ChildToolPolicy};
use crate::session::Session;
use crate::workspace::WorkspaceService;
use anyhow::{Context, Result, ensure};
#[cfg(target_os = "macos")]
use jcode_base::location::native_files::VerifiedFiles;
use jcode_base::location::native_files::{resolve_removal_entry, resolve_target};
#[cfg(not(target_os = "macos"))]
#[path = "native_files_legacy.rs"]
mod legacy;
use jcode_tool_core::native_files::{NativeFilePermit, NativeFilePlan, NativeFilePolicy};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct BoundPolicy {
    session: String,
    state: PathBuf,
    durable: PathBuf,
    runtime: Option<PathBuf>,
    child: Option<Arc<ChildToolPolicy>>,
}
impl BoundPolicy {
    fn new(ctx: &ToolContext, child: Option<Arc<ChildToolPolicy>>) -> Result<Self> {
        Ok(Self {
            session: ctx.session_id.clone(),
            state: crate::storage::jcode_dir()?,
            durable: crate::storage::durable_state_dir(),
            runtime: std::env::var_os("JCODE_RUNTIME_DIR").map(PathBuf::from),
            child,
        })
    }
    fn load(&self, id: &str) -> Result<Session> {
        Session::load_startup_stub_in(&self.state, id).context("Read authoritative native mutation scope; missing/corrupt Sessions cannot acquire authority")
    }
}
impl NativeFilePolicy for BoundPolicy {
    fn session_id(&self) -> &str {
        &self.session
    }
    fn acquire(&self, plan: &NativeFilePlan) -> Result<Box<dyn NativeFilePermit>> {
        let paths = plan.paths();
        let removals = plan.removals();
        let session = self.load(&self.session)?;
        let mut resolved = Vec::new();
        for path in paths {
            if let Some(child) = &self.child {
                ensure!(
                    child.session_id == self.session,
                    "Native child policy belongs to another Session"
                );
                child.check_path(path)?;
            }
            resolved.push((path.clone(), resolve_target(path)?));
        }
        let mut artifacts_only = false;
        let parent = if let Some(child) = &self.child {
            let identity = &session
                .isolated_child
                .as_ref()
                .context("Bound child lost its durable identity")?
                .identity;
            ensure!(
                identity.original_parent == child.original_parent
                    && identity.artifact_dir == child.artifacts,
                "Isolated child identity changed"
            );
            artifacts_only = resolved
                .iter()
                .all(|(_, p)| p.starts_with(&child.artifacts) && p != &child.artifacts);
            if artifacts_only {
                None
            } else {
                Some(self.load(&child.original_parent)?)
            }
        } else {
            ensure!(
                session.isolated_child.is_none(),
                "Isolated mutations require the originating Registry permission snapshot"
            );
            None
        };
        let principal = parent.as_ref().unwrap_or(&session);
        let managed = principal.location.is_some();
        ensure!(
            artifacts_only || managed || !crate::config::config().features.managed_primary_launch,
            "Legacy Session needs reviewed placement before native mutation; open workspace management"
        );
        let protection = if managed || self.child.is_some() {
            let mut roots = vec![resolve_target(&self.state)?, resolve_target(&self.durable)?];
            if let Some(runtime) = &self.runtime {
                roots.push(resolve_target(runtime)?);
            }
            let expected_scratch = resolve_target(&self.state)?.join("scratch");
            let scratch = (resolve_target(&expected_scratch)? == expected_scratch)
                .then_some(expected_scratch);
            let artifacts = self.child.as_ref().map(|child| child.artifacts.clone());
            let protection = Protection {
                roots,
                scratch,
                artifacts,
            };
            for (_, path) in &resolved {
                protection.check(path)?;
            }
            for path in removals {
                protection.check(&resolve_removal_entry(path)?)?;
            }
            Some(protection)
        } else {
            None
        };
        let files: Box<dyn NativeFilePermit> = if managed && !artifacts_only {
            let workspace = WorkspaceService::new(&self.durable);
            let permit = if self.child.is_some() {
                workspace.acquire_child_native_mutation(principal, &session, plan)?
            } else {
                workspace.acquire_native_operation(principal, plan)?
            };
            let current = self.load(&principal.id)?;
            ensure!(
                current.location == principal.location,
                "Session placement changed during native admission; retry with current scope"
            );
            permit.confirm_admission()?;
            Box::new(permit)
        } else {
            unmanaged_permit(&resolved, plan)?
        };
        Ok(Box::new(RestrictedPermit {
            files,
            protection,
            child: self.child.clone(),
        }))
    }
}
struct Protection {
    roots: Vec<PathBuf>,
    scratch: Option<PathBuf>,
    artifacts: Option<PathBuf>,
}
impl Protection {
    fn check(&self, path: &Path) -> Result<()> {
        let artifact = self
            .artifacts
            .as_ref()
            .is_some_and(|root| path.starts_with(root) && path != root);
        let scratch = self
            .scratch
            .as_ref()
            .is_some_and(|root| path.starts_with(root) && path != root);
        ensure!(
            artifact || scratch || !self.roots.iter().any(|root| path.starts_with(root)),
            "Native file mutation cannot edit harness control state; use its authorized service"
        );
        Ok(())
    }
}
struct RestrictedPermit {
    files: Box<dyn NativeFilePermit>,
    protection: Option<Protection>,
    child: Option<Arc<ChildToolPolicy>>,
}
impl RestrictedPermit {
    fn check(&self, path: &Path) -> Result<()> {
        if let Some(child) = &self.child {
            child.check_path(path)?;
        }
        if let Some(protection) = &self.protection {
            protection.check(&resolve_target(path)?)?;
        }
        Ok(())
    }
}
impl NativeFilePermit for RestrictedPermit {
    fn read(&mut self, p: &Path) -> Result<Option<Vec<u8>>> {
        self.check(p)?;
        self.files.read(p)
    }
    fn write(&mut self, p: &Path, bytes: &[u8]) -> Result<()> {
        self.check(p)?;
        self.files.write(p, bytes)
    }
    fn remove(&mut self, p: &Path) -> Result<()> {
        self.check(p)?;
        if let Some(protection) = &self.protection {
            protection.check(&resolve_removal_entry(p)?)?;
        }
        self.files.remove(p)
    }
    fn same_file(&self, a: &Path, b: &Path) -> Result<bool> {
        self.files.same_file(a, b)
    }
    fn copy_metadata(&mut self, a: &Path, b: &Path) -> Result<()> {
        self.check(a)?;
        self.check(b)?;
        self.files.copy_metadata(a, b)
    }
}

pub(super) fn bind(ctx: &mut ToolContext, child: Option<Arc<ChildToolPolicy>>) -> Result<()> {
    if let Some(policy) = &ctx.invocation.native_files {
        ensure!(
            policy.session_id() == ctx.session_id,
            "Native invocation authority cannot be rebound to another Session"
        );
    }
    if ctx.invocation.native_files.is_none() {
        let child = child.or_else(|| {
            super::session_tool_policy(&ctx.session_id)
                .and_then(|p| p.child)
                .map(Arc::new)
        });
        ctx.invocation.native_files = Some(Arc::new(BoundPolicy::new(ctx, child)?));
    }
    Ok(())
}

fn unmanaged_permit(
    paths: &[(PathBuf, PathBuf)],
    plan: &NativeFilePlan,
) -> Result<Box<dyn NativeFilePermit>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(VerifiedFiles::acquire_plan(paths, plan)?))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(Box::new(legacy::LegacyFiles::acquire(
            paths,
            plan.removals(),
        )))
    }
}

pub(super) struct NativeFiles {
    permit: Arc<Mutex<Box<dyn NativeFilePermit>>>,
    context: ToolContext,
}
impl NativeFiles {
    pub async fn acquire(ctx: &ToolContext, paths: Vec<PathBuf>) -> Result<Self> {
        Self::acquire_plan(ctx, NativeFilePlan::new(paths, Vec::new())).await
    }
    pub async fn acquire_plan(ctx: &ToolContext, plan: NativeFilePlan) -> Result<Self> {
        super::mutation_output::check_stop(ctx)?;
        let mut context = ctx.clone();
        bind(&mut context, None)?;
        let policy = context
            .invocation
            .native_files
            .clone()
            .context("Missing native policy")?;
        let worker_context = context.clone();
        let permit = tokio::task::spawn_blocking(move || {
            super::mutation_output::check_stop(&worker_context)?;
            let permit = policy.acquire(&plan)?;
            super::mutation_output::check_stop(&worker_context)?;
            Ok::<_, anyhow::Error>(permit)
        })
        .await??;
        Ok(Self {
            permit: Arc::new(Mutex::new(permit)),
            context,
        })
    }
    async fn with<T: Send + 'static>(
        &self,
        action: impl FnOnce(&mut dyn NativeFilePermit) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let permit = self.permit.clone();
        let context = self.context.clone();
        tokio::task::spawn_blocking(move || {
            super::mutation_output::check_stop(&context)?;
            let mut permit = permit
                .lock()
                .map_err(|_| anyhow::anyhow!("Native mutation owner panicked"))?;
            super::mutation_output::check_stop(&context)?;
            action(permit.as_mut())
        })
        .await?
    }
    pub async fn read_optional(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        let path = path.to_owned();
        self.with(move |p| p.read(&path)).await
    }
    pub async fn read(&self, path: &Path) -> Result<String> {
        let bytes = self
            .read_optional(path)
            .await?
            .context("File does not exist")?;
        Ok(String::from_utf8(bytes)?)
    }
    pub async fn write(&self, path: &Path, text: &str) -> Result<()> {
        let path = path.to_owned();
        let bytes = text.as_bytes().to_vec();
        self.with(move |p| p.write(&path, &bytes)).await
    }
    pub async fn remove(&self, path: &Path) -> Result<()> {
        let path = path.to_owned();
        self.with(move |p| p.remove(&path)).await
    }
    pub async fn same_file(&self, first: &Path, second: &Path) -> Result<bool> {
        let first = first.to_owned();
        let second = second.to_owned();
        self.with(move |p| p.same_file(&first, &second)).await
    }
    pub async fn copy_metadata(&self, source: &Path, destination: &Path) -> Result<()> {
        let source = source.to_owned();
        let destination = destination.to_owned();
        self.with(move |p| p.copy_metadata(&source, &destination))
            .await
    }
}

#[cfg(test)]
pub(super) fn fixture_context(mut context: ToolContext) -> ToolContext {
    struct FixturePolicy(String);
    impl NativeFilePolicy for FixturePolicy {
        fn session_id(&self) -> &str {
            &self.0
        }
        fn acquire(&self, plan: &NativeFilePlan) -> Result<Box<dyn NativeFilePermit>> {
            let resolved = plan
                .paths()
                .iter()
                .map(|p| Ok((p.clone(), resolve_target(p)?)))
                .collect::<Result<Vec<_>>>()?;
            unmanaged_permit(&resolved, plan)
        }
    }
    context.invocation.native_files = Some(Arc::new(FixturePolicy(context.session_id.clone())));
    context
}
