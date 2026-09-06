//! Managed routing guidance. No source watchers or orchestration policy.
use super::*;
use std::path::{Path, PathBuf};

pub fn swarm_routing_source(
    repositories: &InstructionRepositoryService,
    working_dir: Option<&Path>,
) -> Result<(String, PathBuf), SystemPromptActivationError> {
    let runtime = notification::occurrence_runtime(repositories, working_dir)?;
    let project = working_dir
        .map(|dir| repositories.resolve_project_repository(dir))
        .transpose()?
        .flatten();
    let imported = project
        .as_ref()
        .map(|repo| repositories.load_manifest(repo))
        .transpose()?
        .is_some_and(|m| {
            m.legacy_imports
                .values()
                .any(|r| r.source_kind == LegacyInstructionSourceKind::SwarmPrompt)
        });
    let selector = InstructionSelector::project(InstructionKind::ToolGuidance, "swarm-routing")?;
    match runtime.resolve(&selector) {
        Ok(document) => {
            let text = runtime.render(&selector, &())?.text;
            return Ok((
                if imported { text.trim().into() } else { text },
                document.path.clone(),
            ));
        }
        Err(InstructionError::ResourceNotFound { .. }) if !imported => {}
        Err(error) => return Err(error.into()),
    }
    // Preserve unimported project compatibility. A managed empty definition
    // above shadows global; a legacy blank file still means the old fallback.
    if let Some(dir) = working_dir {
        let root = repositories.resolve_project_root(dir)?;
        let path = root.join(".jcode/swarm-prompt.md");
        match std::fs::read_to_string(&path) {
            Ok(text) if !text.trim().is_empty() => return Ok((text.trim().into(), path)),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(SystemPromptActivationError::Compatibility(format!(
                    "could not read {}: {error}",
                    path.display()
                )));
            }
        }
    }
    let registration = workflow::Workflow::SwarmRouting.registration()?;
    let text = runtime.render_registered(&registration, &())?.text;
    let selector = InstructionSelector::global(InstructionKind::ToolGuidance, "swarm-routing")?;
    let path = runtime.resolve(&selector)?.path.clone();
    let global = repositories.load_manifest(&repositories.global_repository()?)?;
    let imported = global
        .legacy_imports
        .values()
        .any(|r| r.source_kind == LegacyInstructionSourceKind::SwarmPrompt);
    Ok((if imported { text.trim().into() } else { text }, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routing_source_respects_managed_empty_shadow_and_legacy_cutover() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(project.join(".jcode")).unwrap();
        let repositories =
            InstructionRepositoryService::from_paths(&home, temp.path().join("state"));
        let mut seed = shipped_instruction_seed().unwrap();
        seed.manifest.seed_version = 25;
        seed.files
            .retain(|f| f.relative_path != Path::new("tools/swarm-routing.md"));
        repositories.initialize_global(&seed, &[]).unwrap();
        std::fs::write(home.join("swarm-prompt.md"), " GLOBAL ").unwrap();
        assert_eq!(
            swarm_routing_source(&repositories, Some(&project))
                .unwrap()
                .0,
            "GLOBAL"
        );
        std::fs::write(home.join("swarm-prompt.md"), [0xff]).unwrap();
        assert_eq!(
            swarm_routing_source(&repositories, Some(&project))
                .unwrap()
                .0,
            "GLOBAL"
        );
        std::fs::write(project.join(".jcode/swarm-prompt.md"), "PROJECT").unwrap();
        assert_eq!(
            swarm_routing_source(&repositories, Some(&project))
                .unwrap()
                .0,
            "PROJECT"
        );
        let seed = InstructionStoreSeed {
            manifest: InstructionStoreManifest::current(),
            files: vec![InstructionSeedFile {
                relative_path: "tools/swarm-routing.md".into(),
                content: b"---\nid: swarm-routing\nkind: tool-guidance\n---\n".to_vec(),
            }],
        };
        let configured = repositories
            .configure_non_git_project(&project, "routing-project", None, &seed, &[])
            .unwrap();
        let path = configured.repository.root.join("tools/swarm-routing.md");
        assert_eq!(
            swarm_routing_source(&repositories, Some(&project)).unwrap(),
            (String::new(), path.clone())
        );
        std::fs::write(
            &path,
            "---\nid: swarm-routing\nkind: tool-guidance\ntemplate: handlebars\n---\n{{missing}}",
        )
        .unwrap();
        assert!(swarm_routing_source(&repositories, Some(&project)).is_err());
    }
}
