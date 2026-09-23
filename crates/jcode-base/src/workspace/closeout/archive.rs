//! One held filesystem anchor for preservation writes. Missing mounts and
//! replaced ancestors cannot turn recursive creation into fallback storage.
use super::*;
use crate::location::native_files::VerifiedDirectory;
use std::fs::File;
use std::io::Write;

pub(super) struct Archive {
    root: PathBuf,
    directory: VerifiedDirectory,
}
impl Archive {
    pub(super) fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
    }
    pub(super) fn existing_directory(&self, path: &Path) -> Result<VerifiedDirectory> {
        self.directory
            .preservation_directory(path.strip_prefix(&self.root).map_err(corrupt)?, false)
            .map_err(io)
    }
    pub(super) fn open(root: &Path) -> Result<Self> {
        Ok(Self {
            root: root.into(),
            directory: VerifiedDirectory::open(root.into()).map_err(io)?,
        })
    }
    pub(super) fn directory(&self, path: &Path) -> Result<VerifiedDirectory> {
        let relative = path.strip_prefix(&self.root).map_err(corrupt)?;
        self.directory
            .preservation_directory(relative, true)
            .map_err(io)
    }
    pub(super) fn subtree(&self, path: &Path) -> Result<Self> {
        Ok(Self {
            root: path.into(),
            directory: self.directory(path)?,
        })
    }
    pub(super) fn file(&self, path: &Path) -> Result<File> {
        let parent = self.directory(
            path.parent()
                .ok_or_else(|| corrupt("Archive file has no parent"))?,
        )?;
        parent
            .create_preserved_file(
                path.file_name()
                    .ok_or_else(|| corrupt("Archive file has no name"))?,
            )
            .map_err(io)
    }
    pub(super) fn json(&self, path: &Path, value: &impl Serialize) -> Result<()> {
        let mut file = self.file(path)?;
        serde_json::to_writer(&mut file, value).map_err(io)?;
        file.write_all(b"\n").map_err(io)?;
        file.sync_all().map_err(io)?;
        self.verify()
    }
    pub(super) fn verify(&self) -> Result<()> {
        self.directory.verify().map_err(io)
    }
}
