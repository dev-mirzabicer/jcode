//! Retained unscoped file behavior on platforms without C01 native acceptance.
//! Managed workspace permits never select this compatibility implementation.
use anyhow::{Context, Result, ensure};
use jcode_tool_core::native_files::NativeFilePermit;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub(super) struct LegacyFiles(BTreeMap<PathBuf, PathBuf>, BTreeSet<PathBuf>);
impl LegacyFiles {
    pub fn acquire(paths: &[(PathBuf, PathBuf)], removals: &[PathBuf]) -> Self {
        Self(
            paths.iter().cloned().collect(),
            removals.iter().cloned().collect(),
        )
    }
    fn path(&self, path: &Path) -> Result<&Path> {
        let resolved = self
            .0
            .get(path)
            .context("File was not included in mutation admission")?;
        ensure!(
            jcode_base::location::native_files::resolve_target(path)? == *resolved,
            "File alias changed after admission"
        );
        Ok(resolved)
    }
}
impl NativeFilePermit for LegacyFiles {
    fn read(&mut self, path: &Path) -> Result<Option<Vec<u8>>> {
        match std::fs::read(self.path(path)?) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    fn write(&mut self, path: &Path, bytes: &[u8]) -> Result<()> {
        let path = self.path(path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(std::fs::write(path, bytes)?)
    }
    fn remove(&mut self, path: &Path) -> Result<()> {
        self.path(path)?;
        ensure!(self.1.contains(path), "Removal was not admitted");
        let entry = jcode_base::location::native_files::resolve_removal_entry(path)?;
        std::fs::remove_file(&entry)?;
        self.0.insert(path.into(), entry);
        Ok(())
    }
    fn same_file(&self, first: &Path, second: &Path) -> Result<bool> {
        Ok(self.path(first)? == self.path(second)?)
    }
    fn copy_metadata(&mut self, source: &Path, destination: &Path) -> Result<()> {
        // Preserve mode using the existing portable owner. Full ACL/xattr fidelity
        // belongs to the supported C01 implementation, not this legacy route.
        Ok(std::fs::set_permissions(
            self.path(destination)?,
            std::fs::metadata(self.path(source)?)?.permissions(),
        )?)
    }
}
