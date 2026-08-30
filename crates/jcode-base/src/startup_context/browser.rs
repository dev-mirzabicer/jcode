use super::StartupContext;
use super::selection::natural_os_cmp;
use super::types::{
    ActiveProject, PreparedStartupEntry, StartupFailurePolicy, StartupFileIssue,
    StartupPathClassification, StartupSelectionInput,
};
use std::collections::HashSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

const MAX_SEARCH_VISITED_ENTRIES: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupBrowserEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupBrowserEntry {
    name: String,
    project_relative_path: PathBuf,
    resolved_path: PathBuf,
    path_valid_utf8: bool,
    kind: StartupBrowserEntryKind,
    classification: StartupPathClassification,
    navigable: bool,
    bytes: Option<u64>,
}

impl StartupBrowserEntry {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn project_relative_path(&self) -> &Path {
        &self.project_relative_path
    }

    pub fn resolved_path(&self) -> &Path {
        &self.resolved_path
    }

    pub fn path_valid_utf8(&self) -> bool {
        self.path_valid_utf8
    }

    pub fn kind(&self) -> StartupBrowserEntryKind {
        self.kind
    }

    pub fn classification(&self) -> StartupPathClassification {
        self.classification
    }

    pub fn navigable(&self) -> bool {
        self.navigable
    }

    pub fn bytes(&self) -> Option<u64> {
        self.bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupDirectoryPage {
    directory: PathBuf,
    total_entries: usize,
    page_start: usize,
    page_end: usize,
    next_page_start: Option<usize>,
    entries: Vec<StartupBrowserEntry>,
}

impl StartupDirectoryPage {
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn total_entries(&self) -> usize {
        self.total_entries
    }

    pub fn page_start(&self) -> usize {
        self.page_start
    }

    pub fn page_end(&self) -> usize {
        self.page_end
    }

    pub fn next_page_start(&self) -> Option<usize> {
        self.next_page_start
    }

    pub fn entries(&self) -> &[StartupBrowserEntry] {
        &self.entries
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupFileSearchResults {
    query: String,
    visited_entries: usize,
    omitted_results: usize,
    truncated: bool,
    canceled: bool,
    results: Vec<StartupBrowserEntry>,
}

impl StartupFileSearchResults {
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn visited_entries(&self) -> usize {
        self.visited_entries
    }

    pub fn omitted_results(&self) -> usize {
        self.omitted_results
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn canceled(&self) -> bool {
        self.canceled
    }

    pub fn results(&self) -> &[StartupBrowserEntry] {
        &self.results
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupCurrentFilePreview {
    logical_path: PathBuf,
    resolved_path: PathBuf,
    classification: StartupPathClassification,
    sha256: String,
    bytes: u64,
    estimated_tokens: u64,
    total_chars: usize,
    start_char: usize,
    end_char: usize,
    next_start_char: Option<usize>,
    content: String,
}

impl StartupCurrentFilePreview {
    pub fn logical_path(&self) -> &Path {
        &self.logical_path
    }

    pub fn resolved_path(&self) -> &Path {
        &self.resolved_path
    }

    pub fn classification(&self) -> StartupPathClassification {
        self.classification
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn estimated_tokens(&self) -> u64 {
        self.estimated_tokens
    }

    pub fn total_chars(&self) -> usize {
        self.total_chars
    }

    pub fn start_char(&self) -> usize {
        self.start_char
    }

    pub fn end_char(&self) -> usize {
        self.end_char
    }

    pub fn next_start_char(&self) -> Option<usize> {
        self.next_start_char
    }

    pub fn content(&self) -> &str {
        &self.content
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupBrowserError {
    InvalidPath { path: PathBuf, detail: String },
    Io { path: PathBuf, detail: String },
    FileIssue(StartupFileIssue),
}

impl fmt::Display for StartupBrowserError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { path, detail } => {
                write!(
                    formatter,
                    "invalid Startup Context browser path {}: {detail}",
                    path.display()
                )
            }
            Self::Io { path, detail } => {
                write!(
                    formatter,
                    "Startup Context browser I/O failed at {}: {detail}",
                    path.display()
                )
            }
            Self::FileIssue(issue) => {
                write!(formatter, "Startup Context file preview failed: {issue:?}")
            }
        }
    }
}

impl std::error::Error for StartupBrowserError {}

impl StartupContext {
    pub fn list_project_directory(
        &self,
        project: &ActiveProject,
        directory: &Path,
        page_start: usize,
        page_size: usize,
    ) -> Result<StartupDirectoryPage, StartupBrowserError> {
        let (logical_directory, relative_directory) = project_directory(project, directory)?;
        let mut entries = Vec::new();
        let read_dir =
            std::fs::read_dir(&logical_directory).map_err(|error| StartupBrowserError::Io {
                path: logical_directory.clone(),
                detail: error.to_string(),
            })?;
        for child in read_dir {
            let child = child.map_err(|error| StartupBrowserError::Io {
                path: logical_directory.clone(),
                detail: error.to_string(),
            })?;
            entries.push(browser_entry(project, &child.path())?);
        }
        entries.sort_by(|left, right| {
            natural_os_cmp(
                left.project_relative_path.as_os_str(),
                right.project_relative_path.as_os_str(),
            )
        });

        let total_entries = entries.len();
        let page_start = page_start.min(total_entries);
        let page_end = page_start.saturating_add(page_size).min(total_entries);
        let next_page_start = (page_end < total_entries).then_some(page_end);
        Ok(StartupDirectoryPage {
            directory: relative_directory,
            total_entries,
            page_start,
            page_end,
            next_page_start,
            entries: entries[page_start..page_end].to_vec(),
        })
    }

    pub fn search_project_files(
        &self,
        project: &ActiveProject,
        query: &str,
        max_results: usize,
        canceled: &AtomicBool,
    ) -> Result<StartupFileSearchResults, StartupBrowserError> {
        let query = query.trim().to_string();
        if query.is_empty() {
            return Err(StartupBrowserError::InvalidPath {
                path: project.active_root().to_path_buf(),
                detail: "search query is empty".to_string(),
            });
        }
        let query_folded = query.to_lowercase();
        let mut stack = vec![project.active_root().to_path_buf()];
        let mut visited_directories = HashSet::new();
        let mut visited_entries = 0usize;
        let mut matches = Vec::<(u8, StartupBrowserEntry)>::new();
        let mut omitted_results = 0usize;
        let mut truncated = false;

        while let Some(directory) = stack.pop() {
            if canceled.load(AtomicOrdering::Relaxed) {
                return Ok(StartupFileSearchResults {
                    query,
                    visited_entries,
                    omitted_results,
                    truncated,
                    canceled: true,
                    results: Vec::new(),
                });
            }
            let canonical =
                std::fs::canonicalize(&directory).map_err(|error| StartupBrowserError::Io {
                    path: directory.clone(),
                    detail: error.to_string(),
                })?;
            if !canonical.starts_with(project.active_root())
                || !visited_directories.insert(canonical)
            {
                continue;
            }
            let read_dir = match std::fs::read_dir(&directory) {
                Ok(read_dir) => read_dir,
                Err(error) => {
                    return Err(StartupBrowserError::Io {
                        path: directory,
                        detail: error.to_string(),
                    });
                }
            };
            for child in read_dir {
                if canceled.load(AtomicOrdering::Relaxed) {
                    return Ok(StartupFileSearchResults {
                        query,
                        visited_entries,
                        omitted_results,
                        truncated,
                        canceled: true,
                        results: Vec::new(),
                    });
                }
                if visited_entries >= MAX_SEARCH_VISITED_ENTRIES {
                    truncated = true;
                    break;
                }
                visited_entries += 1;
                let child = child.map_err(|error| StartupBrowserError::Io {
                    path: directory.clone(),
                    detail: error.to_string(),
                })?;
                if directory == project.active_root() && child.file_name() == ".git" {
                    continue;
                }
                let entry = browser_entry(project, &child.path())?;
                if entry.kind == StartupBrowserEntryKind::Directory && entry.navigable {
                    stack.push(child.path());
                }

                let relative_folded = entry.project_relative_path.to_string_lossy().to_lowercase();
                let name_folded = entry.name.to_lowercase();
                let stem_folded = Path::new(&entry.name)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_lowercase);
                let score = if name_folded == query_folded
                    || stem_folded.as_deref() == Some(query_folded.as_str())
                {
                    Some(0)
                } else if name_folded.starts_with(&query_folded) {
                    Some(1)
                } else if name_folded.contains(&query_folded) {
                    Some(2)
                } else if relative_folded.contains(&query_folded) {
                    Some(3)
                } else {
                    None
                };
                if let Some(score) = score {
                    if matches.len() < max_results {
                        matches.push((score, entry));
                    } else {
                        omitted_results = omitted_results.saturating_add(1);
                        if let Some((worst_index, worst)) = matches
                            .iter()
                            .enumerate()
                            .max_by(|(_, left), (_, right)| search_match_cmp(left, right))
                            && search_match_cmp(&(score, entry.clone()), worst).is_lt()
                        {
                            matches[worst_index] = (score, entry);
                        }
                    }
                }
            }
            if truncated {
                break;
            }
        }

        matches.sort_by(search_match_cmp);
        Ok(StartupFileSearchResults {
            query,
            visited_entries,
            omitted_results,
            truncated,
            canceled: false,
            results: matches.into_iter().map(|(_, entry)| entry).collect(),
        })
    }

    pub fn preview_current_file(
        &self,
        project: &ActiveProject,
        path: &Path,
        start_char: usize,
        max_chars: usize,
    ) -> Result<StartupCurrentFilePreview, StartupBrowserError> {
        let logical_absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            project.active_root().join(path)
        };
        let resolved = std::fs::canonicalize(&logical_absolute).map_err(|_| {
            let preview = self.preview_selection(project, [StartupSelectionInput::new(path)]);
            let issue = preview.issues().next().cloned().unwrap_or_else(|| {
                StartupFileIssue::batch(super::types::StartupFileIssueKind::Missing)
            });
            StartupBrowserError::FileIssue(issue)
        })?;
        let input = StartupSelectionInput::new(path).with_external_approval(resolved);
        let selection = self.preview_selection(project, [input]);
        if let Some(issue) = selection.issues().next() {
            return Err(StartupBrowserError::FileIssue(issue.clone()));
        }
        let preparation = self
            .prepare_selection(project, 0, &selection, StartupFailurePolicy::Block)
            .map_err(|error| StartupBrowserError::Io {
                path: logical_absolute,
                detail: error.to_string(),
            })?
            .into_preparation();
        let captured = preparation
            .entries()
            .iter()
            .find_map(|entry| match entry {
                PreparedStartupEntry::Captured(file) => Some(file),
                PreparedStartupEntry::Issue(_) => None,
            })
            .ok_or_else(|| {
                let issue = preparation.issues().next().cloned().unwrap_or_else(|| {
                    StartupFileIssue::batch(
                        super::types::StartupFileIssueKind::ChangedDuringCapture,
                    )
                });
                StartupBrowserError::FileIssue(issue)
            })?;
        let (content, total_chars, start_char, end_char, next_start_char) =
            char_chunk(captured.text(), start_char, max_chars);
        Ok(StartupCurrentFilePreview {
            logical_path: captured.logical_path().to_path_buf(),
            resolved_path: captured.resolved_path().to_path_buf(),
            classification: captured.classification(),
            sha256: captured.sha256().to_string(),
            bytes: captured.bytes(),
            estimated_tokens: captured.estimated_tokens(),
            total_chars,
            start_char,
            end_char,
            next_start_char,
            content,
        })
    }
}

fn search_match_cmp(
    (left_score, left): &(u8, StartupBrowserEntry),
    (right_score, right): &(u8, StartupBrowserEntry),
) -> std::cmp::Ordering {
    left_score.cmp(right_score).then_with(|| {
        natural_os_cmp(
            left.project_relative_path.as_os_str(),
            right.project_relative_path.as_os_str(),
        )
    })
}

fn project_directory(
    project: &ActiveProject,
    directory: &Path,
) -> Result<(PathBuf, PathBuf), StartupBrowserError> {
    if directory.is_absolute()
        || directory
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(StartupBrowserError::InvalidPath {
            path: directory.to_path_buf(),
            detail: "directory must be project-relative without parent traversal".to_string(),
        });
    }
    if directory.to_str().is_none() {
        return Err(StartupBrowserError::InvalidPath {
            path: directory.to_path_buf(),
            detail: "directory path is not valid UTF-8".to_string(),
        });
    }
    let relative = if directory.as_os_str().is_empty() || directory == Path::new(".") {
        PathBuf::new()
    } else {
        directory.to_path_buf()
    };
    let logical = project.active_root().join(&relative);
    let resolved = std::fs::canonicalize(&logical).map_err(|error| StartupBrowserError::Io {
        path: logical.clone(),
        detail: error.to_string(),
    })?;
    if !resolved.starts_with(project.active_root()) || !resolved.is_dir() {
        return Err(StartupBrowserError::InvalidPath {
            path: directory.to_path_buf(),
            detail: "directory does not resolve to a directory inside the active project"
                .to_string(),
        });
    }
    Ok((logical, relative))
}

fn browser_entry(
    project: &ActiveProject,
    logical_path: &Path,
) -> Result<StartupBrowserEntry, StartupBrowserError> {
    let name_os = logical_path.file_name().unwrap_or_default();
    let name = name_os.to_string_lossy().into_owned();
    let relative = logical_path
        .strip_prefix(project.active_root())
        .map_err(|_| StartupBrowserError::InvalidPath {
            path: logical_path.to_path_buf(),
            detail: "directory entry escaped the active project".to_string(),
        })?
        .to_path_buf();
    let path_valid_utf8 = entry_path_is_valid_utf8(name_os, &relative);
    let symlink_metadata =
        std::fs::symlink_metadata(logical_path).map_err(|error| StartupBrowserError::Io {
            path: logical_path.to_path_buf(),
            detail: error.to_string(),
        })?;
    let is_symlink = symlink_metadata.file_type().is_symlink();
    let resolved =
        std::fs::canonicalize(logical_path).unwrap_or_else(|_| logical_path.to_path_buf());
    let target_metadata = std::fs::metadata(logical_path).ok();
    let classification = if resolved.starts_with(project.active_root()) {
        StartupPathClassification::Project
    } else {
        StartupPathClassification::External
    };
    let kind = if is_symlink {
        StartupBrowserEntryKind::Symlink
    } else if symlink_metadata.is_file() {
        StartupBrowserEntryKind::File
    } else if symlink_metadata.is_dir() {
        StartupBrowserEntryKind::Directory
    } else {
        StartupBrowserEntryKind::Other
    };
    let navigable = target_metadata
        .as_ref()
        .is_some_and(|metadata| metadata.is_dir())
        && classification == StartupPathClassification::Project;
    let bytes = target_metadata
        .as_ref()
        .filter(|metadata| metadata.is_file())
        .map(std::fs::Metadata::len);
    Ok(StartupBrowserEntry {
        name,
        project_relative_path: relative,
        resolved_path: resolved,
        path_valid_utf8,
        kind,
        classification,
        navigable,
        bytes,
    })
}

fn entry_path_is_valid_utf8(name: &std::ffi::OsStr, relative: &Path) -> bool {
    name.to_str().is_some() && relative.to_str().is_some()
}

fn char_chunk(
    text: &str,
    requested_start: usize,
    max_chars: usize,
) -> (String, usize, usize, usize, Option<usize>) {
    let total_chars = text.chars().count();
    let start_char = requested_start.min(total_chars);
    let end_char = start_char.saturating_add(max_chars).min(total_chars);
    let content = text
        .chars()
        .skip(start_char)
        .take(end_char.saturating_sub(start_char))
        .collect();
    let next_start_char = (end_char < total_chars).then_some(end_char);
    (content, total_chars, start_char, end_char, next_start_char)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    use std::sync::atomic::AtomicBool;

    fn project_fixture() -> (tempfile::TempDir, StartupContext, ActiveProject) {
        let root = tempfile::tempdir().expect("temporary project");
        let state = tempfile::tempdir().expect("temporary state");
        let engine = StartupContext::from_durable_state_dir(state.keep());
        let project = engine
            .resolve_project(root.path())
            .expect("resolve project");
        (root, engine, project)
    }

    #[test]
    fn directory_pages_are_project_rooted_natural_and_honestly_paginated() {
        let (root, engine, project) = project_fixture();
        std::fs::write(root.path().join("file10.md"), "ten").unwrap();
        std::fs::write(root.path().join("file2.md"), "two").unwrap();
        std::fs::write(root.path().join("file1.md"), "one").unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();

        let first = engine
            .list_project_directory(&project, Path::new(""), 0, 2)
            .expect("first page");
        assert_eq!(first.total_entries(), 4);
        assert_eq!(first.page_start(), 0);
        assert_eq!(first.page_end(), 2);
        assert_eq!(first.next_page_start(), Some(2));
        assert_eq!(
            first
                .entries()
                .iter()
                .map(StartupBrowserEntry::name)
                .collect::<Vec<_>>(),
            vec!["file1.md", "file2.md"]
        );

        let second = engine
            .list_project_directory(&project, Path::new(""), 2, 8)
            .expect("second page");
        assert_eq!(second.next_page_start(), None);
        assert_eq!(
            second
                .entries()
                .iter()
                .map(StartupBrowserEntry::name)
                .collect::<Vec<_>>(),
            vec!["file10.md", "nested"]
        );
        assert_eq!(
            second.entries()[1].kind(),
            StartupBrowserEntryKind::Directory
        );
        assert!(second.entries()[1].navigable());

        let traversal = engine
            .list_project_directory(&project, Path::new("../"), 0, 10)
            .expect_err("parent traversal must fail");
        assert!(matches!(traversal, StartupBrowserError::InvalidPath { .. }));
    }

    #[test]
    fn search_is_recursive_ranked_bounded_and_cancellable() {
        let (root, engine, project) = project_fixture();
        std::fs::create_dir_all(root.path().join("docs/deep")).unwrap();
        std::fs::write(root.path().join("docs/plan-notes.md"), "notes").unwrap();
        std::fs::write(root.path().join("docs/deep/project-plan.md"), "plan").unwrap();
        std::fs::write(root.path().join("plan.md"), "best").unwrap();

        let cancel = AtomicBool::new(false);
        let results = engine
            .search_project_files(&project, "plan", 2, &cancel)
            .expect("search");
        assert!(!results.canceled());
        assert_eq!(results.results().len(), 2);
        assert_eq!(results.omitted_results(), 1);
        assert_eq!(results.results()[0].name(), "plan.md");
        assert!(results.visited_entries() >= 5);

        cancel.store(true, AtomicOrdering::Relaxed);
        let canceled = engine
            .search_project_files(&project, "plan", 2, &cancel)
            .expect("canceled search");
        assert!(canceled.canceled());
        assert!(canceled.results().is_empty());
    }

    #[test]
    fn current_preview_uses_complete_capture_then_returns_only_the_requested_chunk() {
        let (root, engine, project) = project_fixture();
        let text = "abcdef🤖uvwxyz";
        std::fs::write(root.path().join("PLAN.md"), text).unwrap();

        let preview = engine
            .preview_current_file(&project, Path::new("PLAN.md"), 6, 3)
            .expect("preview");
        assert_eq!(preview.content(), "🤖uv");
        assert_eq!(preview.total_chars(), text.chars().count());
        assert_eq!(preview.start_char(), 6);
        assert_eq!(preview.end_char(), 9);
        assert_eq!(preview.next_start_char(), Some(9));
        assert_eq!(preview.bytes(), text.len() as u64);
        assert_eq!(
            preview.sha256(),
            format!("{:x}", sha2::Sha256::digest(text.as_bytes()))
        );

        let outside = tempfile::NamedTempFile::new().expect("external file");
        std::fs::write(outside.path(), "outside").unwrap();
        let external = engine
            .preview_current_file(&project, outside.path(), 0, 64)
            .expect("external exact preview");
        assert_eq!(
            external.classification(),
            StartupPathClassification::External
        );
        assert_eq!(external.content(), "outside");

        std::fs::write(root.path().join("binary.bin"), b"a\0b").unwrap();
        let unsupported = engine
            .preview_current_file(&project, Path::new("binary.bin"), 0, 64)
            .expect_err("binary preview must fail");
        assert!(matches!(unsupported, StartupBrowserError::FileIssue(_)));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_directory_entries_remain_visible_but_unselectable() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let (root, engine, project) = project_fixture();
        let name = OsString::from_vec(vec![b'f', b'o', 0x80]);
        let relative = PathBuf::from(&name);
        assert!(!entry_path_is_valid_utf8(&name, &relative));
        if std::fs::write(root.path().join(&name), "opaque").is_ok() {
            let page = engine
                .list_project_directory(&project, Path::new(""), 0, 10)
                .expect("directory page");
            assert_eq!(page.entries().len(), 1);
            assert!(!page.entries()[0].path_valid_utf8());
        }
    }
}
