//! Code-owned native-file boundary. Tool arguments cannot construct policy or permits.
use anyhow::Result;
use std::path::{Path, PathBuf};

pub trait NativeFilePolicy: Send + Sync {
    fn session_id(&self) -> &str;
    /// Called when effects start, after any execution queue or pre-tool hook.
    fn acquire(&self, paths: &[PathBuf], removals: &[PathBuf])
    -> Result<Box<dyn NativeFilePermit>>;
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
