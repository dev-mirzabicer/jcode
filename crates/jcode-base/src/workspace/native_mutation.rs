//! Native admission snapshots catalog authority once. Its short permits retain
//! physical ownership; later revocation blocks new admissions, not completed bytes.
use super::*;
use crate::location::native_files::{
    VerifiedDirectory, VerifiedFiles, resolve_removal_entry, resolve_target,
};
use crate::location::{ProjectFacts, resolve_project};
use anyhow::{Context, ensure};
use jcode_tool_core::native_files::{NativeFilePermit, NativeFilePlan};
use std::collections::{BTreeMap, BTreeSet};

pub struct WorkspaceMutationPermit {
    service: WorkspaceService,
    catalog_revision: Revision,
    pub session_revision: Revision,
    files: VerifiedFiles,
    roots: Vec<RootGuard>,
    destinations: Vec<(PathBuf, usize)>,
    _leases: Vec<RootLease>,
}
struct RootGuard {
    directory: VerifiedDirectory,
    path: PathBuf,
    facts: ProjectFacts,
}
impl RootGuard {
    fn new(root: &Location) -> anyhow::Result<Self> {
        let facts = resolve_project(&root.observed_path)?;
        if let LocationKind::Checkout {
            common_directory, ..
        } = &root.kind
        {
            ensure!(
                facts.active_root() == root.observed_path
                    && facts.key().canonical_identity_path() == common_directory,
                "Checkout {} Git metadata changed; review its binding",
                root.id
            );
        }
        Ok(Self {
            directory: VerifiedDirectory::open(root.observed_path.clone())?,
            path: root.observed_path.clone(),
            facts,
        })
    }
    fn verify(&self) -> anyhow::Result<()> {
        self.directory.verify()?;
        ensure!(
            resolve_project(&self.path)? == self.facts,
            "Git identity changed during native mutation admission"
        );
        Ok(())
    }
    fn validate_nested(&self, path: &Path) -> anyhow::Result<()> {
        if path == self.path {
            return self.directory.verify();
        }
        let parent = path.parent().context("Native destination has no parent")?;
        let suffix = parent.strip_prefix(&self.path)?;
        let mut directory = self.path.clone();
        let mut owner = self
            .facts
            .key()
            .is_git()
            .then(|| self.facts.active_root().to_path_buf());
        for component in suffix.components() {
            directory.push(component);
            match std::fs::symlink_metadata(&directory) {
                Ok(metadata) => {
                    ensure!(
                        metadata.is_dir(),
                        "Native ancestor changed to a non-directory"
                    );
                    self.directory.verify_volume(&directory)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => return Err(error.into()),
            }
            match std::fs::symlink_metadata(directory.join(".git")) {
                Ok(_) => {
                    let nested = resolve_project(&directory)?;
                    ensure!(
                        nested.active_root() == directory
                            && owner.is_some()
                            && nested.superproject_root()? == owner,
                        "Unregistered independent Git root {} requires explicit root selection",
                        directory.display()
                    );
                    owner = Some(directory.clone());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        if path.try_exists()? {
            self.directory.verify_volume(path)?;
        }
        Ok(())
    }
}

impl WorkspaceService {
    pub fn acquire_native_mutation(
        &self,
        session: &crate::session::Session,
        paths: &[PathBuf],
    ) -> anyhow::Result<WorkspaceMutationPermit> {
        self.acquire_native_mutation_inner(
            session,
            &NativeFilePlan::new(paths.to_vec(), Vec::new()),
            None,
        )
    }
    pub fn acquire_native_operation(
        &self,
        session: &crate::session::Session,
        plan: &NativeFilePlan,
    ) -> anyhow::Result<WorkspaceMutationPermit> {
        self.acquire_native_mutation_inner(session, plan, None)
    }
    pub fn acquire_child_native_mutation(
        &self,
        parent: &crate::session::Session,
        child: &crate::session::Session,
        plan: &NativeFilePlan,
    ) -> anyhow::Result<WorkspaceMutationPermit> {
        let identity = &child
            .isolated_child
            .as_ref()
            .context("Missing isolated child identity")?
            .identity;
        ensure!(
            identity.original_parent == parent.id,
            "Child workspace authority belongs to its original parent"
        );
        let artifacts = identity.artifact_dir.canonicalize()?;
        ensure!(
            artifacts == identity.artifact_dir
                && !std::fs::symlink_metadata(&artifacts)?
                    .file_type()
                    .is_symlink(),
            "Child artifact root changed"
        );
        self.acquire_native_mutation_inner(parent, plan, Some(artifacts))
    }
    fn acquire_native_mutation_inner(
        &self,
        session: &crate::session::Session,
        plan: &NativeFilePlan,
        artifacts: Option<PathBuf>,
    ) -> anyhow::Result<WorkspaceMutationPermit> {
        let paths = plan.paths();
        let removals = plan.removals();
        let location = session
            .location
            .as_ref()
            .context("Legacy Session needs reviewed placement before native work")?;
        let _catalog = self.lease(false)?;
        let connection = self.connection()?;
        let transaction = connection.unchecked_transaction()?;
        let scope = self.scope_snapshot_mode(&transaction, &session.id, location, false)?;
        let all_roots = scope::locations(&transaction)?;
        let mut selected = BTreeMap::new();
        let resolved = paths
            .iter()
            .map(|path| Ok((path.clone(), resolve_target(path)?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut scope_paths = resolved
            .iter()
            .map(|(_, target)| target.clone())
            .collect::<Vec<_>>();
        for path in removals {
            scope_paths.push(resolve_removal_entry(path)?);
        }
        let mut destination_roots = Vec::new();
        self.reject_closeout_control_paths(&transaction, None, &scope_paths)?;
        for target in scope_paths {
            if artifacts
                .as_ref()
                .is_some_and(|root| target.starts_with(root) && target != *root)
            {
                destination_roots.push((target, None));
                continue;
            }
            // Catalog boundaries include inaccessible/closed roots. A parent prefix
            // must never bypass a deeper independent registered root.
            let root = all_roots
                .iter()
                .filter(|root| target.starts_with(&root.observed_path))
                .max_by_key(|root| {
                    (
                        root.observed_path.components().count(),
                        root.lifecycle != LocationLifecycle::Closed && !root.retired,
                    )
                })
                .with_context(|| {
                    format!(
                        "PermissionRequired: no registered writable root for {}",
                        target.display()
                    )
                })?;
            ensure!(
                scope
                    .roots
                    .iter()
                    .any(|allowed| allowed.location.id == root.id),
                "PermissionRequired: Session {} cannot write location {} ({})",
                session.id,
                root.id,
                root.observed_path.display()
            );
            if let std::collections::btree_map::Entry::Vacant(entry) = selected.entry(root.id) {
                entry.insert((root.clone(), self.verify_writable_root(&transaction, root)?));
            }
            destination_roots.push((target, Some(root.id)));
        }
        transaction.commit()?;
        let mut roots = Vec::new();
        let mut leases = Vec::new();
        let mut indexes = BTreeMap::new();
        let mut physical = BTreeSet::new();
        for (id, (root, binding)) in selected {
            if physical.insert(storage::physical_key(&binding)?) {
                leases.push(self.acquire_mutation_binding(&binding)?);
            }
            indexes.insert(id, roots.len());
            self.checkpoint("native_before_root_pin")?;
            let guard = RootGuard::new(&root)?;
            let resolved = self.resolver.resolve_directory(&binding)?;
            ensure!(
                !resolved.relocated,
                "Native root moved while admission was being pinned"
            );
            guard.verify()?;
            roots.push(guard);
        }
        let artifact_index = if let Some(path) = artifacts {
            let index = roots.len();
            roots.push(RootGuard {
                directory: VerifiedDirectory::open(path.clone())?,
                facts: resolve_project(&path)?,
                path,
            });
            Some(index)
        } else {
            None
        };
        let destinations = destination_roots
            .into_iter()
            .map(|(path, id)| {
                let index = match id {
                    Some(id) => *indexes.get(&id).context("Missing admitted root")?,
                    None => artifact_index.context("Missing child artifact ownership")?,
                };
                Ok((path.clone(), index))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        for (path, root) in &destinations {
            roots[*root].validate_nested(path)?;
        }
        let permit = WorkspaceMutationPermit {
            service: self.clone(),
            catalog_revision: scope.catalog_revision,
            session_revision: location.revision,
            files: VerifiedFiles::acquire_plan(&resolved, plan)?,
            roots,
            destinations,
            _leases: leases,
        };
        permit.confirm_admission()?;
        Ok(permit)
    }
}
impl WorkspaceMutationPermit {
    /// Used after the caller rechecks its authoritative Session revision. No
    /// transaction or Session mutex is held during later filesystem effects.
    pub fn confirm_admission(&self) -> anyhow::Result<()> {
        ensure!(
            self.service.status()?.revision == self.catalog_revision,
            "Workspace policy changed during native mutation admission; retry with current scope"
        );
        self.verify_physical()
    }
    fn verify_physical(&self) -> anyhow::Result<()> {
        for root in &self.roots {
            root.verify()?;
        }
        for (path, root) in &self.destinations {
            self.roots[*root].validate_nested(path)?;
        }
        Ok(())
    }
}
impl NativeFilePermit for WorkspaceMutationPermit {
    fn read(&mut self, path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
        self.verify_physical()?;
        self.files.read(path)
    }
    fn write(&mut self, path: &Path, contents: &[u8]) -> anyhow::Result<()> {
        self.verify_physical()?;
        self.files.write(path, contents)
    }
    fn remove(&mut self, path: &Path) -> anyhow::Result<()> {
        self.verify_physical()?;
        self.files.remove(path)
    }
    fn same_file(&self, a: &Path, b: &Path) -> anyhow::Result<bool> {
        self.files.same_file(a, b)
    }
    fn copy_metadata(&mut self, source: &Path, destination: &Path) -> anyhow::Result<()> {
        self.verify_physical()?;
        self.files.copy_metadata(source, destination)
    }
}
