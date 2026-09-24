//! Read lifetime for local Git transports. A read grant is unnecessary, but
//! local acquisition cannot race a catalog Closing gate or physical removal.
use super::*;

pub(super) struct LocalSourceUse {
    _location: WorkspaceUseLease,
    _physical: RootLease,
}
impl WorkspaceService {
    pub(super) fn acquire_source_path(&self, path: &Path) -> Result<LocalSourceUse> {
        let location = self.acquire_location_use(Some(path), &[])?;
        let binding = self.resolver.bind_directory(path).map_err(io)?;
        let physical = self.acquire_mutation_binding(&binding)?;
        Ok(LocalSourceUse {
            _location: location,
            _physical: physical,
        })
    }
    pub(super) fn acquire_transport_source(
        &self,
        repository: &Path,
        url: &str,
    ) -> Result<Option<LocalSourceUse>> {
        local_transport_path(repository, url)?
            .map(|path| self.acquire_source_path(&path))
            .transpose()
    }

    pub(super) fn acquire_lfs_source(
        &self,
        repository: &Path,
        endpoint: &str,
    ) -> Result<Vec<LocalSourceUse>> {
        let Some(path) = local_transport_path(repository, endpoint)? else {
            return Ok(Vec::new());
        };
        let root = if path.is_file() {
            path.parent()
                .ok_or_else(|| corrupt("LFS Git file has no parent"))?
        } else {
            &path
        };
        let mut leases = vec![self.acquire_source_path(root)?];
        // Git LFS may place media outside the repository. Ask the same owner
        // rather than infer its cache from a spelling of .git/lfs/objects.
        let directory = materialize::local_lfs_media_directory(root)?;
        leases.push(self.acquire_source_path(&directory)?);
        Ok(leases)
    }
}
fn local_transport_path(repository: &Path, url: &str) -> Result<Option<PathBuf>> {
    if url.starts_with("file://") {
        return url::Url::parse(url)
            .map_err(io)?
            .to_file_path()
            .map(Some)
            .map_err(|_| {
                issue(
                    IssueCode::InvalidInput,
                    "Local transport URL does not identify a filesystem path",
                )
            });
    }
    if Path::new(url).is_absolute() {
        return Ok(Some(url.into()));
    }
    if url.starts_with("./") || url.starts_with("../") {
        return Ok(Some(repository.join(url)));
    }
    Ok(None)
}
