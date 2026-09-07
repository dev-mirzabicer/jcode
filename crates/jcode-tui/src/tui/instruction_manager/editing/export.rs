use crate::protocol::InstructionRevisionExport;
use anyhow::{Context, Result};
use base64::Engine;
use std::{
    io::Write,
    path::{Component, Path, PathBuf},
};

pub(crate) fn write_revision(root: &Path, export: InstructionRevisionExport) -> Result<PathBuf> {
    let directory = root.join("instruction-exports");
    if directory.is_symlink() {
        anyhow::bail!("Export directory is a symlink; it was not followed");
    }
    crate::storage::ensure_dir(&directory)?;
    let temporary = tempfile::Builder::new()
        .prefix("revision-")
        .tempdir_in(&directory)?;
    for file in export.files {
        let relative = Path::new(&file.path);
        anyhow::ensure!(
            !relative.as_os_str().is_empty()
                && relative.components().all(
                    |component| matches!(component, Component::Normal(name) if name != ".git")
                ),
            "Invalid export path"
        );
        let path = temporary.path().join(relative);
        let parent = path.parent().context("Export file has no parent")?;
        std::fs::create_dir_all(parent)?;
        let mut ancestor = Some(parent);
        while let Some(path) = ancestor.filter(|path| path.starts_with(temporary.path())) {
            crate::platform::set_directory_permissions_owner_only(path)?;
            ancestor = path.parent();
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(file.base64)?;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        crate::platform::set_permissions_owner_only(&path)?;
        #[cfg(unix)]
        if file.executable {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        }
        output.write_all(&bytes)?;
        output.sync_all()?;
    }
    Ok(temporary.keep())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::InstructionExportFile;
    #[test]
    fn export_preserves_binary_bytes_and_rejects_parent_paths() {
        let root = tempfile::tempdir().unwrap();
        let bytes = [0xff, 0, 1];
        let export = InstructionRevisionExport {
            revision: "synthetic".into(),
            files: vec![InstructionExportFile {
                path: "skills/package/data.bin".into(),
                base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                executable: false,
            }],
        };
        let target = write_revision(root.path(), export).unwrap();
        assert_eq!(
            std::fs::read(target.join("skills/package/data.bin")).unwrap(),
            bytes
        );
        let bad = InstructionRevisionExport {
            revision: "synthetic".into(),
            files: vec![InstructionExportFile {
                path: "../outside".into(),
                base64: String::new(),
                executable: false,
            }],
        };
        assert!(write_revision(root.path(), bad).is_err());
        assert!(!root.path().join("outside").exists());
    }
}
