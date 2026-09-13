use super::*;
#[path = "retention_tests.rs"]
mod retention_tests;
use crate::execution::{Invocation, PreparedInvocation};
use std::sync::{
    Mutex,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};

struct Environment {
    archive: PathBuf,
    capacity: AtomicU8,
    fault: AtomicU8,
    writes: AtomicUsize,
    first: Mutex<Vec<u8>>,
    fail_stage: Mutex<Option<&'static str>>,
}
impl StorageEnvironment for Environment {
    fn available(&self, path: &Path) -> Result<u64> {
        Ok(match self.capacity.load(Ordering::SeqCst) {
            2 => 0,
            1 if !path.starts_with(&self.archive) => 0,
            _ => u64::MAX,
        })
    }
    fn archive(&self, _: &ArchiveConfig) -> Result<DirectoryBinding> {
        ensure!(self.archive.is_dir(), "fixture archive offline");
        DirectoryBinding::open(&self.archive)
    }
    fn checkpoint(&self, stage: &str) -> Result<()> {
        let mut fault = self.fail_stage.lock().unwrap();
        if *fault == Some(stage) {
            *fault = None;
            bail!("injected {stage}");
        }
        Ok(())
    }
    fn write(&self, _: &Path, file: &mut File, bytes: &[u8]) -> std::io::Result<usize> {
        let fault = self.fault.load(Ordering::SeqCst);
        let step = self.writes.fetch_add(1, Ordering::SeqCst);
        if fault != 0 && step == 0 {
            let n = file.write(&bytes[..bytes.len().min(8)])?;
            *self.first.lock().unwrap() = bytes[..n].to_vec();
            return Ok(n);
        }
        if step == 1 {
            match fault {
                1 => return Err(std::io::Error::from_raw_os_error(libc::EIO)),
                2 => return Err(std::io::Error::from_raw_os_error(libc::ENOSPC)),
                _ => {}
            }
        }
        file.write(bytes)
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    store: ExecutionStore,
    record: RunRecord,
    config: StorageConfig,
    environment: Arc<Environment>,
}
impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let archive = directory.path().join("archive");
        std::fs::create_dir(&archive)?;
        let store = ExecutionStore::open(directory.path())?;
        let call = Invocation {
            session_id: "storage".into(),
            message_id: "message".into(),
            call_path: vec!["call".into()],
            tool: "fixture".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(record) = store.prepare(&call, "owner")? else {
            panic!()
        };
        store.start(&record.id, "owner")?;
        let config = StorageConfig {
            local_reserve_bytes: 1,
            archive_reserve_bytes: 1,
            archive: Some(ArchiveConfig {
                mount: archive.clone(),
                directory: "fixture".into(),
                volume_uuid: "fixture".into(),
            }),
        };
        let environment = Arc::new(Environment {
            archive,
            capacity: AtomicU8::new(0),
            fault: AtomicU8::new(0),
            writes: AtomicUsize::new(0),
            first: Mutex::new(Vec::new()),
            fail_stage: Mutex::new(None),
        });
        Ok(Self {
            _directory: directory,
            store,
            record,
            config,
            environment,
        })
    }
    fn bundle(&self) -> Result<BundleStorage> {
        BundleStorage::create_with_environment(
            self.store.clone(),
            &self.record,
            self.config.clone(),
            self.environment.clone(),
        )
    }
    fn journal(&self, bundle: &BundleStorage) -> Result<MoveManifest> {
        let root = archive_namespace(&self.store, &self.config, self.environment.as_ref())?;
        let destination = root.path.join(format!("{}-fixture", self.record.id));
        let manifest = MoveManifest::capture(&self.record.id, &bundle.physical, &destination)?;
        let path = self.store.root().join("fixture-move.json");
        crate::storage::write_json_secret(&path, &manifest)?;
        self.store.connection()?.execute("INSERT INTO relocations (id,source,destination,stage,manifest_path) VALUES (?1,?2,?3,'copying',?4)", params![manifest.id,manifest.source.to_str(),manifest.destination.to_str(),path.to_str()])?;
        Ok(manifest)
    }
}

#[test]
fn json_write_failure_preserves_prefix_without_drop_retry() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut bundle = fixture.bundle()?;
    fixture.environment.fault.store(1, Ordering::SeqCst);
    let result = bundle.write_json(
        "large.json",
        &serde_json::json!({"data":"x".repeat(100_000)}),
    );
    assert!(result.is_err());
    let stages: Vec<_> = std::fs::read_dir(&bundle.physical)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("stage-"))
        .collect();
    assert_eq!(stages.len(), 1);
    let actual = std::fs::read(stages[0].path())?;
    let expected = fixture.environment.first.lock().unwrap();
    assert!(
        actual == *expected,
        "failed writer retried bytes: retained {} bytes, expected {}",
        actual.len(),
        expected.len()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn owned_allocation_recovery_is_targeted_and_idempotent() -> Result<()> {
    for stage in [
        "allocation_before_files",
        "allocation_after_files",
        "allocation_after_location",
        "allocation_after_alias",
    ] {
        let fixture = Fixture::new()?;
        *fixture.environment.fail_stage.lock().unwrap() = Some(stage);
        assert!(fixture.bundle().is_err());
        let lease = output_lease(&fixture.store, &fixture.record.id)?;
        fixture
            .store
            .recover_owned_storage(&fixture.record.id, &lease)?;
        fixture
            .store
            .recover_owned_storage(&fixture.record.id, &lease)?;
        let record = fixture.store.inspect(&fixture.record.id)?.unwrap();
        assert_eq!(record.state, crate::execution::RunState::Running);
        assert_eq!(std::fs::read(record.output_path.unwrap())?, b"");
    }
    Ok(())
}

#[test]
fn partial_enospc_spills_without_duplicate_bytes() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut bundle = fixture.bundle()?;
    fixture.environment.fault.store(2, Ordering::SeqCst);
    let bytes = b"a complete output with an exact preserved prefix";
    bundle.append("output.txt", bytes)?;
    assert!(bundle.archived);
    assert_eq!(std::fs::read(bundle.alias().join("output.txt"))?, bytes);
    Ok(())
}

#[cfg(unix)]
#[test]
fn relocation_rejects_a_matching_destination_symlink() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut bundle = fixture.bundle()?;
    bundle.append("output.txt", b"retained")?;
    let manifest = fixture.journal(&bundle)?;
    std::fs::create_dir(&manifest.destination)?;
    let unrelated = fixture._directory.path().join("unrelated");
    std::fs::write(&unrelated, b"retained")?;
    std::os::unix::fs::symlink(&unrelated, manifest.destination.join("output.txt"))?;
    let result = complete_move(&fixture.store, &manifest, fixture.environment.as_ref());
    assert!(
        result.is_err(),
        "matching bytes do not establish destination ownership"
    );
    assert_eq!(std::fs::read(&unrelated)?, b"retained");
    assert!(manifest.source.exists());
    Ok(())
}

#[test]
fn relocation_rejects_an_unexpected_current_location() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut bundle = fixture.bundle()?;
    bundle.append("output.txt", b"retained")?;
    let manifest = fixture.journal(&bundle)?;
    let unrelated = fixture._directory.path().join("different-location");
    fixture.store.connection()?.execute(
        "UPDATE output_locations SET physical=?2 WHERE id=?1",
        params![fixture.record.id, unrelated.to_str()],
    )?;
    let result = complete_move(&fixture.store, &manifest, fixture.environment.as_ref());
    assert!(
        result.is_err(),
        "a move cannot replace an unrelated current location"
    );
    assert!(manifest.source.exists());
    Ok(())
}

#[test]
fn relocation_resumes_only_an_exact_destination_prefix() -> Result<()> {
    for matches in [false, true] {
        let fixture = Fixture::new()?;
        let mut bundle = fixture.bundle()?;
        bundle.append("output.txt", b"original full output")?;
        let manifest = fixture.journal(&bundle)?;
        std::fs::create_dir(&manifest.destination)?;
        let path = manifest.destination.join("output.txt");
        let prefix = if matches {
            b"original".as_slice()
        } else {
            b"unrelated".as_slice()
        };
        std::fs::write(&path, prefix)?;
        let result = complete_move(&fixture.store, &manifest, fixture.environment.as_ref());
        if matches {
            result?;
            assert_eq!(std::fs::read(path)?, b"original full output");
        } else {
            assert!(result.is_err());
            assert_eq!(std::fs::read(path)?, prefix);
            assert!(manifest.source.exists());
        }
    }
    Ok(())
}

#[test]
fn archive_path_replacement_is_not_a_valid_writer_destination() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.environment.capacity.store(1, Ordering::SeqCst);
    let mut bundle = fixture.bundle()?;
    bundle.append("output.txt", b"original")?;
    let offline = fixture._directory.path().join("offline");
    std::fs::rename(&fixture.environment.archive, &offline)?;
    std::fs::create_dir_all(&bundle.physical)?;
    let fake = bundle.physical.join("output.txt");
    std::fs::write(&fake, b"unrelated")?;
    assert!(bundle.append("output.txt", b"must-not-land-here").is_err());
    assert!(bundle.write_file("empty.bin", b"").is_err());
    assert_eq!(
        std::fs::read_dir(&bundle.physical)?.count(),
        1,
        "failed empty writes must not create files in a replacement directory"
    );
    assert_eq!(std::fs::read(fake)?, b"unrelated");
    Ok(())
}

#[test]
fn every_initial_publication_boundary_recovers_without_reexecution() -> Result<()> {
    for stage in [
        "allocation_before_files",
        "allocation_after_files",
        "allocation_after_location",
        "allocation_after_alias",
    ] {
        let fixture = Fixture::new()?;
        *fixture.environment.fail_stage.lock().unwrap() = Some(stage);
        assert!(fixture.bundle().is_err(), "{stage}");
        let reopened = ExecutionStore::open(fixture._directory.path())?;
        assert_eq!(
            reopened.recover_output_storage_with_environment(
                &fixture.config,
                fixture.environment.as_ref()
            )?,
            1,
            "{stage}"
        );
        assert_eq!(
            reopened.recover_output_storage_with_environment(
                &fixture.config,
                fixture.environment.as_ref()
            )?,
            0
        );
        let record = reopened.inspect(&fixture.record.id)?.unwrap();
        assert_eq!(std::fs::read(record.output_path.unwrap())?, b"");
        assert_eq!(reopened.list("storage", None, 10)?.len(), 1);
        assert_eq!(
            record.state,
            crate::execution::RunState::Running,
            "storage recovery must not invent producer completion"
        );
    }
    Ok(())
}

#[test]
fn every_move_boundary_recovers_exact_bytes_and_publishes_once() -> Result<()> {
    for stage in [
        "before_copy",
        "after_copy",
        "after_publish",
        "after_alias",
        "after_delete",
    ] {
        let fixture = Fixture::new()?;
        let mut bundle = fixture.bundle()?;
        let original = b"exact retained output before interruption";
        bundle.append("output.txt", original)?;
        let local = bundle.physical.clone();
        *fixture.environment.fail_stage.lock().unwrap() = Some(stage);
        fixture.environment.capacity.store(1, Ordering::SeqCst);
        assert!(
            bundle.append("output.txt", b"unwritten suffix").is_err(),
            "{stage}"
        );
        drop(bundle);
        let reopened = ExecutionStore::open(fixture._directory.path())?;
        assert_eq!(
            reopened.recover_output_storage_with_environment(
                &fixture.config,
                fixture.environment.as_ref()
            )?,
            1,
            "{stage}"
        );
        assert_eq!(
            reopened.recover_output_storage_with_environment(
                &fixture.config,
                fixture.environment.as_ref()
            )?,
            0
        );
        let output = reopened
            .inspect(&fixture.record.id)?
            .unwrap()
            .output_path
            .unwrap();
        assert_eq!(std::fs::read(output)?, original, "{stage}");
        assert!(!local.exists(), "{stage}");
        let generation: i64 = reopened.connection()?.query_row(
            "SELECT generation FROM output_locations WHERE id=?1",
            [&fixture.record.id],
            |row| row.get(0),
        )?;
        assert_eq!(
            generation, 1,
            "duplicate recovery must not republish {stage}"
        );
    }
    Ok(())
}

#[test]
fn exhausting_all_storage_preserves_existing_prefix() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut bundle = fixture.bundle()?;
    bundle.append("output.txt", b"retained prefix")?;
    fixture.environment.capacity.store(2, Ordering::SeqCst);
    assert!(bundle.append("output.txt", b"not retained").is_err());
    assert_eq!(
        std::fs::read(bundle.alias().join("output.txt"))?,
        b"retained prefix"
    );
    Ok(())
}

#[test]
fn absent_archive_does_not_create_a_fake_mount() -> Result<()> {
    let fixture = Fixture::new()?;
    let mount = fixture._directory.path().join("absent-volume");
    let config = ArchiveConfig {
        mount: mount.clone(),
        volume_uuid: "missing".into(),
        directory: "jcode".into(),
    };
    assert!(verified_archive(&config).is_err());
    assert!(!mount.exists());
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn wrong_native_volume_identity_rejects_before_archive_creation() -> Result<()> {
    let fixture = Fixture::new()?;
    let config = ArchiveConfig {
        mount: fixture._directory.path().to_path_buf(),
        volume_uuid: "00000000-0000-0000-0000-000000000000".into(),
        directory: "must-not-create".into(),
    };
    assert!(verified_archive(&config).is_err());
    assert!(!config.mount.join(config.directory).exists());
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires explicit owned fixture configuration on a verified archive volume"]
fn native_archive_fixture_uses_verified_volume_and_stable_aliases() -> Result<()> {
    let path = std::env::var_os("JCODE_EXECUTION_ARCHIVE_FIXTURE")
        .context("Missing explicit archive fixture configuration")?;
    let archive: ArchiveConfig = crate::storage::read_json(Path::new(&path))?;
    ensure!(
        archive.directory.components().count() == 1
            && archive
                .directory
                .to_string_lossy()
                .starts_with("jcode-execution-fixture-"),
        "Archive test must use an explicitly isolated fixture directory"
    );
    ensure!(
        !archive.mount.join(&archive.directory).exists(),
        "Archive fixture already exists; do not overwrite it"
    );
    let root = verified_archive(&archive)?;
    let result = (|| -> Result<()> {
        let fixture = Fixture::new()?;
        let config = StorageConfig {
            local_reserve_bytes: 0,
            archive_reserve_bytes: 1024 * 1024,
            archive: Some(archive.clone()),
        };
        let mut bundle = BundleStorage::create(fixture.store.clone(), &fixture.record, config)?;
        let local = bundle.physical.clone();
        bundle.append("output.txt", b"before-")?;
        // Force the reserve policy, not actual host disk exhaustion. Copy,
        // fsync, alias publication and subsequent writes use the real SSD.
        bundle.config.local_reserve_bytes = u64::MAX;
        bundle.append("output.txt", b"after")?;
        ensure!(
            bundle.archived && !local.exists(),
            "Writer did not relocate to the verified archive"
        );
        ensure!(
            std::fs::read(bundle.alias().join("output.txt"))? == b"before-after",
            "Real relocation lost or duplicated bytes"
        );
        let location = fixture
            .store
            .output_location(&fixture.record.id)?
            .context("Missing archived location")?
            .0;
        ensure!(
            location.starts_with(&root.path),
            "Output escaped the fixture archive"
        );
        let second = Invocation {
            session_id: "native-fixture".into(),
            message_id: "second".into(),
            call_path: vec!["call".into()],
            tool: "fixture".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(second) = fixture.store.prepare(&second, "owner")? else {
            bail!("Unexpected duplicate fixture");
        };
        fixture.store.start(&second.id, "owner")?;
        let mut second =
            BundleStorage::create(fixture.store.clone(), &second, bundle.config.clone())?;
        second.append("output.txt", b"initial-placement")?;
        ensure!(
            second.archived,
            "Initial emergency placement did not use archive"
        );
        ensure!(
            std::fs::read(second.alias().join("output.txt"))? == b"initial-placement",
            "Initial archive bytes changed"
        );
        use jcode_tool_core::{OutputCapture, OutputStream};
        use jcode_tool_types::{OutputSource, ToolOutput};
        let input = Invocation {
            session_id: "native-managed-read".into(),
            message_id: "message".into(),
            call_path: vec!["call".into()],
            tool: "fixture".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(record) = fixture.store.prepare(&input, "owner")? else {
            bail!("Duplicate read fixture");
        };
        fixture.store.start(&record.id, "owner")?;
        let capture = crate::execution::Capture::create(
            fixture.store.clone(),
            record.clone(),
            StorageConfig {
                local_reserve_bytes: 0,
                archive: Some(archive.clone()),
                ..Default::default()
            },
        )?;
        capture.write(
            OutputStream::Text,
            format!("{}TAIL", "α".repeat(500)).as_bytes(),
        )?;
        let alias = capture.reference()?.path;
        let image_data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
        let mut output = ToolOutput::new("").with_image("image/png", image_data);
        output.source = OutputSource::Retained(capture.reference()?);
        capture.seal(output, crate::execution::RunState::Completed)?;
        drop(capture);
        let image_alias = alias.with_file_name("image-0.bin");
        let original_image = fixture
            .store
            .retained_image(&image_alias, 20 * 1024 * 1024)?;
        let image_record = fixture.store.inspect(&record.id)?.unwrap();
        let first_image_page =
            fixture
                .store
                .read_part_page(&image_record, "image-0.bin", 0, 32, None)?;
        let state_root = fixture
            .store
            .root()
            .parent()
            .context("Missing fixture state root")?;
        let reader = crate::execution::reader::SourceReader::new(state_root);
        let request = |point| crate::execution::reader::ReadRequest {
            path: alias.clone(),
            point,
            start_line: 1,
            end_line: None,
            target: std::num::NonZeroUsize::new(100).unwrap(),
            stop: None,
        };
        let first = reader.read(request(None))?;
        let metadata_path = alias.with_file_name("manifest.json");
        let mut metadata_request = request(None);
        metadata_request.path = metadata_path.clone();
        let first_metadata = reader.read(metadata_request)?;
        let OutputSource::ReadPage(metadata_page) = first_metadata.source else {
            bail!("Missing metadata read point");
        };
        let OutputSource::ReadPage(page) = first.source else {
            bail!("Expected managed read page");
        };
        let source = fixture.store.output_location(&record.id)?.unwrap().0;
        let namespace = archive_namespace(
            &fixture.store,
            &StorageConfig {
                archive: Some(archive.clone()),
                ..Default::default()
            },
            &NativeEnvironment,
        )?;
        let destination = namespace.path.join(format!("managed-read-{}", record.id));
        let mut manifest = MoveManifest::capture(&record.id, &source, &destination)?;
        manifest.archive_spec = Some(archive.clone());
        let journal = fixture
            .store
            .root()
            .join("moves")
            .join(format!("{}-read.json", record.id));
        crate::storage::write_json_secret(&journal, &manifest)?;
        fixture.store.connection()?.execute("INSERT INTO relocations(id,source,destination,stage,manifest_path) VALUES (?1,?2,?3,'copying',?4)",params![record.id,source.to_str(),destination.to_str(),journal.to_str()])?;
        let lease = output_lease(&fixture.store, &record.id)?;
        fixture.store.recover_owned_storage(&record.id, &lease)?;
        fixture.store.recover_owned_storage(&record.id, &lease)?;
        drop(lease);
        ensure!(
            fixture
                .store
                .retained_image(&image_alias, 20 * 1024 * 1024)?
                == original_image,
            "Archived image identity or bytes changed"
        );
        let rest = fixture.store.read_part_page(
            &image_record,
            "image-0.bin",
            first_image_page.next_offset.unwrap(),
            1024,
            Some(&first_image_page.sha256),
        )?;
        use base64::Engine;
        let mut reassembled =
            base64::engine::general_purpose::STANDARD.decode(&first_image_page.data_base64)?;
        reassembled.extend(base64::engine::general_purpose::STANDARD.decode(&rest.data_base64)?);
        ensure!(
            reassembled == original_image.0,
            "Binary part continuation changed after relocation"
        );
        let mut next = request(page.next_point.clone());
        next.target = std::num::NonZeroUsize::new(10_000).unwrap();
        let continued = reader.read(next)?;
        let mut metadata_request = request(metadata_page.next_point.clone());
        metadata_request.path = metadata_path;
        metadata_request.target = std::num::NonZeroUsize::new(100_000).unwrap();
        let continued_metadata = reader.read(metadata_request)?;
        let OutputSource::ReadPage(continued_metadata_page) = continued_metadata.source else {
            bail!("Missing continued metadata point");
        };
        ensure!(
            continued_metadata_page.start_byte == metadata_page.end_byte
                && continued_metadata.output.contains("image-0.base64"),
            "Metadata read point changed after relocation"
        );
        let OutputSource::ReadPage(continued_page) = continued.source else {
            bail!("Expected continued page");
        };
        ensure!(
            continued_page.start_byte == page.end_byte && continued.output.contains("TAIL"),
            "Relocation retargeted the read point"
        );
        ensure!(
            !source.exists()
                && fixture.store.output_location(&record.id)?.unwrap().0 == destination,
            "Read moved archived output back"
        );
        let offline = ArchiveConfig {
            mount: fixture.store.root().join("not-mounted"),
            ..archive.clone()
        };
        fixture.store.connection()?.execute(
            "UPDATE output_locations SET archive_spec=?2 WHERE id=?1",
            params![record.id, serde_json::to_string(&offline)?],
        )?;
        ensure!(
            fixture
                .store
                .retained_image(&image_alias, 20 * 1024 * 1024)
                .is_err(),
            "Offline image archive must not be read through an unverified path"
        );
        ensure!(
            reader.read(request(page.next_point)).is_err() && !offline.mount.exists(),
            "Offline read created a substitute mount"
        );
        Ok(())
    })();
    // Only this newly-created fixture tree is removed. The verified handle
    // prevents cleanup from following a replaced mount/directory identity.
    root.verify()?;
    std::fs::remove_dir_all(&root.path).context("Owned archive fixture cleanup failed")?;
    result
}
