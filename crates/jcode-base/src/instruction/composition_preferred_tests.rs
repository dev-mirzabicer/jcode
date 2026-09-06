use super::*;

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, SystemPromptComposer) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join(".jcode")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let composer = SystemPromptComposer::from_repository_service(
        InstructionRepositoryService::from_paths(&home, temp.path().join("state")),
    );
    (temp, home, project, composer)
}

#[test]
fn preferred_tools_import_once_and_keep_project_addition_and_source_failures() {
    let (_temp, home, project, composer) = fixture();
    let mut old_seed = shipped_instruction_seed().unwrap();
    old_seed.manifest.seed_version = 24;
    composer
        .repositories
        .initialize_global(&old_seed, &[])
        .unwrap();
    std::fs::write(home.join("preferred-tools.md"), "  GLOBAL<&>\n").unwrap();
    std::fs::write(project.join(".jcode/preferred-tools.md"), "PROJECT<&>\n").unwrap();
    let first = composer.legacy_preferred_tools(Some(&project)).unwrap();
    assert_eq!(
        first.0.as_deref(),
        Some(
            "# Project Preferred Tools (.jcode/preferred-tools.md)\n\nPROJECT<&>\n\n# Global Preferred Tools (~/.jcode/preferred-tools.md)\n\nGLOBAL<&>"
        )
    );
    let global = composer.repositories.global_repository().unwrap();
    assert!(
        composer
            .repositories
            .load_manifest(&global)
            .unwrap()
            .legacy_imports
            .contains_key("global-preferred-tools")
    );
    assert_eq!(
        std::fs::read_to_string(home.join("preferred-tools.md")).unwrap(),
        "  GLOBAL<&>\n"
    );
    std::fs::write(home.join("preferred-tools.md"), [0xff]).unwrap();
    assert_eq!(
        composer.legacy_preferred_tools(Some(&project)).unwrap(),
        first
    );
    std::fs::write(
        global.root.join("tools/preferred-tools.md"),
        "---\nid: preferred-tools\nkind: tool-guidance\n---\nMANAGED",
    )
    .unwrap();
    assert!(
        composer
            .legacy_preferred_tools(Some(&project))
            .unwrap()
            .0
            .unwrap()
            .ends_with("MANAGED")
    );
    let project_seed = InstructionStoreSeed {
        manifest: InstructionStoreManifest::current(),
        files: vec![],
    };
    let configured = composer
        .repositories
        .configure_non_git_project(&project, "preferred-project", None, &project_seed, &[])
        .unwrap();
    std::fs::create_dir_all(configured.repository.root.join("tools")).unwrap();
    let source = configured.repository.root.join("tools/preferred-tools.md");
    std::fs::write(
        &source,
        "---\nid: preferred-tools\nkind: tool-guidance\ntemplate: handlebars\n---\n{{missing}}",
    )
    .unwrap();
    assert!(composer.legacy_preferred_tools(Some(&project)).is_err());
    std::fs::write(
        &source,
        "---\nid: preferred-tools\nkind: tool-guidance\n---\n",
    )
    .unwrap();
    let empty = composer
        .legacy_preferred_tools(Some(&project))
        .unwrap()
        .0
        .unwrap();
    assert!(empty.starts_with("# Project Preferred Tools\n\n"));
    assert!(!empty.contains("PROJECT<&>"));
    assert!(empty.ends_with("MANAGED"));
    std::fs::remove_file(&source).unwrap();
    assert!(
        composer
            .legacy_preferred_tools(Some(&project))
            .unwrap()
            .0
            .unwrap()
            .contains("PROJECT<&>")
    );
    let spec = InstructionLegacyImportSpec {
        import_id: "project-preferred-tools".into(),
        source_kind: LegacyInstructionSourceKind::PreferredTools,
        source_path: project.join(".jcode/preferred-tools.md"),
        target: InstructionLegacyImportTarget {
            relative_path: "tools/preferred-tools.md".into(),
            id: InstructionId::parse("preferred-tools").unwrap(),
            kind: InstructionKind::ToolGuidance,
            scope: InstructionScope::Project,
            template_mode: TemplateMode::Plain,
            metadata: InstructionMetadata::default(),
        },
    };
    composer
        .repositories
        .import_legacy(&configured.repository, &spec, "project-preferred-import")
        .unwrap();
    std::fs::remove_file(&source).unwrap();
    assert!(composer.legacy_preferred_tools(Some(&project)).is_err());
}

#[test]
fn explicit_project_consumer_does_not_fall_back_to_global() {
    let (_temp, _home, project, composer) = fixture();
    composer.ensure_global_store().unwrap();
    let global = composer.repositories.global_repository().unwrap();
    std::fs::create_dir_all(global.root.join("tools")).unwrap();
    std::fs::write(
        global.root.join("tools/preferred-tools.md"),
        "---\nid: preferred-tools\nkind: tool-guidance\n---\nGLOBAL",
    )
    .unwrap();
    let runtime =
        super::super::notification::occurrence_runtime(&composer.repositories, Some(&project))
            .unwrap();
    let registration = preferred_tools_registration(InstructionScope::Project).unwrap();
    assert!(matches!(
        runtime.render_registered(&registration, &()),
        Err(InstructionError::ResourceNotFound { .. })
    ));
    let global_registration = preferred_tools_registration(InstructionScope::Global).unwrap();
    assert_eq!(
        runtime
            .render_registered(&global_registration, &())
            .unwrap()
            .text,
        "GLOBAL"
    );
}
