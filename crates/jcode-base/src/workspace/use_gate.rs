//! Checkout lifetime safety, independent of write permission. Reading another
//! Ready root needs no grant, but cannot race its authorized removal.
use super::*;
use crate::location::native_files::resolve_target;

pub struct WorkspaceUseLease {
    _roots: Vec<RootLease>,
}

impl WorkspaceService {
    /// `cwd` binds this invocation's execution location. Explicit directory
    /// targets also cover descendants, unlike cwd alone. Keep the receipt for
    /// the actual producer lifetime, not just its foreground waiter.
    pub fn acquire_location_use(
        &self,
        cwd: Option<&Path>,
        targets: &[PathBuf],
    ) -> Result<WorkspaceUseLease> {
        let mut leases = Vec::new();
        if !self.catalog_present()? {
            return Ok(WorkspaceUseLease { _roots: leases });
        }
        let _catalog = self.lease(false)?;
        let connection = self.connection()?;
        let snapshot = connection.unchecked_transaction().map_err(io)?;
        let revision = storage::status(&snapshot)?.revision;
        let cwd = cwd.map(resolve_target).transpose().map_err(io)?;
        let targets = targets
            .iter()
            .map(|path| resolve_target(path))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(io)?;
        let locations = scope::locations(&snapshot)?;
        for location in &locations {
            let path = &location.observed_path;
            let touches = cwd.as_ref().is_some_and(|cwd| cwd.starts_with(path))
                || targets
                    .iter()
                    .any(|target| target.starts_with(path) || path.starts_with(target));
            if !touches {
                continue;
            }
            // A newly registered replacement is a new identity, not revival of
            // the old location. Its own binding is validated below.
            if location.lifecycle == LocationLifecycle::Closed
                && locations.iter().any(|other| {
                    other.id != location.id
                        && other.observed_path == *path
                        && other.lifecycle == LocationLifecycle::Ready
                })
            {
                continue;
            }
            let binding = self
                .verify_writable_root(&snapshot, location)
                .map_err(|mut error| {
                    if location.lifecycle == LocationLifecycle::Closing {
                        error.code = IssueCode::LiveWork;
                        error.detail = format!(
                            "Checkout {} is closing and cannot admit new filesystem work",
                            location.id
                        );
                    }
                    error
                })?;
            leases.push(self.acquire_mutation_binding(&binding)?);
        }
        drop(snapshot);
        if storage::status(&connection)?.revision != revision {
            return Err(issue(
                IssueCode::Conflict,
                "Workspace changed during filesystem-use admission; retry with current state",
            ));
        }
        Ok(WorkspaceUseLease { _roots: leases })
    }
}
