use super::types::{
    ActiveProject, LoadedStartupProjectPlan, ProjectKey, STARTUP_PROJECT_PLAN_SCHEMA_VERSION,
    StartupContextError, StartupFileSpec, StartupPlanLoadSource, StartupProjectPlan,
    StartupProjectPlanCommitOutcome, StartupProjectPlanTransition, StartupSelectionPreview,
};
use chrono::{DateTime, Utc};
use jcode_session_types::{StoredStartupFileSpec, StoredStartupProjectIdentity};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(super) struct StartupPlanStore {
    projects_dir: PathBuf,
    max_entries: usize,
}

impl StartupPlanStore {
    pub(super) fn new(projects_dir: PathBuf, max_entries: usize) -> Self {
        Self {
            projects_dir,
            max_entries,
        }
    }

    pub(super) fn load(
        &self,
        project: &ActiveProject,
    ) -> Result<LoadedStartupProjectPlan, StartupContextError> {
        let path = self.path_for(project.key());
        if !path.exists() {
            return Ok(LoadedStartupProjectPlan::new(
                StartupProjectPlan::empty(project.key().clone()),
                StartupPlanLoadSource::Missing,
            ));
        }
        match self.read_and_decode(&path, project.key()) {
            Ok(plan) => Ok(LoadedStartupProjectPlan::new(
                plan,
                StartupPlanLoadSource::Primary,
            )),
            Err(StoredPlanReadError::UnsupportedSchema(schema_version)) => {
                Err(StartupContextError::UnsupportedPlanSchema {
                    path,
                    schema_version,
                })
            }
            Err(StoredPlanReadError::Corrupt(primary_detail)) => {
                let backup_path = path.with_extension("bak");
                if !backup_path.exists() {
                    return Err(StartupContextError::PlanStorage {
                        path,
                        detail: primary_detail,
                    });
                }
                match self.read_and_decode(&backup_path, project.key()) {
                    Ok(plan) => {
                        restore_recovered_plan(&plan, &path)?;
                        Ok(LoadedStartupProjectPlan::new(
                            plan,
                            StartupPlanLoadSource::RecoveredBackup,
                        ))
                    }
                    Err(StoredPlanReadError::UnsupportedSchema(schema_version)) => {
                        Err(StartupContextError::UnsupportedPlanSchema {
                            path: backup_path,
                            schema_version,
                        })
                    }
                    Err(StoredPlanReadError::Corrupt(backup_detail)) => {
                        Err(StartupContextError::PlanStorage {
                            path,
                            detail: format!(
                                "primary plan is invalid ({primary_detail}); backup is also invalid ({backup_detail})"
                            ),
                        })
                    }
                }
            }
        }
    }

    pub(super) fn save(
        &self,
        project: &ActiveProject,
        expected_revision: u64,
        preview: &StartupSelectionPreview,
    ) -> Result<StartupProjectPlan, StartupContextError> {
        let transition = self.prepare_transition(project, expected_revision, preview)?;
        let _ = self.commit_transition(project, &transition)?;
        self.load(project).map(LoadedStartupProjectPlan::into_plan)
    }

    pub(super) fn prepare_transition(
        &self,
        project: &ActiveProject,
        expected_revision: u64,
        preview: &StartupSelectionPreview,
    ) -> Result<StartupProjectPlanTransition, StartupContextError> {
        if preview.project_key() != project.key() {
            return Err(StartupContextError::SelectionProjectMismatch);
        }
        if !preview.is_valid() {
            return Err(StartupContextError::InvalidSelection {
                issue_count: preview.issue_count(),
            });
        }

        let loaded = self.load(project)?;
        let current = loaded.plan();
        if current.revision() != expected_revision {
            return Err(StartupContextError::StalePlanRevision {
                expected: expected_revision,
                actual: current.revision(),
            });
        }

        let entries = preview
            .selected()
            .map(|selected| selected.spec().clone())
            .collect::<Vec<_>>();
        validate_unique_entries(&entries)?;
        let revision = if entries == current.entries() {
            current.revision()
        } else {
            current
                .revision()
                .checked_add(1)
                .ok_or(StartupContextError::PlanRevisionOverflow)?
        };
        let updated_at = Utc::now();
        Ok(StartupProjectPlanTransition::new(
            project.key().to_stored()?,
            current.revision(),
            revision,
            current
                .entries()
                .iter()
                .map(StartupFileSpec::to_stored)
                .collect::<Result<Vec<_>, _>>()?,
            entries
                .iter()
                .map(StartupFileSpec::to_stored)
                .collect::<Result<Vec<_>, _>>()?,
            updated_at,
        ))
    }

    pub(super) fn commit_transition(
        &self,
        project: &ActiveProject,
        transition: &StartupProjectPlanTransition,
    ) -> Result<StartupProjectPlanCommitOutcome, StartupContextError> {
        let expected_project = project.key().to_stored()?;
        if transition.project() != &expected_project {
            return Err(StartupContextError::PlanProjectMismatch);
        }
        let previous_entries = decode_transition_entries(transition.previous_entries())?;
        let proposed_entries = decode_transition_entries(transition.proposed_entries())?;
        validate_unique_entries(&previous_entries)?;
        validate_unique_entries(&proposed_entries)?;

        let expected_proposed_revision = if previous_entries == proposed_entries {
            transition.previous_revision()
        } else {
            transition
                .previous_revision()
                .checked_add(1)
                .ok_or(StartupContextError::PlanRevisionOverflow)?
        };
        if transition.proposed_revision() != expected_proposed_revision {
            return Err(StartupContextError::InvalidPlanTransition {
                detail: format!(
                    "proposed revision {} does not follow previous revision {}",
                    transition.proposed_revision(),
                    transition.previous_revision()
                ),
            });
        }

        let current = self.load(project)?.into_plan();
        if current.revision() == transition.proposed_revision()
            && current.entries() == proposed_entries
        {
            return Ok(if transition.changes_plan() {
                StartupProjectPlanCommitOutcome::AlreadyApplied
            } else {
                StartupProjectPlanCommitOutcome::Unchanged
            });
        }
        if current.revision() != transition.previous_revision()
            || current.entries() != previous_entries
        {
            return Err(StartupContextError::StalePlanRevision {
                expected: transition.previous_revision(),
                actual: current.revision(),
            });
        }
        if !transition.changes_plan() {
            return Ok(StartupProjectPlanCommitOutcome::Unchanged);
        }

        let stored = StoredProjectPlan {
            schema_version: STARTUP_PROJECT_PLAN_SCHEMA_VERSION,
            revision: transition.proposed_revision(),
            project: expected_project,
            entries: transition.proposed_entries().to_vec(),
            updated_at: transition.updated_at(),
        };
        self.write_stored_plan(project.key(), &stored)?;
        Ok(StartupProjectPlanCommitOutcome::Applied)
    }

    fn write_stored_plan(
        &self,
        project_key: &ProjectKey,
        stored: &StoredProjectPlan,
    ) -> Result<(), StartupContextError> {
        let path = self.path_for(project_key);
        jcode_storage::write_json_secret(&path, stored).map_err(|error| {
            StartupContextError::PlanStorage {
                path,
                detail: error.to_string(),
            }
        })
    }

    pub(super) fn path_for(&self, project_key: &ProjectKey) -> PathBuf {
        let digest = Sha256::digest(project_key.stable_bytes());
        self.projects_dir.join(format!("{digest:x}.json"))
    }

    #[cfg(test)]
    pub(super) fn projects_dir(&self) -> &Path {
        &self.projects_dir
    }

    fn read_and_decode(
        &self,
        path: &Path,
        expected_project: &ProjectKey,
    ) -> Result<StartupProjectPlan, StoredPlanReadError> {
        let bytes = std::fs::read(path).map_err(|error| {
            StoredPlanReadError::Corrupt(format!("could not read plan: {error}"))
        })?;
        let stored: StoredProjectPlan = serde_json::from_slice(&bytes)
            .map_err(|error| StoredPlanReadError::Corrupt(format!("invalid JSON: {error}")))?;
        if stored.schema_version != STARTUP_PROJECT_PLAN_SCHEMA_VERSION {
            return Err(StoredPlanReadError::UnsupportedSchema(
                stored.schema_version,
            ));
        }
        let project_key = ProjectKey::from_stored(stored.project)
            .map_err(|error| StoredPlanReadError::Corrupt(error.to_string()))?;
        if &project_key != expected_project {
            return Err(StoredPlanReadError::Corrupt(
                "stored project identity does not match its project-key path".to_string(),
            ));
        }
        if stored.entries.len() > self.max_entries {
            return Err(StoredPlanReadError::Corrupt(format!(
                "plan contains {} entries, exceeding the {} entry safety limit",
                stored.entries.len(),
                self.max_entries
            )));
        }
        let entries = stored
            .entries
            .into_iter()
            .map(StartupFileSpec::from_stored)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| StoredPlanReadError::Corrupt(error.to_string()))?;
        validate_unique_entries(&entries)
            .map_err(|error| StoredPlanReadError::Corrupt(error.to_string()))?;
        Ok(StartupProjectPlan::stored(
            project_key,
            stored.revision,
            entries,
            stored.updated_at,
        ))
    }
}

fn decode_transition_entries(
    entries: &[StoredStartupFileSpec],
) -> Result<Vec<StartupFileSpec>, StartupContextError> {
    entries
        .iter()
        .cloned()
        .map(StartupFileSpec::from_stored)
        .collect()
}

enum StoredPlanReadError {
    UnsupportedSchema(u32),
    Corrupt(String),
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredProjectPlan {
    schema_version: u32,
    revision: u64,
    project: StoredStartupProjectIdentity,
    entries: Vec<StoredStartupFileSpec>,
    updated_at: DateTime<Utc>,
}

fn validate_unique_entries(entries: &[StartupFileSpec]) -> Result<(), StartupContextError> {
    let mut ids = HashSet::with_capacity(entries.len());
    let mut paths = HashSet::with_capacity(entries.len());
    for entry in entries {
        if !ids.insert(entry.id().clone()) {
            return Err(StartupContextError::InvalidStoredPlan {
                detail: format!("duplicate startup file spec id {}", entry.id()),
            });
        }
        if !paths.insert(entry.path().clone()) {
            return Err(StartupContextError::InvalidStoredPlan {
                detail: format!(
                    "duplicate startup file path {}",
                    entry.path().as_path().display()
                ),
            });
        }
    }
    Ok(())
}

fn restore_recovered_plan(
    plan: &StartupProjectPlan,
    primary_path: &Path,
) -> Result<(), StartupContextError> {
    let stored = StoredProjectPlan {
        schema_version: STARTUP_PROJECT_PLAN_SCHEMA_VERSION,
        revision: plan.revision(),
        project: plan.project_key().to_stored()?,
        entries: plan
            .entries()
            .iter()
            .map(StartupFileSpec::to_stored)
            .collect::<Result<Vec<_>, _>>()?,
        updated_at: plan
            .updated_at()
            .ok_or_else(|| StartupContextError::InvalidStoredPlan {
                detail: "a recovered stored plan is missing its update time".to_string(),
            })?,
    };
    let bytes = serde_json::to_vec(&stored).map_err(|error| StartupContextError::PlanStorage {
        path: primary_path.to_path_buf(),
        detail: format!("could not serialize recovered plan: {error}"),
    })?;
    let parent = primary_path
        .parent()
        .ok_or_else(|| StartupContextError::PlanStorage {
            path: primary_path.to_path_buf(),
            detail: "plan path has no parent directory".to_string(),
        })?;
    jcode_storage::ensure_dir(parent).map_err(|error| StartupContextError::PlanStorage {
        path: primary_path.to_path_buf(),
        detail: format!("could not prepare recovered plan directory: {error}"),
    })?;
    let filename = primary_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("startup-plan");
    let temporary_path = parent.join(format!(
        ".{filename}.recovery.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));

    let result = (|| -> Result<(), StartupContextError> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_path)
            .map_err(|error| StartupContextError::PlanStorage {
                path: primary_path.to_path_buf(),
                detail: format!("could not create recovered plan temporary file: {error}"),
            })?;
        crate::platform::set_permissions_owner_only(&temporary_path).map_err(|error| {
            StartupContextError::PlanStorage {
                path: primary_path.to_path_buf(),
                detail: format!("could not harden recovered plan temporary file: {error}"),
            }
        })?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| StartupContextError::PlanStorage {
                path: primary_path.to_path_buf(),
                detail: format!("could not durably write recovered plan: {error}"),
            })?;

        #[cfg(not(unix))]
        if primary_path.exists() {
            std::fs::remove_file(primary_path).map_err(|error| {
                StartupContextError::PlanStorage {
                    path: primary_path.to_path_buf(),
                    detail: format!("could not replace corrupt recovered plan: {error}"),
                }
            })?;
        }
        std::fs::rename(&temporary_path, primary_path).map_err(|error| {
            StartupContextError::PlanStorage {
                path: primary_path.to_path_buf(),
                detail: format!("could not publish recovered plan atomically: {error}"),
            }
        })?;
        crate::platform::set_permissions_owner_only(primary_path).map_err(|error| {
            StartupContextError::PlanStorage {
                path: primary_path.to_path_buf(),
                detail: format!("could not harden recovered primary plan: {error}"),
            }
        })?;
        #[cfg(unix)]
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    result
}

#[cfg(test)]
pub(super) fn stored_plan_path(store: &StartupPlanStore, project_key: &ProjectKey) -> PathBuf {
    store.path_for(project_key)
}

#[cfg(test)]
pub(super) fn write_raw_plan(path: &Path, value: &serde_json::Value) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create raw plan parent");
    }
    std::fs::write(path, serde_json::to_vec(value).expect("serialize raw plan"))
        .expect("write raw plan");
}
