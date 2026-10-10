//! The session-work harness destination for native file tools. A session's
//! own `workflow.md` and `summary.md` under `<jcode home>/session-work/` are
//! written through the session-work store, never as plain files: a workflow
//! text is validated and committed as a revision before the file changes.
use super::*;
use jcode_base::session_work::{SessionWorkSurface, SurfaceFile, SurfacePath};

/// The session-work part of one native operation, admitted for the acting
/// Session only.
pub(super) struct SessionWorkFiles {
    surface: SessionWorkSurface,
    session: String,
    /// Stable across a replay of the same invocation, so a recovered write
    /// converges on the revision it already committed.
    request: String,
    writes: usize,
}

/// Where a native path belongs. The leaf is not followed, so a symlink placed
/// at `workflow.md` cannot redirect a workflow write elsewhere.
pub(super) fn classify(surface: &SessionWorkSurface, path: &Path) -> Result<Option<SurfacePath>> {
    let entry = resolve_removal_entry(path)?;
    Ok(surface.classify(&entry))
}

pub(super) fn request_base(ctx: &ToolContext) -> String {
    match &ctx.invocation.identity {
        Some(identity) => identity.id.clone(),
        None if !ctx.tool_call_id.is_empty() => {
            format!("{}:{}", ctx.message_id, ctx.tool_call_id)
        }
        None => uuid::Uuid::new_v4().to_string(),
    }
}

impl SessionWorkFiles {
    pub(super) fn admit(
        surface: SessionWorkSurface,
        session: &Session,
        request: String,
        paths: &[(PathBuf, SurfacePath)],
        removals: &[PathBuf],
    ) -> Result<Self> {
        ensure!(
            session.session_work.is_some(),
            "Native file mutation cannot edit harness control state; this session does not use session work"
        );
        for (path, place) in paths {
            ensure!(
                place.session == session.id,
                "{} belongs to another session's session work; only its own session can edit it",
                path.display()
            );
            match place.file {
                SurfaceFile::Workflow | SurfaceFile::Summary => {}
                SurfaceFile::History => anyhow::bail!(
                    "history/ is read-only. To undo, write an older revision's text to workflow.md"
                ),
                SurfaceFile::Directory | SurfaceFile::Other => anyhow::bail!(
                    "The session-work directory holds only workflow.md and summary.md; {} can't be written",
                    path.display()
                ),
            }
            ensure!(
                !removals.contains(path),
                "{} can't be deleted or moved. A workflow stays once created: skip the modules you drop and give the reason",
                path.display()
            );
        }
        // Fail closed before any effect when the store cannot be read.
        let activation = surface.store().activation(&session.id)?;
        ensure!(
            activation.is_some(),
            "Session work for {} is missing from its store; restore the store before editing its files",
            session.id
        );
        Ok(Self {
            surface,
            session: session.id.clone(),
            request,
            writes: 0,
        })
    }

    fn file(&self, path: &Path) -> Result<SurfaceFile> {
        let place = classify(&self.surface, path)?
            .context("Path is outside the session-work destination")?;
        ensure!(
            place.session == self.session,
            "Session-work path was not admitted"
        );
        Ok(place.file)
    }

    pub(super) fn read(&mut self, path: &Path) -> Result<Option<Vec<u8>>> {
        Ok(match self.file(path)? {
            // The store is the authority, whatever the file currently holds.
            SurfaceFile::Workflow => self
                .surface
                .current_workflow(&self.session)?
                .map(String::into_bytes),
            SurfaceFile::Summary => self.surface.read_summary(&self.session)?,
            _ => anyhow::bail!("Session-work path was not admitted"),
        })
    }

    pub(super) fn write(&mut self, path: &Path, bytes: &[u8]) -> Result<()> {
        match self.file(path)? {
            SurfaceFile::Workflow => {
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| anyhow::anyhow!("workflow.md must be UTF-8 text"))?;
                self.writes += 1;
                let request = format!("{}#{}", self.request, self.writes);
                self.surface
                    .write_workflow(&self.session, &request, text, chrono::Utc::now())
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                Ok(())
            }
            SurfaceFile::Summary => Ok(self.surface.write_summary(&self.session, bytes)?),
            _ => anyhow::bail!("Session-work path was not admitted"),
        }
    }
}

/// One native operation spanning the session-work destination and ordinary
/// destinations. Each path goes to the permit that admitted it.
pub(super) struct SplitPermit {
    pub(super) session_work: SessionWorkFiles,
    pub(super) regular: Option<Box<dyn NativeFilePermit>>,
}

impl SplitPermit {
    fn is_session_work(&self, path: &Path) -> Result<bool> {
        Ok(classify(&self.session_work.surface, path)?.is_some())
    }
    fn regular(&mut self) -> Result<&mut Box<dyn NativeFilePermit>> {
        self.regular
            .as_mut()
            .context("Native destination was not admitted")
    }
}

impl NativeFilePermit for SplitPermit {
    fn read(&mut self, path: &Path) -> Result<Option<Vec<u8>>> {
        if self.is_session_work(path)? {
            self.session_work.read(path)
        } else {
            self.regular()?.read(path)
        }
    }
    fn write(&mut self, path: &Path, contents: &[u8]) -> Result<()> {
        if self.is_session_work(path)? {
            self.session_work.write(path, contents)
        } else {
            self.regular()?.write(path, contents)
        }
    }
    fn remove(&mut self, path: &Path) -> Result<()> {
        ensure!(
            !self.is_session_work(path)?,
            "Session-work files can't be deleted"
        );
        self.regular()?.remove(path)
    }
    fn same_file(&self, first: &Path, second: &Path) -> Result<bool> {
        let surface = &self.session_work.surface;
        if classify(surface, first)?.is_some() || classify(surface, second)?.is_some() {
            return Ok(resolve_removal_entry(first)? == resolve_removal_entry(second)?);
        }
        self.regular
            .as_ref()
            .context("Native destination was not admitted")?
            .same_file(first, second)
    }
    fn copy_metadata(&mut self, source: &Path, destination: &Path) -> Result<()> {
        ensure!(
            !self.is_session_work(source)?,
            "Session-work files can't be moved"
        );
        if self.is_session_work(destination)? {
            // Host-owned files keep the host's permissions.
            return Ok(());
        }
        self.regular()?.copy_metadata(source, destination)
    }
}
