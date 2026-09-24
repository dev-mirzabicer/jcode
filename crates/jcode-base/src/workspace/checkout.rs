//! Reviewed independent checkout operations. Catalog intent precedes every filesystem effect.
use super::*;
use crate::location::volume::{CheckoutDestination, PathBinding, VolumeIdentity};
use jcode_tool_core::OutputCapture;
use rusqlite::{TransactionBehavior, params};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::path::Component;

pub(super) mod git;
#[cfg(test)]
mod lfs_test_server;
mod materialize;
#[cfg(test)]
mod recovery_tests;
mod remotes;
mod source_use;
mod trust;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedClone {
    review: CloneReview,
    destination: PathBinding,
    source_binding: Option<PhysicalBinding>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloneOperation {
    public: CloneRecord,
    destination: PathBinding,
    source_binding: Option<PhysicalBinding>,
    stage_binding: Option<PhysicalBinding>,
    published_binding: Option<PhysicalBinding>,
    acquired: bool,
    #[serde(default)]
    acquired_refs: Option<Vec<AcquiredRef>>,
    #[serde(default)]
    content_materialized: bool,
    materialized: bool,
    backup_pending: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcquiredRef {
    branch: String,
    oid: String,
}

struct CloneLease {
    _catalog: CatalogLease,
    _file: File,
}

impl WorkspaceService {
    pub fn volumes(&self) -> Result<Vec<WorkspaceVolume>> {
        Ok(self
            .resolver
            .mounted_volumes()
            .map_err(io)?
            .into_iter()
            .map(|volume| WorkspaceVolume {
                uuid: volume.identity.as_str().into(),
                mount: volume.mount,
                label: volume.label,
                internal: volume.internal,
                writable: volume.writable,
                available_bytes: volume.available_bytes,
            })
            .collect())
    }
    pub fn review_clone(&self, expected: Revision, mut spec: CloneSpec) -> Result<CloneReview> {
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        organization::require_revision(&connection, expected)?;
        let project = organization::home_project(&connection, spec.home)?;
        organization::association(&connection, spec.home, spec.repository, false)?;
        spec.name = checked_name(&spec.name)?;
        if matches!(&spec.branch, CloneBranch::KeepName)
            && !matches!(&spec.base, CloneBase::Branch { .. })
        {
            return Err(issue(
                IssueCode::InvalidInput,
                "Keeping a branch name requires a selected branch",
            ));
        }
        if let CloneBranch::Create { name } = &spec.branch {
            git::validate_branch(name)?;
        }
        let source_binding = match &mut spec.source {
            CloneSource::Local { path } => {
                let binding = self.resolver.bind_directory(path).map_err(io)?;
                let facts =
                    crate::location::resolve_project(binding.observed_path()).map_err(io)?;
                if !facts.key().is_git() || facts.active_root() != binding.observed_path() {
                    return Err(issue(
                        IssueCode::InvalidInput,
                        "Local source must be an actual non-bare checkout root",
                    ));
                }
                *path = binding.observed_path().to_path_buf();
                Some(binding)
            }
            CloneSource::Remote { url } => {
                *url = checked_git_url(url)?;
                None
            }
        };
        let mut names = std::collections::HashSet::new();
        for remote in &mut spec.remotes {
            remote.name = checked_remote_name(&remote.name)?;
            remote.url = checked_git_url(&remote.url)?;
            if !names.insert(remote.name.clone()) {
                return Err(issue(
                    IssueCode::InvalidInput,
                    "Duplicate resulting remote name",
                ));
            }
        }
        if spec.remotes.is_empty()
            && let CloneSource::Remote { url } = &spec.source
        {
            spec.remotes.push(CloneRemote {
                name: "origin".into(),
                url: url.clone(),
            });
        }
        for url in &spec.trusted_submodule_urls {
            trust::validate_submodule_url(url)?;
        }
        for url in &spec.trusted_lfs_urls {
            checked_git_url(url)?;
        }
        let (volume, choice) = match &spec.destination {
            CloneDestination::Default {
                volume_uuid,
                project_component,
                checkout_component,
            } => {
                let volume = VolumeIdentity::parse(volume_uuid.clone()).map_err(io)?;
                let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
                    issue(
                        IssueCode::InvalidInput,
                        "HOME is needed for the internal default checkout path",
                    )
                })?;
                let saved: Option<String> = connection
                    .query_row(
                        "SELECT body FROM volume_defaults WHERE volume=?1",
                        [volume.as_str()],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(io)?;
                let saved: Option<PathBinding> = saved.as_deref().map(decode).transpose()?;
                let bound = self
                    .resolver
                    .checkout_destination(
                        &volume,
                        CheckoutDestination::Default {
                            home: &home,
                            project_component,
                            checkout_component,
                            saved_base: saved.as_ref(),
                        },
                    )
                    .map_err(io)?;
                (volume, bound)
            }
            CloneDestination::Custom { volume_uuid, path } => {
                let volume = VolumeIdentity::parse(volume_uuid.clone()).map_err(io)?;
                let bound = self
                    .resolver
                    .checkout_destination(&volume, CheckoutDestination::Custom(path))
                    .map_err(io)?;
                (volume, bound)
            }
        };
        let source_commit = git::resolve_base(&spec.source, &spec.base)?;
        if let CloneSource::Local { path } = &spec.source
            && choice.observed_path().starts_with(path)
        {
            return Err(issue(
                IssueCode::InvalidInput,
                "Clone destination cannot be inside its local source",
            ));
        }
        let Entity::Project(project_entity) = entity(&connection, EntityId::Project(project))?
        else {
            unreachable!()
        };
        if project_entity.state == OrganizationState::Retired {
            return Err(issue(
                IssueCode::InvalidIdentity,
                "Clone project is retired",
            ));
        }
        let review = CloneReview {
            id: ReviewId::new(),
            revision: expected,
            spec,
            source_commit,
            destination: choice.observed_path(),
            volume_uuid: volume.as_str().to_owned(),
            issues: Vec::new(),
        };
        let prepared = PreparedClone {
            review: review.clone(),
            destination: choice,
            source_binding,
        };
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        organization::require_revision(&transaction, expected)?;
        transaction
            .execute(
                "INSERT INTO reviews VALUES(?1,'clone',?2)",
                params![review.id.to_string(), encode(&prepared)?],
            )
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        Ok(review)
    }

    pub fn begin_clone(&self, request: RequestId, review: ReviewId) -> Result<CloneRecord> {
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        if let Some(operation) = read_clone(&connection, request)? {
            if operation.public.review.id != review {
                return Err(issue(
                    IssueCode::Conflict,
                    "Clone request ID belongs to another review",
                ));
            }
            return Ok(operation.public);
        }
        let prepared: PreparedClone = organization::read_review(&connection, review, "clone")?;
        organization::require_revision(&connection, prepared.review.revision)?;
        let destination = self
            .resolver
            .resolve_destination(&prepared.destination)
            .map_err(io)?;
        if destination.path != prepared.review.destination || destination.relocated {
            return Err(issue(
                IssueCode::ReplacedRoot,
                "Clone destination changed since review",
            ));
        }
        if let Some(source) = &prepared.source_binding {
            self.resolver.resolve_directory(source).map_err(io)?;
        }
        if git::resolve_base(&prepared.review.spec.source, &prepared.review.spec.base)?
            != prepared.review.source_commit
        {
            return Err(issue(
                IssueCode::Conflict,
                "Selected source ref changed since clone review",
            ));
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        if let Some(operation) = read_clone(&transaction, request)? {
            if operation.public.review.id != review {
                return Err(issue(
                    IssueCode::Conflict,
                    "Clone request ID belongs to another review",
                ));
            }
            return Ok(operation.public);
        }
        organization::require_revision(&transaction, prepared.review.revision)?;
        let reserved: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM operations WHERE kind='checkout_clone' AND json_extract(body,'$.public.review.destination')=?1 AND state!='complete' AND json_extract(body,'$.public.state')!='cancelled')",
            [prepared.review.destination.to_string_lossy().as_ref()], |row| row.get(0),
        ).map_err(io)?;
        if reserved {
            return Err(issue(
                IssueCode::Conflict,
                "Another checkout operation already reserves this destination",
            ));
        }
        let location = LocationId::new();
        let revision = prepared
            .review
            .revision
            .checked_add(1)
            .ok_or_else(|| corrupt("Catalog revision exhausted"))?;
        let public = CloneRecord {
            operation: request.to_string().parse().map_err(corrupt)?,
            request,
            location,
            review: prepared.review,
            state: CloneState::Pending,
            cancel_requested: false,
            stage: None,
            output_runs: Vec::new(),
            discovered_sources: Vec::new(),
            pending_trust: Vec::new(),
            trust_approvals: Vec::new(),
            output_issue: None,
            issue: None,
            revision,
        };
        let operation = CloneOperation {
            public: public.clone(),
            destination: prepared.destination,
            source_binding: prepared.source_binding,
            stage_binding: None,
            published_binding: None,
            acquired: false,
            acquired_refs: None,
            content_materialized: false,
            materialized: false,
            backup_pending: false,
        };
        transaction.execute("INSERT INTO operations(id,kind,state,body) VALUES(?1,'checkout_clone','pending',?2)", params![public.operation.to_string(), encode(&operation)?]).map_err(io)?;
        transaction
            .execute(
                "UPDATE catalog SET revision=revision+1 WHERE singleton=1",
                [],
            )
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        self.checkpoint("clone_intent")?;
        Ok(public)
    }

    pub fn inspect_clone(&self, request: RequestId) -> Result<CloneRecord> {
        let _lease = self.lease(false)?;
        Ok(read_clone(&self.connection()?, request)?
            .ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown checkout clone request"))?
            .public)
    }

    pub fn finish_clone_backup(&self, request: RequestId) -> Result<CloneRecord> {
        let operation = self.load_clone(request)?;
        if operation.public.state != CloneState::Ready || !operation.backup_pending {
            return Ok(operation.public);
        }
        let outcome = self.automatic_backup();
        self.update_clone(request, |op| {
            if op.public.state != CloneState::Ready {
                return Err(issue(IssueCode::Conflict, "Checkout changed during backup"));
            }
            match outcome {
                Ok(()) => {
                    op.backup_pending = false;
                    op.public.issue = None;
                }
                Err(ref problem) => {
                    op.public.issue = Some(issue(
                        IssueCode::BackupFailed,
                        format!("Checkout is Ready, but catalog backup is pending: {problem}"),
                    ))
                }
            }
            Ok(())
        })
    }

    pub fn request_clone_cancel(&self, request: RequestId) -> Result<CloneRecord> {
        let idle = match self.clone_lease(request) {
            Ok(lease) => Some(lease),
            Err(error) if error.code == IssueCode::Busy => None,
            Err(error) => return Err(error),
        };
        let record = self.update_clone(request, |op| {
            if op.public.state == CloneState::Ready || op.public.state == CloneState::Cancelled {
                return Ok(());
            }
            if op.published_binding.is_some() || op.public.state == CloneState::Publishing {
                return Err(issue(IssueCode::Conflict,
                    "Clone already reached physical publication; reconcile it instead of cancelling"));
            }
            op.public.cancel_requested = true;
            if idle.is_some() {
                op.public.state = CloneState::Cancelled;
                op.public.issue = None;
            }
            Ok(())
        })?;
        if idle.is_some() && record.state == CloneState::Cancelled {
            self.cleanup_cancelled_empty_stage(request)?;
            self.inspect_clone(request)
        } else {
            Ok(record)
        }
    }

    pub fn record_clone_output(&self, request: RequestId, run: String) -> Result<CloneRecord> {
        if !run.starts_with("run-") || run.len() > 128 {
            return Err(issue(
                IssueCode::InvalidInput,
                "Invalid retained output identity",
            ));
        }
        self.update_clone(request, |op| {
            if matches!(
                op.public.state,
                CloneState::Ready | CloneState::Cancelled | CloneState::AwaitingTrust
            ) {
                return Err(issue(
                    IssueCode::Conflict,
                    "Completed or trust-paused checkout cannot start another execution run",
                ));
            }
            if !op.public.output_runs.contains(&run) {
                op.public.output_runs.push(run);
            }
            Ok(())
        })
    }

    pub fn record_clone_launch_failure(
        &self,
        request: RequestId,
        problem: Issue,
    ) -> Result<CloneRecord> {
        self.update_clone(request, |op| {
            if op.public.state == CloneState::Pending {
                op.public.state = CloneState::PreparationFailed;
                op.public.issue = Some(problem);
            } else if op.public.state == CloneState::Ready {
                op.public.output_issue = Some(issue(
                    IssueCode::RecoveryRequired,
                    format!(
                        "Checkout is Ready but its retained output could not be sealed: {}",
                        problem.detail
                    ),
                ));
            }
            Ok(())
        })
    }

    pub fn clone_cancelled(&self, request: RequestId) -> Result<bool> {
        let _lease = self.lease(false)?;
        Ok(read_clone(&self.connection()?, request)?
            .ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown checkout clone"))?
            .public
            .cancel_requested)
    }

    fn clone_lease(&self, request: RequestId) -> Result<CloneLease> {
        let catalog = self.lease(false)?;
        storage::private_dir(&self.root.join("leases"))?;
        let file = storage::private_file(
            &self
                .root
                .join("leases")
                .join(format!("clone-{request}.lock")),
            false,
        )?;
        file.try_lock().map_err(|e| {
            issue(
                IssueCode::Busy,
                format!("Checkout clone is already running: {e}"),
            )
        })?;
        Ok(CloneLease {
            _catalog: catalog,
            _file: file,
        })
    }

    fn load_clone(&self, request: RequestId) -> Result<CloneOperation> {
        let _lease = self.lease(false)?;
        read_clone(&self.connection()?, request)?
            .ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown checkout clone"))
    }

    /// The caller owns one detached runtime task and a retained execution sink.
    /// A process crash leaves the catalog/stage for explicit reconciliation; this
    /// method never replays an unproven Git effect on a later request.
    pub async fn execute_clone(
        &self,
        request: RequestId,
        capture: &dyn OutputCapture,
    ) -> Result<CloneRecord> {
        let _lease = self.clone_lease(request)?;
        let operation = self.load_clone(request)?;
        if operation.public.state == CloneState::Ready {
            return self.finish_clone_backup(request);
        }
        if matches!(operation.public.state, CloneState::Cancelled) {
            return Err(issue(
                IssueCode::Conflict,
                "Cancelled clone needs a fresh review",
            ));
        }
        if matches!(operation.public.state, CloneState::AwaitingTrust) {
            return Err(issue(
                IssueCode::PermissionRequired,
                "Review the discovered submodule/LFS sources before resuming this clone",
            ));
        }
        let outcome = self.execute_clone_owned(request, capture).await;
        match outcome {
            Ok(()) => self.inspect_clone(request),
            Err(problem) => {
                let retained = self.update_clone(request, |operation| {
                    if operation.public.state != CloneState::Ready {
                        operation.public.state = if operation.public.cancel_requested {
                            CloneState::Cancelled
                        } else if operation.public.state == CloneState::AwaitingTrust {
                            CloneState::AwaitingTrust
                        } else if operation.published_binding.is_some() {
                            CloneState::RecoveryRequired
                        } else {
                            CloneState::PreparationFailed
                        };
                        operation.public.issue = Some(problem.clone());
                    }
                    Ok(())
                });
                retained?;
                if self.inspect_clone(request)?.state == CloneState::Cancelled {
                    self.cleanup_cancelled_empty_stage(request)?;
                }
                Err(problem)
            }
        }
    }

    async fn execute_clone_owned(
        &self,
        request: RequestId,
        capture: &dyn OutputCapture,
    ) -> Result<()> {
        let operation = self.load_clone(request)?;
        let parent = if let Some(stage) = &operation.stage_binding
            && let Ok(published) = self
                .resolver
                .bind_directory(&operation.public.review.destination)
            && published.root_witness() == stage.root_witness()
            && published.volume() == stage.volume()
        {
            let parent_path = operation
                .public
                .review
                .destination
                .parent()
                .ok_or_else(|| corrupt("Clone destination has no parent"))?;
            self.resolver.bind_directory(parent_path).map_err(io)?
        } else {
            self.prepare_clone_parent(&operation.destination)?
        };
        // Child creation/publication uses exclusive names and no-replacement
        // rename. Keep the parent alive without excluding source readers or
        // ordinary work sharing this directory.
        let _root = self.acquire_mutation_binding(&parent)?;
        let final_path = operation.public.review.destination.clone();
        if final_path.parent() != Some(parent.observed_path()) {
            return Err(issue(
                IssueCode::ReplacedRoot,
                "Clone parent changed after review",
            ));
        }
        let stage_name = format!(".jcode-clone-{}", operation.public.operation);
        let stage_path = parent.observed_path().join(&stage_name);
        let stage = if let Some(stage) = operation.stage_binding {
            let resolved = self.resolver.resolve_directory(&stage);
            if resolved.is_err() {
                // A rename can precede its journal receipt. Only the exact
                // witness at the reviewed final target is eligible for repair.
                if let Ok(target) = self.resolver.bind_directory(&final_path)
                    && target.root_witness() == stage.root_witness()
                    && target.volume() == stage.volume()
                {
                    self.update_clone(request, |op| {
                        op.published_binding = Some(target);
                        op.public.state = CloneState::Publishing;
                        Ok(())
                    })?;
                    return self.publish_clone(request);
                }
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    "Stage is missing or replaced; inspect the retained operation and final target",
                ));
            }
            if resolved.map_err(io)?.path != stage_path {
                return Err(issue(
                    IssueCode::ReplacedRoot,
                    "Stage moved since acquisition",
                ));
            }
            stage
        } else {
            if std::fs::symlink_metadata(&stage_path).is_ok() {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    format!(
                        "Unwitnessed stage at {} must be inspected before retry",
                        stage_path.display()
                    ),
                ));
            }
            self.check_cancel(request)?;
            let stage = self
                .resolver
                .create_empty_child(&parent, OsStr::new(&stage_name))
                .map_err(io)?;
            self.update_clone(request, |op| {
                op.stage_binding = Some(stage.clone());
                op.public.stage = Some(stage.observed_path().to_path_buf());
                op.public.state = CloneState::Acquiring;
                Ok(())
            })?;
            self.checkpoint("clone_stage_bound")?;
            stage
        };
        let mut operation = self.load_clone(request)?;
        if !operation.acquired {
            if std::fs::read_dir(stage.observed_path())
                .map_err(io)?
                .next()
                .is_some()
            {
                // No automatic delete or second clone over a stage containing
                // possible user work after an interrupted Git command.
                self.verify_acquired(&operation, &stage)?;
            } else {
                self.check_cancel(request)?;
                let local_source = match &operation.public.review.spec.source {
                    CloneSource::Local { path } => Some(path.clone()),
                    CloneSource::Remote { url } if url.starts_with("file://") => Some(
                        url::Url::parse(url)
                            .map_err(io)?
                            .to_file_path()
                            .map_err(|_| {
                                issue(
                                    IssueCode::InvalidInput,
                                    "Local Git URL does not identify a filesystem path",
                                )
                            })?,
                    ),
                    _ => None,
                };
                let _source_use = local_source
                    .as_deref()
                    .map(|path| self.acquire_source_path(path))
                    .transpose()?;
                self.checkpoint("clone_source_admitted")?;
                self.verify_source(&operation)?;
                let template = self.root.join("clone-empty-template");
                storage::private_dir(&template)?;
                let source: OsString = match &operation.public.review.spec.source {
                    CloneSource::Remote { url } => url.into(),
                    CloneSource::Local { path } => path.as_os_str().into(),
                };
                let local_transport = matches!(
                    &operation.public.review.spec.source,
                    CloneSource::Local { .. }
                ) || matches!(&operation.public.review.spec.source, CloneSource::Remote { url } if url.starts_with("file://"));
                let mut args = Vec::new();
                if local_transport {
                    args.extend([
                        OsString::from("-c"),
                        OsString::from("protocol.file.allow=always"),
                    ]);
                }
                args.extend([
                    OsString::from("clone"),
                    OsString::from("--no-local"),
                    OsString::from("--no-hardlinks"),
                    OsString::from("--no-checkout"),
                    OsString::from("--no-recurse-submodules"),
                    OsString::from(format!("--template={}", template.display())),
                    OsString::from("--"),
                    source,
                    stage.observed_path().as_os_str().into(),
                ]);
                let mut command = git::authorized(None, args, false)?;
                command.env("GIT_PROTOCOL_FROM_USER", "0");
                git::run(self, request, command, capture).await?;
                self.verify_acquired(&operation, &stage)?;
            }
            git::detach_borrowed_objects(stage.observed_path(), stage.observed_path())?;
            self.update_clone(request, |op| {
                op.acquired = true;
                op.public.state = CloneState::Materializing;
                Ok(())
            })?;
            self.checkpoint("clone_acquired")?;
        }
        operation = self.load_clone(request)?;
        if operation.acquired_refs.is_none() {
            remotes::verify_acquisition_origin(&operation, stage.observed_path())?;
            let snapshot = remotes::observe_acquired_refs(stage.observed_path())?;
            self.update_clone(request, |op| {
                op.acquired_refs = Some(snapshot);
                Ok(())
            })?;
            self.checkpoint("clone_refs_recorded")?;
            operation = self.load_clone(request)?;
        }
        if !operation.materialized {
            if !operation.content_materialized {
                remotes::verify_acquisition_origin(&operation, stage.observed_path())?;
                if !operation.public.trust_approvals.is_empty() {
                    trust::verify_source_observations(self, &operation, &stage)?;
                }
                self.materialize_clone_content(request, &operation, &stage, capture)
                    .await?;
                self.update_clone(request, |op| {
                    op.content_materialized = true;
                    Ok(())
                })?;
                self.checkpoint("clone_content_materialized")?;
                operation = self.load_clone(request)?;
            }
            self.configure_clone_remotes(request, &operation, &stage, capture)
                .await?;
            self.update_clone(request, |op| {
                op.materialized = true;
                op.public.state = CloneState::Verifying;
                Ok(())
            })?;
            self.checkpoint("clone_materialized")?;
        }
        self.verify_ready(&self.load_clone(request)?, &stage, capture)
            .await?;
        self.check_cancel(request)?;
        let marker = stage
            .observed_path()
            .join(".git")
            .join("jcode-clone-operation.json");
        storage::atomic_json(&marker, &request)?;
        let final_leaf = final_path
            .file_name()
            .ok_or_else(|| corrupt("Clone destination lost leaf"))?;
        let target = self
            .resolver
            .publish_empty_child(&parent, &stage, final_leaf)
            .map_err(io)?;
        self.checkpoint("clone_renamed")?;
        self.update_clone(request, |op| {
            op.published_binding = Some(target);
            op.public.state = CloneState::Publishing;
            Ok(())
        })?;
        self.publish_clone(request)
    }

    fn prepare_clone_parent(&self, destination: &PathBinding) -> Result<PhysicalBinding> {
        self.resolver.resolve_destination(destination).map_err(io)?;
        let mut current = destination.existing_ancestor().clone();
        let components: Vec<_> = destination.missing_suffix().components().collect();
        if components.is_empty() {
            return Err(issue(
                IssueCode::Conflict,
                "Clone destination already exists",
            ));
        }
        let _ancestor_lease = self.acquire_mutation_binding(&current)?;
        for component in components.iter().take(components.len() - 1) {
            let Component::Normal(name) = component else {
                return Err(corrupt("Invalid destination suffix"));
            };
            let child = current.observed_path().join(name);
            current = if child.is_dir() {
                self.resolver.bind_directory(&child).map_err(io)?
            } else {
                self.resolver
                    .create_empty_child(&current, name)
                    .map_err(io)?
            };
            if current.volume() != destination.volume() {
                return Err(issue(
                    IssueCode::OfflineVolume,
                    "Destination parent crossed a volume boundary",
                ));
            }
        }
        let observed = destination.observed_path();
        let parent = observed
            .parent()
            .ok_or_else(|| corrupt("Missing clone parent"))?;
        let verified = self.resolver.bind_directory(parent).map_err(io)?;
        if verified != current {
            return Err(issue(
                IssueCode::ReplacedRoot,
                "Clone parent changed during preparation",
            ));
        }
        Ok(current)
    }

    fn cleanup_cancelled_empty_stage(&self, request: RequestId) -> Result<()> {
        let operation = self.load_clone(request)?;
        let Some(stage) = &operation.stage_binding else {
            return Ok(());
        };
        let Some(stage_path) = &operation.public.stage else {
            return Err(corrupt("Clone stage has no path"));
        };
        if operation.public.state != CloneState::Cancelled || stage.observed_path() != stage_path {
            return Err(corrupt("Clone cancellation ownership mismatch"));
        }
        let parent = self
            .resolver
            .bind_directory(
                stage_path
                    .parent()
                    .ok_or_else(|| corrupt("Clone stage has no parent"))?,
            )
            .map_err(io)?;
        let _root = self.acquire_binding(&parent)?;
        if let Ok(resolved) = self.resolver.resolve_directory(stage)
            && !resolved.relocated
            && resolved.path == *stage_path
            && std::fs::read_dir(stage_path).map_err(io)?.next().is_none()
        {
            std::fs::remove_dir(stage_path).map_err(io)?;
            self.update_clone(request, |op| {
                op.stage_binding = None;
                op.public.stage = None;
                Ok(())
            })?;
        } else {
            self.update_clone(request, |op| {
                op.public.issue = Some(issue(IssueCode::RecoveryRequired,
                    format!("Clone cancelled. Its nonempty or changed stage is retained at {} for inspection; no user data was deleted", stage_path.display())));
                Ok(())
            })?;
        }
        Ok(())
    }

    fn check_cancel(&self, request: RequestId) -> Result<()> {
        if self.clone_cancelled(request)? {
            Err(issue(
                IssueCode::Busy,
                "Checkout clone cancelled before publication",
            ))
        } else {
            Ok(())
        }
    }

    fn verify_source(&self, operation: &CloneOperation) -> Result<()> {
        if let Some(binding) = &operation.source_binding {
            let current = self.resolver.resolve_directory(binding).map_err(io)?;
            if current.relocated || current.path != binding.observed_path() {
                return Err(issue(
                    IssueCode::ReplacedRoot,
                    "Local source relocated since review",
                ));
            }
        }
        if git::resolve_base(
            &operation.public.review.spec.source,
            &operation.public.review.spec.base,
        )? != operation.public.review.source_commit
        {
            return Err(issue(
                IssueCode::Conflict,
                "Source ref moved before acquisition",
            ));
        }
        Ok(())
    }

    fn verify_acquired(&self, operation: &CloneOperation, stage: &PhysicalBinding) -> Result<()> {
        let path = stage.observed_path();
        let git_dir = path.join(".git");
        if !std::fs::symlink_metadata(&git_dir)
            .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink())
        {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Incomplete or modified stage is retained for review; no acquisition is replayed",
            ));
        }
        if git_dir
            .join("objects/info/alternates")
            .try_exists()
            .map_err(io)?
        {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Clone has borrowed objects; stage retained, not published",
            ));
        }
        let oid = format!("{}^{{commit}}", operation.public.review.source_commit);
        let output = git::git(
            Some(path),
            [OsStr::new("cat-file"), OsStr::new("-e"), OsStr::new(&oid)],
        )
        .output()
        .map_err(io)?;
        if !output.status.success() {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Reviewed commit is missing from stage; no acquisition replay",
            ));
        }
        Ok(())
    }

    fn publish_clone(&self, request: RequestId) -> Result<()> {
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let operation =
            read_clone(&connection, request)?.ok_or_else(|| corrupt("Clone intent disappeared"))?;
        if operation.public.state == CloneState::Ready {
            self.finish_clone_backup(request)?;
            return Ok(());
        }
        let binding = operation.published_binding.as_ref().ok_or_else(|| {
            issue(
                IssueCode::RecoveryRequired,
                "Clone rename has no verified publication witness",
            )
        })?;
        let resolved = self.resolver.resolve_directory(binding).map_err(io)?;
        if resolved.relocated || resolved.path != operation.public.review.destination {
            return Err(issue(
                IssueCode::ReplacedRoot,
                "Published clone moved before catalog commit",
            ));
        }
        let marker: RequestId =
            storage::read_json(&resolved.path.join(".git/jcode-clone-operation.json"))?;
        if marker != request {
            return Err(issue(
                IssueCode::Conflict,
                "Published clone belongs to another operation",
            ));
        }
        let facts = crate::location::resolve_project(&resolved.path).map_err(io)?;
        let crate::location::ProjectKey::Git {
            canonical_common_dir,
        } = facts.key()
        else {
            return Err(corrupt("Published clone is not Git"));
        };
        if facts.active_root() != resolved.path || facts.is_linked_worktree().map_err(io)? {
            return Err(issue(
                IssueCode::RecoveryRequired,
                "Published clone has unexpected shared Git layout",
            ));
        }
        let _root = self.acquire_binding(binding)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        if read_clone(&transaction, request)?.is_some_and(|op| op.public.state == CloneState::Ready)
        {
            return Ok(());
        }
        organization::association(
            &transaction,
            operation.public.review.spec.home,
            operation.public.review.spec.repository,
            false,
        )?;
        let revision = storage::status(&transaction)?
            .revision
            .checked_add(1)
            .ok_or_else(|| corrupt("Catalog revision overflow"))?;
        let loc = Location {
            id: operation.public.location,
            name: operation.public.review.spec.name.clone(),
            home: Some(operation.public.review.spec.home),
            kind: LocationKind::Checkout {
                repository: operation.public.review.spec.repository,
                origin: CheckoutOrigin::ManagedClone,
                common_directory: canonical_common_dir.clone(),
            },
            observed_path: resolved.path.clone(),
            volume_uuid: binding.volume().as_str().to_owned(),
            binding_generation: binding.generation(),
            lifecycle: LocationLifecycle::Ready,
            retired: false,
            revision,
        };
        organization::save_entity(&transaction, &Entity::Location(loc))?;
        transaction
            .execute(
                "INSERT INTO bindings VALUES(?1,?2,?3)",
                params![
                    operation.public.location.to_string(),
                    encode(&BoundLocation {
                        binding: binding.clone()
                    })?,
                    storage::physical_key(binding)?
                ],
            )
            .map_err(|e| issue(IssueCode::Conflict, e.to_string()))?;
        transaction
            .execute(
                "INSERT INTO operation_targets VALUES(?1,?2)",
                params![
                    operation.public.operation.to_string(),
                    operation.public.location.to_string()
                ],
            )
            .map_err(io)?;
        let mut finished = operation;
        finished.public.state = CloneState::Ready;
        finished.public.issue = None;
        finished.public.revision = revision;
        finished.backup_pending = true;
        transaction
            .execute(
                "UPDATE operations SET state='complete',body=?2 WHERE id=?1",
                params![request.to_string(), encode(&finished)?],
            )
            .map_err(io)?;
        transaction
            .execute(
                "UPDATE catalog SET revision=revision+1 WHERE singleton=1",
                [],
            )
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        self.checkpoint("clone_catalog_committed")?;
        self.finish_clone_backup(request)?;
        Ok(())
    }

    fn update_clone(
        &self,
        request: RequestId,
        update: impl FnOnce(&mut CloneOperation) -> Result<()>,
    ) -> Result<CloneRecord> {
        let _lease = self.lease(false)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io)?;
        let mut operation = read_clone(&transaction, request)?
            .ok_or_else(|| issue(IssueCode::InvalidIdentity, "Unknown checkout clone request"))?;
        let prior = encode(&operation)?;
        update(&mut operation)?;
        if encode(&operation)? == prior {
            return Ok(operation.public);
        }
        let revision = storage::status(&transaction)?
            .revision
            .checked_add(1)
            .ok_or_else(|| corrupt("Catalog revision exhausted"))?;
        operation.public.revision = revision;
        let state = match operation.public.state {
            CloneState::Ready => "complete",
            CloneState::PreparationFailed | CloneState::Cancelled => "failed",
            CloneState::RecoveryRequired => "recovery_required",
            _ => "pending",
        };
        transaction
            .execute(
                "UPDATE operations SET state=?2,body=?3 WHERE id=?1",
                params![
                    operation.public.operation.to_string(),
                    state,
                    encode(&operation)?
                ],
            )
            .map_err(io)?;
        transaction
            .execute(
                "UPDATE catalog SET revision=revision+1 WHERE singleton=1",
                [],
            )
            .map_err(io)?;
        transaction.commit().map_err(io)?;
        Ok(operation.public)
    }
}

fn read_clone(connection: &Connection, request: RequestId) -> Result<Option<CloneOperation>> {
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT body,state FROM operations WHERE id=?1 AND kind='checkout_clone'",
            [request.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(io)?;
    row.map(|(body, state)| {
        let operation: CloneOperation = decode(&body)?;
        let consistent = matches!(
            (state.as_str(), operation.public.state),
            ("complete", CloneState::Ready)
                | (
                    "failed",
                    CloneState::Cancelled | CloneState::PreparationFailed
                )
                | ("recovery_required", CloneState::RecoveryRequired)
                | (
                    "pending",
                    CloneState::Pending
                        | CloneState::Acquiring
                        | CloneState::Materializing
                        | CloneState::AwaitingTrust
                        | CloneState::Verifying
                        | CloneState::Publishing
                )
        );
        if !consistent {
            return Err(corrupt(
                "Checkout SQL state disagrees with its typed operation journal",
            ));
        }
        Ok(operation)
    })
    .transpose()
    .and_then(|operation| {
        if operation.as_ref().is_some_and(|op| {
            op.public.request != request || op.public.operation.to_string() != request.to_string()
        }) {
            return Err(corrupt("Clone operation identity changed"));
        }
        Ok(operation)
    })
}

fn checked_name(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(issue(
            IssueCode::InvalidInput,
            "Checkout name must be nonempty and contain no control characters",
        ));
    }
    Ok(value.to_string())
}
fn checked_remote_name(value: &str) -> Result<String> {
    if value.is_empty()
        || value.starts_with('-')
        || !value
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || b"._-".contains(&v))
    {
        return Err(issue(
            IssueCode::InvalidInput,
            "Remote name must use letters, digits, '.', '_' or '-'",
        ));
    }
    Ok(value.to_string())
}
fn checked_git_url(value: &str) -> Result<String> {
    let cleaned = organization::clean_remotes(&[value.to_string()])?.remove(0);
    if cleaned.starts_with('-')
        || cleaned.contains('\n')
        || cleaned.contains('\r')
        || cleaned.contains('\0')
    {
        return Err(issue(
            IssueCode::InvalidInput,
            "Invalid clone transport reference",
        ));
    }
    if cleaned.contains("://") {
        let url = url::Url::parse(&cleaned).map_err(io)?;
        if !matches!(url.scheme(), "https" | "http" | "ssh" | "git" | "file") {
            return Err(issue(
                IssueCode::InvalidInput,
                "Unsupported Git clone transport",
            ));
        }
    } else if !cleaned.starts_with("git@") {
        return Err(issue(
            IssueCode::InvalidInput,
            "Use an absolute local path as a Local source, or a reviewed Git transport URL",
        ));
    }
    Ok(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{
        Capture, ExecutionStore, Invocation, PreparedInvocation, RunState, StorageConfig,
    };
    use jcode_tool_types::ToolOutput;

    fn command(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
    fn organization(service: &WorkspaceService, change: OrganizationChange) -> EntityId {
        let review = service
            .review_organization_change(service.status().unwrap().revision, change)
            .unwrap();
        service
            .apply_organization_change(RequestId::new(), review.id)
            .unwrap()
            .targets[0]
    }

    #[tokio::test]
    async fn reviewed_local_clone_is_independent_and_replays_one_publication() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let service = WorkspaceService::new(&state);
        service.initialize(RequestId::new()).unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        command(&source, &["init", "-b", "main"]);
        std::fs::write(source.join("tracked.txt"), "committed").unwrap();
        command(&source, &["add", "tracked.txt"]);
        command(
            &source,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-m",
                "source",
            ],
        );
        std::fs::write(source.join("dirty.txt"), "not committed").unwrap();
        let project = match organization(
            &service,
            OrganizationChange::CreateProject { name: "P".into() },
        ) {
            EntityId::Project(id) => id,
            _ => unreachable!(),
        };
        let repository = match organization(
            &service,
            OrganizationChange::CreateRepository {
                name: "R".into(),
                remotes: vec![],
            },
        ) {
            EntityId::Repository(id) => id,
            _ => unreachable!(),
        };
        organization(
            &service,
            OrganizationChange::AssociateRepository {
                project,
                repository,
            },
        );
        let destination = temp.path().join("checkout");
        let volume = service.resolver.containing_volume(temp.path()).unwrap();
        let review = service
            .review_clone(
                service.status().unwrap().revision,
                CloneSpec {
                    home: Home::Project(project),
                    repository,
                    name: "checkout".into(),
                    source: CloneSource::Local {
                        path: source.clone(),
                    },
                    base: CloneBase::Branch {
                        name: "main".into(),
                    },
                    branch: CloneBranch::Create {
                        name: "feature".into(),
                    },
                    remotes: vec![],
                    destination: CloneDestination::Custom {
                        volume_uuid: volume.identity.as_str().into(),
                        path: destination.clone(),
                    },
                    submodules: false,
                    lfs: false,
                    trusted_submodule_urls: vec![],
                    trusted_lfs_urls: vec![],
                },
            )
            .unwrap();
        let request = RequestId::new();
        let initial = service.begin_clone(request, review.id).unwrap();
        assert_eq!(initial, service.begin_clone(request, review.id).unwrap());
        let store = ExecutionStore::open(temp.path()).unwrap();
        let invocation = Invocation {
            session_id: "workspace".into(),
            message_id: request.to_string(),
            call_path: vec!["clone".into()],
            tool: "workspace_clone".into(),
            input: serde_json::json!({"request":request}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(run) = store.prepare(&invocation, "fixture").unwrap() else {
            panic!()
        };
        store.start(&run.id, "fixture").unwrap();
        let capture = Capture::create(store.clone(), run, StorageConfig::default()).unwrap();
        let ready = service.execute_clone(request, &capture).await.unwrap();
        let mut output = ToolOutput::new("");
        output.source = jcode_tool_types::OutputSource::Retained(capture.reference().unwrap());
        capture.seal(output, RunState::Completed).unwrap();
        assert_eq!(ready.state, CloneState::Ready);
        assert_eq!(ready, service.inspect_clone(request).unwrap());
        assert!(!destination.join("dirty.txt").exists());
        assert_eq!(
            std::fs::read_to_string(destination.join("tracked.txt")).unwrap(),
            "committed"
        );
        assert!(!destination.join(".git/objects/info/alternates").exists());
        assert_eq!(
            command(&destination, &["branch", "--show-current"]),
            "feature"
        );
        std::fs::rename(&source, temp.path().join("source-offline")).unwrap();
        command(&destination, &["fsck", "--full"]);
    }
}
