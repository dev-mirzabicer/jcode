//! Acquisition and resulting remotes have different authority. Never discard
//! acquired branch identity merely because a reviewed push remote differs.
use super::*;
use std::collections::{BTreeMap, HashSet};

fn source_url(operation: &CloneOperation) -> String {
    match &operation.public.review.spec.source {
        CloneSource::Local { path } => path.to_string_lossy().into_owned(),
        CloneSource::Remote { url } => url.clone(),
    }
}

fn remote_urls(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut remotes = BTreeMap::new();
    for name in materialize::checked_git(root, ["remote"])?.lines() {
        let url = materialize::checked_git(root, ["remote", "get-url", name])?;
        let url = url.trim_end().to_owned();
        if url.is_empty() || remotes.insert(name.to_owned(), url).is_some() {
            return Err(corrupt("Clone stage has ambiguous Git remotes"));
        }
    }
    Ok(remotes)
}

pub(super) fn verify_acquisition_origin(operation: &CloneOperation, root: &Path) -> Result<()> {
    let remotes = remote_urls(root)?;
    if remotes.len() != 1 || remotes.get("origin") != Some(&source_url(operation)) {
        return Err(issue(
            IssueCode::Conflict,
            "Acquired clone origin changed before reviewed materialization",
        ));
    }
    Ok(())
}

pub(super) fn observe_acquired_refs(root: &Path) -> Result<Vec<AcquiredRef>> {
    let output = materialize::checked_git(
        root,
        [
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/remotes/origin",
        ],
    )?;
    let mut refs = Vec::new();
    let mut names = HashSet::new();
    for line in output.lines() {
        let (name, oid) = line
            .split_once(' ')
            .ok_or_else(|| corrupt("Invalid acquired Git ref"))?;
        let branch = name
            .strip_prefix("refs/remotes/origin/")
            .ok_or_else(|| corrupt("Acquired ref is not in the origin namespace"))?;
        if branch == "HEAD" {
            continue;
        } // symbolic default, not another branch
        git::validate_branch(branch)?;
        if !names.insert(branch.to_owned())
            || !matches!(oid.len(), 40 | 64)
            || !oid.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(corrupt(
                "Acquired Git branch ref is malformed or duplicated",
            ));
        }
        refs.push(AcquiredRef {
            branch: branch.into(),
            oid: oid.into(),
        });
    }
    refs.sort_by(|a, b| a.branch.cmp(&b.branch));
    Ok(refs)
}

fn acquired_refs(operation: &CloneOperation) -> Result<&[AcquiredRef]> {
    operation.acquired_refs.as_deref().ok_or_else(|| issue(
        IssueCode::RecoveryRequired,
        "Clone has no recorded acquisition refs; inspect its retained stage before changing remotes"))
}

fn neutral_ref(operation: &CloneOperation, branch: &str) -> String {
    format!(
        "refs/jcode/checkout-acquired/{}/{}",
        operation.public.operation, branch
    )
}

fn ref_oid(root: &Path, reference: &str) -> Result<Option<String>> {
    let listing = materialize::checked_git(
        root,
        [
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            reference,
        ],
    )?;
    let mut result = None;
    for line in listing.lines() {
        let (name, oid) = line
            .split_once(' ')
            .ok_or_else(|| corrupt("Invalid Git ref listing"))?;
        if name == reference && result.replace(oid.to_owned()).is_some() {
            return Err(corrupt("Duplicate Git ref during clone recovery"));
        }
    }
    Ok(result)
}

pub(super) fn verify_result_refs(operation: &CloneOperation, root: &Path) -> Result<()> {
    let refs = acquired_refs(operation)?;
    let keep_origin = operation
        .public
        .review
        .spec
        .remotes
        .iter()
        .any(|remote| remote.name == "origin" && remote.url == source_url(operation));
    if keep_origin {
        verify_acquisition_origin_or_extras(operation, root)?;
        if observe_acquired_refs(root)? != refs {
            return Err(issue(
                IssueCode::Conflict,
                "Acquired branch history changed before checkout publication",
            ));
        }
    } else {
        for acquired in refs {
            if ref_oid(root, &neutral_ref(operation, &acquired.branch))?.as_deref()
                != Some(acquired.oid.as_str())
            {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    "Acquired branch history is not preserved independently of resulting remotes",
                ));
            }
        }
    }
    Ok(())
}

fn verify_acquisition_origin_or_extras(operation: &CloneOperation, root: &Path) -> Result<()> {
    if remote_urls(root)?.get("origin") != Some(&source_url(operation)) {
        return Err(issue(
            IssueCode::Conflict,
            "Acquisition origin or branch tracking changed",
        ));
    }
    Ok(())
}

impl WorkspaceService {
    async fn preserve_acquired_refs(
        &self,
        request: RequestId,
        operation: &CloneOperation,
        root: &Path,
        capture: &dyn OutputCapture,
    ) -> Result<()> {
        let refs = acquired_refs(operation)?;
        let current_origin = remote_urls(root)?.get("origin") == Some(&source_url(operation));
        if current_origin && observe_acquired_refs(root)? != refs {
            return Err(issue(
                IssueCode::Conflict,
                "Acquired source branches changed before preservation",
            ));
        }
        for acquired in refs {
            let name = neutral_ref(operation, &acquired.branch);
            match ref_oid(root, &name)? {
                Some(value) if value == acquired.oid => continue,
                Some(_) => {
                    return Err(issue(
                        IssueCode::Conflict,
                        "An acquired branch preservation ref was modified",
                    ));
                }
                None if !current_origin => {
                    return Err(issue(
                        IssueCode::RecoveryRequired,
                        "Acquisition origin disappeared before branch history was preserved",
                    ));
                }
                None => {}
            }
            let absent = "0".repeat(acquired.oid.len());
            self.git_step(
                request,
                root,
                ["update-ref", &name, &acquired.oid, &absent],
                capture,
            )
            .await?;
            self.checkpoint("clone_acquired_ref_saved")?;
        }
        verify_result_refs(operation, root)
    }

    pub(super) async fn configure_clone_remotes(
        &self,
        request: RequestId,
        operation: &CloneOperation,
        stage: &PhysicalBinding,
        capture: &dyn OutputCapture,
    ) -> Result<()> {
        let root = stage.observed_path();
        self.verify_acquired(operation, stage)?;
        materialize::verify_clean_tree(root, &operation.public.review.spec)?;
        let spec = &operation.public.review.spec;
        let source = source_url(operation);
        let expected: BTreeMap<_, _> = spec
            .remotes
            .iter()
            .map(|remote| (remote.name.as_str(), remote.url.as_str()))
            .collect();
        let actual = remote_urls(root)?;
        for (name, url) in &actual {
            if name == "origin" && url == &source {
                continue;
            }
            if expected.get(name.as_str()) != Some(&url.as_str()) {
                return Err(issue(
                    IssueCode::Conflict,
                    "Clone stage has a remote not in its reviewed acquisition/result",
                ));
            }
        }
        let keep_origin = expected.get("origin") == Some(&source.as_str());
        if keep_origin {
            if actual.get("origin") != Some(&source) {
                return Err(issue(
                    IssueCode::RecoveryRequired,
                    "Unchanged acquisition origin is missing; branch tracking cannot be reconstructed",
                ));
            }
            verify_result_refs(operation, root)?;
        } else {
            self.preserve_acquired_refs(request, operation, root, capture)
                .await?;
            if actual.get("origin") == Some(&source) {
                self.git_step(request, root, ["remote", "remove", "origin"], capture)
                    .await?;
                self.checkpoint("clone_remote_removed")?;
            }
        }
        for remote in &spec.remotes {
            match remote_urls(root)?.get(&remote.name) {
                Some(url) if url == &remote.url => continue,
                Some(_) => {
                    return Err(issue(
                        IssueCode::Conflict,
                        "Resulting remote URL changed during clone preparation",
                    ));
                }
                None => {}
            }
            self.git_step(
                request,
                root,
                ["remote", "add", &remote.name, &remote.url],
                capture,
            )
            .await?;
            self.checkpoint("clone_remote_added")?;
        }
        let final_remotes = remote_urls(root)?;
        if final_remotes
            != expected
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        {
            return Err(issue(
                IssueCode::Conflict,
                "Resulting remotes differ from reviewed selection",
            ));
        }
        verify_result_refs(operation, root)
    }
}
