#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::fs;

    fn acquire(paths: &[PathBuf]) -> Result<VerifiedFiles> {
        let resolved = paths.iter().map(|p| Ok((p.clone(), resolve_target(p)?))).collect::<Result<Vec<_>>>()?;
        VerifiedFiles::acquire(&resolved)
    }
    #[test]
    fn native_files_create_edit_delete_preserve_mode_and_all_destination_admission() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("new/child/a");
        let b = temp.path().join("b");
        fs::write(&b, "old").unwrap();
        fs::set_permissions(&b, fs::Permissions::from_mode(0o751)).unwrap();
        let mut permit = acquire(&[a.clone(), b.clone()]).unwrap();
        assert!(permit.read(&a).unwrap().is_none());
        permit.write(&a, b"new").unwrap();
        permit.write(&b, b"modified").unwrap();
        assert_eq!(permit.read(&b).unwrap().unwrap(), b"modified");
        assert_eq!(fs::metadata(&b).unwrap().permissions().mode() & 0o777, 0o751);
        permit.remove(&a).unwrap();
        permit.write(&a, b"recreated").unwrap();
        assert_eq!(fs::read(&a).unwrap(), b"recreated");
        assert!(permit.write(&temp.path().join("unadmitted"), b"no").is_err());
        drop(permit);
        let late = temp.path().join("directory");
        fs::create_dir(&late).unwrap();
        let early = temp.path().join("absent/early");
        assert!(acquire(&[early.clone(), late]).is_err());
        assert!(!early.parent().unwrap().exists());
    }
    #[test]
    fn native_files_symlink_targets_are_pinned_and_retargeting_never_writes_new_target() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        let alias = temp.path().join("alias");
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        symlink(&a, &alias).unwrap();
        let mut permit = acquire(std::slice::from_ref(&alias)).unwrap();
        permit.write(&alias, b"allowed").unwrap();
        assert!(fs::symlink_metadata(&alias).unwrap().file_type().is_symlink());
        fs::remove_file(&alias).unwrap();
        symlink(&b, &alias).unwrap();
        assert!(permit.write(&alias, b"escape").is_err());
        assert_eq!(fs::read(&b).unwrap(), b"b");
        drop(permit);
        fs::remove_file(&alias).unwrap();
        let absent = temp.path().join("absent/new");
        symlink(&absent, &alias).unwrap();
        let mut permit = acquire(std::slice::from_ref(&alias)).unwrap();
        permit.write(&alias, b"created through exact target").unwrap();
        permit.remove(&alias).unwrap();
        assert_eq!(fs::read(&absent).unwrap(), b"created through exact target");
        assert!(fs::symlink_metadata(&alias).is_err());
        permit.write(&alias, b"again").unwrap();
        assert_eq!(fs::read(&alias).unwrap(), b"again");
        assert_eq!(fs::read(&absent).unwrap(), b"created through exact target");
    }
    #[test]
    fn native_files_reject_hardlinks_replacements_and_concurrent_changes() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let alias = temp.path().join("alias");
        fs::write(&a, "original").unwrap();
        fs::hard_link(&a, &alias).unwrap();
        assert!(acquire(std::slice::from_ref(&a)).is_err());
        fs::remove_file(&alias).unwrap();
        let mut permit = acquire(std::slice::from_ref(&a)).unwrap();
        fs::hard_link(&a, &alias).unwrap();
        assert!(permit.write(&a, b"no").is_err());
        fs::remove_file(&alias).unwrap();
        fs::rename(&a, &alias).unwrap();
        fs::write(&a, "replacement").unwrap();
        assert!(permit.write(&a, b"no").is_err());
        assert_eq!(fs::read(&a).unwrap(), b"replacement");
        drop(permit);
        let mut permit = acquire(std::slice::from_ref(&a)).unwrap();
        fs::write(&a, "changed").unwrap();
        assert!(permit.write(&a, b"no").is_err());
        assert_eq!(fs::read(&a).unwrap(), b"changed");
    }
    #[test]
    fn native_files_parent_replacement_and_missing_parent_symlink_fail_before_file_effect() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let a = root.join("new/a");
        let mut permit = acquire(std::slice::from_ref(&a)).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, root.join("new")).unwrap();
        assert!(permit.write(&a, b"no").is_err());
        assert!(!outside.join("a").exists());
        fs::remove_file(root.join("new")).unwrap();
        fs::rename(&root, temp.path().join("moved")).unwrap();
        fs::create_dir(&root).unwrap();
        assert!(permit.write(&a, b"no").is_err());
        assert!(!root.join("new").exists());
    }
    #[test]
    fn native_files_exact_file_leases_serialize_aliases_but_not_other_files() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        fs::write(&a, "a").unwrap();
        let alias = temp.path().join("alias");
        symlink(&a, &alias).unwrap();
        let first = acquire(&[a]).unwrap();
        assert!(acquire(std::slice::from_ref(&alias)).is_err());
        let mut other = acquire(&[temp.path().join("b")]).unwrap();
        other.write(&temp.path().join("b"), b"b").unwrap();
        drop(first);
        assert!(acquire(&[alias]).is_ok());
    }
    #[test]
    fn native_files_moves_preserve_mode_and_extended_attributes() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::write(&a, "source").unwrap();
        fs::set_permissions(&a, fs::Permissions::from_mode(0o751)).unwrap();
        assert!(std::process::Command::new("/usr/bin/xattr").args(["-w", "com.jcode.fixture", "metadata"]).arg(&a).status().unwrap().success());
        let mut permit = acquire(&[a.clone(), b.clone()]).unwrap();
        permit.write(&b, b"updated source").unwrap();
        permit.copy_metadata(&a, &b).unwrap();
        permit.remove(&a).unwrap();
        assert_eq!(fs::read(&b).unwrap(), b"updated source");
        assert_eq!(fs::metadata(&b).unwrap().permissions().mode() & 0o777, 0o751);
        let attr = std::process::Command::new("/usr/bin/xattr").args(["-p", "com.jcode.fixture"]).arg(&b).output().unwrap();
        assert!(attr.status.success());
        assert_eq!(attr.stdout, b"metadata\n");
    }
}

#[cfg(all(test, target_os = "macos"))]
#[test]
fn native_files_follow_actual_case_and_unicode_alias_identity() {
    use std::os::unix::fs::MetadataExt;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("Café.txt");
    std::fs::write(&path, "before").unwrap();
    for alias in [temp.path().join("CAFÉ.TXT"), temp.path().join("Cafe\u{301}.txt")] {
        if !alias.exists() { continue; } // This volume's actual alias semantics, not guessed case folding.
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), std::fs::metadata(&alias).unwrap().ino());
        let paths = vec![(path.clone(), resolve_target(&path).unwrap()), (alias.clone(), resolve_target(&alias).unwrap())];
        let mut permit = VerifiedFiles::acquire(&paths).unwrap();
        assert!(permit.same_file(&path, &alias).unwrap());
        permit.write(&alias, b"updated through alias").unwrap();
        assert_eq!(permit.read(&path).unwrap().unwrap(), b"updated through alias");
    }
}

#[cfg(all(test, target_os = "macos"))]
#[test]
fn removal_only_directory_references_and_ordered_recreation_preserve_contents() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("directory");
    std::fs::create_dir(&directory).unwrap();
    let sentinel = directory.join("sentinel");
    std::fs::write(&sentinel, "retained").unwrap();
    let link = temp.path().join("link");
    symlink(&directory, &link).unwrap();
    let mut plan = NativeFilePlan::default();
    plan.remove(link.clone()); plan.file(link.clone());
    let paths = [(link.clone(), resolve_target(&link).unwrap())];
    let mut permit = VerifiedFiles::acquire_plan(&paths, &plan).unwrap();
    assert!(permit.write(&link, b"not before unlink").is_err());
    permit.remove(&link).unwrap();
    permit.write(&link, b"new regular file").unwrap();
    assert_eq!(std::fs::read(&link).unwrap(), b"new regular file");
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"retained");
    drop(permit);
    let root_link = temp.path().join("root-link");
    symlink("/", &root_link).unwrap();
    let plan = NativeFilePlan::new(Vec::new(),vec![root_link.clone()]);
    let mut permit = VerifiedFiles::acquire_plan(&[(root_link.clone(),resolve_target(&root_link).unwrap())],&plan).unwrap();
    permit.remove(&root_link).unwrap();
    assert!(std::fs::symlink_metadata(&root_link).is_err());
    assert!(Path::new("/").is_dir());
    assert!(VerifiedFiles::acquire_plan(&[(directory.clone(),resolve_target(&directory).unwrap())],&NativeFilePlan::new(Vec::new(),vec![directory])).is_err());
}
