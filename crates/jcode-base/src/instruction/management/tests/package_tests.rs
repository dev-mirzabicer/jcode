use super::*;

async fn begin_copy(fixture: &mut Fixture) -> InstructionEditDraft {
    let snapshot = fixture.open("package-copy").await;
    let row = snapshot
        .resources
        .rows
        .iter()
        .find(|row| row.origin == InstructionOrigin::External && row.name == "package-copy")
        .unwrap();
    draft(
        fixture
            .request(InstructionManagementRequest::Begin {
                snapshot: snapshot.snapshot,
                target: InstructionInspectionTarget::Resource(row.key.clone()),
                action: InstructionEditAction::CopySkill {
                    scope: InstructionEditScope::Global,
                    destination_id: None,
                },
            })
            .await,
    )
}
pub(super) async fn save(fixture: &Fixture, draft: &InstructionEditDraft) {
    let reviewed = fixture
        .request(InstructionManagementRequest::Review {
            draft: draft.id.clone(),
            generation: draft.generation,
        })
        .await;
    let InstructionManagementResult::Reviewed(review) = reviewed else {
        panic!("{reviewed:?}")
    };
    assert!(review.errors.is_empty(), "{:?}", review.errors);
    let saved = fixture
        .request(InstructionManagementRequest::Save {
            draft: draft.id.clone(),
            generation: draft.generation,
        })
        .await;
    assert!(
        matches!(saved, InstructionManagementResult::Saved { .. }),
        "{saved:?}"
    );
    fixture.request(InstructionManagementRequest::Close).await;
}

#[test]
fn manager_copy_rename_delete_preserve_complete_binary_package_and_original_source() {
    let _environment = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut fixture = Fixture::new();
        let source = fixture.context.working_dir.as_ref().unwrap().join(".jcode/skills/package-copy");
        std::fs::create_dir_all(source.join("references")).unwrap();
        let original = "---\nname: package-copy\ndescription: Synthetic package\n---\nSYNTHETIC {{literal}}";
        let bytes = [0xff,0,0xfe,0x80,1,2,3];
        std::fs::write(source.join("SKILL.md"), original).unwrap();
        std::fs::write(source.join("references/binary.dat"), bytes).unwrap();
        std::fs::write(source.join("references/plain.md"), "PLAIN REFERENCE").unwrap();
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(source.join("references/run.sh"), "#!/bin/sh\nexit 99\n").unwrap();
            std::fs::set_permissions(source.join("references/run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let copy = begin_copy(&mut fixture).await;
        assert!(copy.files.iter().any(|file| matches!(file.metadata, InstructionEditMetadata::Binary { bytes: 7, .. })));
        assert!(!fixture.repository.root.join("skills/package-copy/SKILL.md").exists());
        // A later source edit cannot replace the captured Copy under review.
        std::fs::write(source.join("references/binary.dat"), b"NEW SOURCE").unwrap();
        save(&fixture, &copy).await;
        assert_eq!(std::fs::read(fixture.repository.root.join("skills/package-copy/references/binary.dat")).unwrap(), bytes);
        assert_eq!(std::fs::read_to_string(fixture.repository.root.join("skills/package-copy/.jcode-source/original-SKILL.md")).unwrap(), original);
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            let script = fixture.repository.root.join("skills/package-copy/references/run.sh");
            assert_eq!(std::fs::metadata(&script).unwrap().permissions().mode() & 0o777, 0o700);
        }
        let rename = fixture.begin("package-copy", InstructionEditAction::Rename { id: "renamed-package".into() }).await;
        save(&fixture, &rename).await;
        assert!(!fixture.repository.root.join("skills/package-copy/SKILL.md").exists());
        assert_eq!(std::fs::read(fixture.repository.root.join("skills/renamed-package/references/binary.dat")).unwrap(), bytes);
        let delete = fixture.begin("renamed-package", InstructionEditAction::Delete).await;
        save(&fixture, &delete).await;
        assert!(!fixture.repository.root.join("skills/renamed-package/references/binary.dat").exists());
        assert_eq!(std::fs::read_to_string(source.join("SKILL.md")).unwrap(), original);
        assert_eq!(std::fs::read(source.join("references/binary.dat")).unwrap(), b"NEW SOURCE");
    });
}

#[test]
fn invalid_utf8_working_resource_can_be_restored_without_losing_original_bytes() {
    let _environment = crate::storage::lock_test_env();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut fixture = Fixture::new();
            let head = fixture
                .service
                .inspect(&fixture.repository)
                .unwrap()
                .head
                .unwrap();
            let path = fixture.repository.root.join("modules/shared.md");
            let committed = std::fs::read(&path).unwrap();
            std::fs::write(&path, [0xff, 0x80, 0]).unwrap();
            let restored = fixture
                .begin("shared", InstructionEditAction::Restore { revision: head })
                .await;
            assert_eq!(std::fs::read(&path).unwrap(), [0xff, 0x80, 0]);
            save(&fixture, &restored).await;
            assert_eq!(std::fs::read(&path).unwrap(), committed);
        });
}
