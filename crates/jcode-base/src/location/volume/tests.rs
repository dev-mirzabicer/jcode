use super::*;
use std::fs;
use std::sync::Mutex;

#[derive(Default)]
struct FakeVolumes(Mutex<Vec<VolumeInfo>>);
impl VolumeEnvironment for FakeVolumes {
    fn mounted(&self) -> Result<Vec<VolumeInfo>> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn containing(&self, path: &Path) -> Result<VolumeInfo> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|v| path.starts_with(&v.mount))
            .max_by_key(|v| v.mount.components().count())
            .cloned()
            .ok_or_else(|| {
                LocationError::new(LocationIssue::OfflineVolume, path, "fixture volume offline")
            })
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    env: Arc<FakeVolumes>,
    resolver: LocationResolver,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let env = Arc::new(FakeVolumes::default());
        for (name, uuid, internal) in [
            ("internal", "11111111-1111-4111-8111-111111111111", true),
            ("external", "22222222-2222-4222-8222-222222222222", false),
        ] {
            let mount = root.join(name);
            fs::create_dir(&mount).unwrap();
            env.0.lock().unwrap().push(VolumeInfo {
                identity: VolumeIdentity::parse(uuid).unwrap(),
                mount,
                label: name.into(),
                internal,
                writable: true,
                available_bytes: 123456,
            });
        }
        let resolver = LocationResolver {
            environment: env.clone(),
        };
        Self {
            _temp: temp,
            root,
            env,
            resolver,
        }
    }
    fn volume(&self, index: usize) -> VolumeInfo {
        self.env.0.lock().unwrap()[index].clone()
    }
}
fn default<'a>(home: &'a Path, saved_base: Option<&'a PathBinding>) -> CheckoutDestination<'a> {
    CheckoutDestination::Default {
        home,
        project_component: "project",
        checkout_component: "checkout",
        saved_base,
    }
}

#[test]
fn defaults_custom_saved_base_and_serialization_are_effect_free() {
    let f = Fixture::new();
    let internal = f.volume(0);
    let external = f.volume(1);
    let home = internal.mount.join("home");
    fs::create_dir(&home).unwrap();
    let a = f
        .resolver
        .checkout_destination(&internal.identity, default(&home, None))
        .unwrap();
    assert_eq!(
        a.observed_path(),
        home.join("jcode-checkouts/project/checkout")
    );
    let b = f
        .resolver
        .checkout_destination(&external.identity, default(&home, None))
        .unwrap();
    assert_eq!(
        b.observed_path(),
        external.mount.join("jcode-checkouts/project/checkout")
    );
    let custom = external.mount.join("custom/nested/new");
    let c = f
        .resolver
        .checkout_destination(&external.identity, CheckoutDestination::Custom(&custom))
        .unwrap();
    assert_eq!(c.observed_path(), custom);
    let saved = f
        .resolver
        .bind_path(&external.mount.join("saved-base"))
        .unwrap();
    let d = f
        .resolver
        .checkout_destination(&external.identity, default(&home, Some(&saved)))
        .unwrap();
    assert_eq!(
        d.observed_path(),
        external.mount.join("saved-base/project/checkout")
    );
    for binding in [a, b, c, d] {
        let encoded = serde_json::to_vec(&binding).unwrap();
        let decoded: PathBinding = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, binding);
        assert_eq!(
            f.resolver.resolve_destination(&decoded).unwrap().path,
            binding.observed_path()
        );
        assert!(!binding.observed_path().exists());
    }
    assert!(!home.join("jcode-checkouts").exists());
    assert!(!external.mount.join("jcode-checkouts").exists());
}

#[test]
fn wrong_offline_readonly_and_duplicate_volumes_never_create_paths() {
    let f = Fixture::new();
    let a = f.volume(0);
    let b = f.volume(1);
    let target = b.mount.join("new");
    assert_eq!(
        f.resolver
            .checkout_destination(&a.identity, CheckoutDestination::Custom(&target))
            .unwrap_err()
            .kind,
        LocationIssue::WrongVolume
    );
    f.env.0.lock().unwrap()[1].writable = false;
    assert_eq!(
        f.resolver
            .checkout_destination(&b.identity, CheckoutDestination::Custom(&target))
            .unwrap_err()
            .kind,
        LocationIssue::ReadOnlyVolume
    );
    f.env.0.lock().unwrap()[1].writable = true;
    f.env.0.lock().unwrap().push(b.clone());
    assert_eq!(
        f.resolver.volume(&b.identity).unwrap_err().kind,
        LocationIssue::AmbiguousVolume
    );
    f.env.0.lock().unwrap().truncate(1);
    fs::remove_dir(&b.mount).unwrap();
    assert_eq!(
        f.resolver
            .checkout_destination(&b.identity, CheckoutDestination::Custom(&target))
            .unwrap_err()
            .kind,
        LocationIssue::OfflineVolume
    );
    assert!(!b.mount.exists());
}

#[test]
fn remount_follows_uuid_but_replaced_roots_require_rebind() {
    let f = Fixture::new();
    let volume = f.volume(1);
    let directory = volume.mount.join("project");
    fs::create_dir(&directory).unwrap();
    let binding = f.resolver.bind_directory(&directory).unwrap();
    let renamed = f.root.join("renamed-volume");
    fs::rename(&volume.mount, &renamed).unwrap();
    f.env.0.lock().unwrap()[1].mount = renamed.clone();
    let resolved = f.resolver.resolve_directory(&binding).unwrap();
    assert_eq!(resolved.path, renamed.join("project"));
    assert!(resolved.relocated);
    assert!(!volume.mount.exists());
    fs::rename(renamed.join("project"), renamed.join("old-project")).unwrap();
    fs::create_dir(renamed.join("project")).unwrap();
    assert_eq!(
        f.resolver.resolve_directory(&binding).unwrap_err().kind,
        LocationIssue::ReplacedRoot
    );
}

#[cfg(unix)]
#[test]
fn aliases_dangling_links_and_late_symlink_substitution() {
    let f = Fixture::new();
    let volume = f.volume(1);
    let directory = volume.mount.join("project");
    fs::create_dir(&directory).unwrap();
    let alias = f.root.join("alias");
    std::os::unix::fs::symlink(&directory, &alias).unwrap();
    let bound = f.resolver.bind_directory(&directory).unwrap();
    assert_eq!(f.resolver.bind_directory(&alias).unwrap(), bound);
    let target = alias.join("new/sub");
    let pending = f
        .resolver
        .checkout_destination(&volume.identity, CheckoutDestination::Custom(&target))
        .unwrap();
    assert_eq!(pending.observed_path(), directory.join("new/sub"));
    std::os::unix::fs::symlink(f.volume(0).mount, directory.join("new")).unwrap();
    assert_eq!(
        f.resolver.resolve_destination(&pending).unwrap_err().kind,
        LocationIssue::ReplacedRoot
    );
    std::os::unix::fs::symlink(directory.join("missing"), directory.join("dangling")).unwrap();
    assert!(
        f.resolver
            .bind_path(&directory.join("dangling/new"))
            .is_err()
    );
}

#[test]
fn existing_destinations_malformed_bindings_and_names_fail_closed() {
    let f = Fixture::new();
    let volume = f.volume(0);
    let target = volume.mount.join("new");
    let binding = f
        .resolver
        .checkout_destination(&volume.identity, CheckoutDestination::Custom(&target))
        .unwrap();
    fs::create_dir(&target).unwrap();
    assert_eq!(
        f.resolver.resolve_destination(&binding).unwrap_err().kind,
        LocationIssue::AlreadyExists
    );
    for suffix in ["../escape", "/absolute"] {
        let mut json = serde_json::to_value(&binding).unwrap();
        json["suffix"] = suffix.into();
        let bad: PathBinding = serde_json::from_value(json).unwrap();
        assert_eq!(
            f.resolver.resolve_path(&bad).unwrap_err().kind,
            LocationIssue::InvalidPath
        );
    }
    for component in ["", "..", "../bad", "a/b", "/root", "x\0y"] {
        let result = f.resolver.checkout_destination(
            &volume.identity,
            CheckoutDestination::Default {
                home: &volume.mount,
                project_component: component,
                checkout_component: "new",
                saved_base: None,
            },
        );
        assert!(result.is_err(), "accepted {component:?}");
    }
    assert!(VolumeIdentity::parse("not-a-uuid").is_err());
}

#[test]
fn newly_mounted_nested_volume_cannot_inherit_destination_binding() {
    let f = Fixture::new();
    let volume = f.volume(0);
    let target = volume.mount.join("nested/checkout");
    let binding = f
        .resolver
        .checkout_destination(&volume.identity, CheckoutDestination::Custom(&target))
        .unwrap();
    fs::create_dir(volume.mount.join("nested")).unwrap();
    let mut nested = f.volume(1);
    nested.mount = volume.mount.join("nested");
    f.env.0.lock().unwrap().push(nested);
    assert_eq!(
        f.resolver.resolve_destination(&binding).unwrap_err().kind,
        LocationIssue::WrongVolume
    );
}

#[cfg(target_os = "macos")]
#[test]
fn native_internal_volume_binding_and_readonly_mount() {
    let resolver = LocationResolver::new();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let volume = resolver.containing_volume(&root).unwrap();
    assert!(volume.internal);
    assert!(volume.writable);
    assert!(volume.available_bytes > 0);
    let binding = resolver.bind_directory(&root).unwrap();
    assert_eq!(resolver.resolve_directory(&binding).unwrap().path, root);
    let destination = resolver
        .checkout_destination(&volume.identity, default(&root, None))
        .unwrap();
    assert_eq!(
        destination.observed_path(),
        root.join("jcode-checkouts/project/checkout")
    );
    assert!(!destination.observed_path().exists());
    let system = resolver.containing_volume(Path::new("/")).unwrap();
    assert!(!system.writable);
    assert_eq!(
        resolver
            .checkout_destination(
                &system.identity,
                CheckoutDestination::Custom(Path::new("/jcode-test-must-not-exist"))
            )
            .unwrap_err()
            .kind,
        LocationIssue::ReadOnlyVolume
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires explicit external volume mount and UUID; creates only an owned tempfile fixture"]
fn native_external_volume_fixture() {
    let mount =
        PathBuf::from(std::env::var_os("JCODE_LOCATION_FIXTURE_MOUNT").expect("explicit mount"));
    let uuid =
        VolumeIdentity::parse(std::env::var("JCODE_LOCATION_FIXTURE_UUID").expect("explicit UUID"))
            .unwrap();
    let resolver = LocationResolver::new();
    let volume = resolver.volume(&uuid).unwrap();
    assert_eq!(volume.mount, mount.canonicalize().unwrap());
    assert!(!volume.internal);
    assert!(volume.writable);
    let fixture = tempfile::Builder::new()
        .prefix("jcode-location-fixture-")
        .tempdir_in(&mount)
        .unwrap();
    let path = fixture.path().canonicalize().unwrap();
    let binding = resolver.bind_directory(&path).unwrap();
    assert_eq!(binding.volume(), &uuid);
    assert_eq!(resolver.resolve_directory(&binding).unwrap().path, path);
    fs::write(
        path.join("owned-sentinel"),
        b"owned native volume fixture\n",
    )
    .unwrap();
    assert_eq!(
        fs::read(path.join("owned-sentinel")).unwrap(),
        b"owned native volume fixture\n"
    );
    let custom = resolver
        .checkout_destination(&uuid, CheckoutDestination::Custom(&path.join("custom/new")))
        .unwrap();
    assert_eq!(
        resolver.resolve_destination(&custom).unwrap().path,
        path.join("custom/new")
    );
    let external_default = resolver
        .checkout_destination(&uuid, default(&path, None))
        .unwrap();
    assert_eq!(
        external_default.observed_path(),
        mount.join("jcode-checkouts/project/checkout")
    );
    assert!(!path.join("custom").exists());
    println!(
        "verified external UUID={} fixture={} free_bytes={}",
        uuid.as_str(),
        path.display(),
        volume.available_bytes
    );
    fixture.close().unwrap();
    assert!(!path.exists());
}
