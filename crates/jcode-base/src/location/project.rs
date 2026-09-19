use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ProjectKey {
    Git { canonical_common_dir: PathBuf },
    Directory { canonical_root: PathBuf },
}

impl ProjectKey {
    pub fn canonical_identity_path(&self) -> &Path {
        match self {
            Self::Git {
                canonical_common_dir,
            } => canonical_common_dir,
            Self::Directory { canonical_root } => canonical_root,
        }
    }

    pub fn is_git(&self) -> bool {
        matches!(self, Self::Git { .. })
    }

    pub(crate) fn stable_bytes(&self) -> Vec<u8> {
        let (kind, path) = match self {
            Self::Git {
                canonical_common_dir,
            } => (b"git".as_slice(), canonical_common_dir),
            Self::Directory { canonical_root } => (b"directory".as_slice(), canonical_root),
        };
        let mut bytes = Vec::with_capacity(kind.len() + 1 + path.as_os_str().len());
        bytes.extend_from_slice(kind);
        bytes.push(0);
        bytes.extend_from_slice(path.to_string_lossy().as_bytes());
        bytes
    }

    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(self.stable_bytes()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectFacts {
    key: ProjectKey,
    active_root: PathBuf,
}

impl ProjectFacts {
    pub(crate) fn new(key: ProjectKey, active_root: PathBuf) -> Self {
        Self { key, active_root }
    }

    pub fn key(&self) -> &ProjectKey {
        &self.key
    }

    pub fn active_root(&self) -> &Path {
        &self.active_root
    }
}
