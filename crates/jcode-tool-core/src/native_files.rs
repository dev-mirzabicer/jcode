//! Code-owned native-file boundary. Tool arguments cannot construct policy or permits.
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Derived from parsed operations, never from a caller's approval claims.
/// A move source needs regular contents as well as entry removal. A deletion
/// alone may unlink a symlink without requiring a regular-file referent.
#[derive(Clone, Debug, Default)]
pub struct NativeFilePlan {
    paths: Vec<PathBuf>,
    files: BTreeSet<PathBuf>,
    removals: Vec<PathBuf>,
}
impl NativeFilePlan {
    pub fn new(files: Vec<PathBuf>, removals: Vec<PathBuf>) -> Self {
        let files: BTreeSet<_> = files.into_iter().collect();
        let removals: BTreeSet<_> = removals.into_iter().collect();
        Self {
            paths: files.union(&removals).cloned().collect(),
            files,
            removals: removals.into_iter().collect(),
        }
    }
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
    pub fn removals(&self) -> &[PathBuf] {
        &self.removals
    }
    pub fn requires_file(&self, path: &Path) -> bool {
        self.files.contains(path)
    }
    pub fn file(&mut self, path: PathBuf) {
        if !self.removals.contains(&path) {
            self.files.insert(path.clone());
        }
        if !self.paths.contains(&path) {
            self.paths.push(path);
        }
    }
    pub fn remove(&mut self, path: PathBuf) {
        if !self.paths.contains(&path) {
            self.paths.push(path.clone());
        }
        if !self.removals.contains(&path) {
            self.removals.push(path);
        }
    }
}

pub trait NativeFilePolicy: Send + Sync {
    fn session_id(&self) -> &str;
    /// Called when effects start, after any execution queue or pre-tool hook.
    fn acquire(&self, plan: &NativeFilePlan) -> Result<Box<dyn NativeFilePermit>>;
}

/// One admission covers every explicit destination of one native operation.
/// Implementations retain ownership and revalidate physical targets at each effect.
pub trait NativeFilePermit: Send {
    fn read(&mut self, path: &Path) -> Result<Option<Vec<u8>>>;
    fn write(&mut self, path: &Path, contents: &[u8]) -> Result<()>;
    fn remove(&mut self, path: &Path) -> Result<()>;
    fn same_file(&self, first: &Path, second: &Path) -> Result<bool>;
    fn copy_metadata(&mut self, source: &Path, destination: &Path) -> Result<()>;
}
