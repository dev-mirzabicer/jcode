use super::*;
use std::process::Command;

struct Fixture {
    temp: tempfile::TempDir,
    repositories: InstructionRepositoryService,
    root: PathBuf,
    project: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let repositories =
            InstructionRepositoryService::from_paths(&home, temp.path().join("state"));
        let root = home.join("instructions");
        Self {
            temp,
            repositories,
            root,
            project,
        }
    }
    fn seed(&self) {
        let files = [
            ("system/kernel.md", document("kernel", "system", "KERNEL")),
            ("system/common.md", document("common", "system", "COMMON")),
            ("system/mermaid.md", document("mermaid", "system", "CAPABILITY")),
            ("system/available-skills.md", document("available-skills", "system", "CATALOG")),
            ("agents/jcode.md", "---\nid: jcode\nkind: agent\nname: Fixture\ndescription: Synthetic fixture\navailability: both\nincludes: [shared]\n---\nAGENT".into()),
            ("modules/shared.md", document("shared", "module", "SHARED")),
            ("notifications/synthetic.md", document("synthetic", "notification", "NOTICE")),
            ("tools/synthetic.md", document("synthetic", "tool-guidance", "TOOL")),
            ("skills/sample/SKILL.md", "---\nid: sample\nkind: skill\nname: Sample\ndescription: Fixture\n---\nSKILL".into()),
            ("model-roster.toml", "[aliases.fixture]\ndescription='Synthetic'\nmodels=['openai-oauth:gpt-6-astra']\nnotes='HUMAN ONLY'\n".into()),
        ];
        self.repositories
            .initialize_global(
                &InstructionStoreSeed {
                    manifest: InstructionStoreManifest::current(),
                    files: files
                        .into_iter()
                        .map(|(path, content)| InstructionSeedFile {
                            relative_path: path.into(),
                            content: content.into_bytes(),
                        })
                        .collect(),
                },
                &[],
            )
            .unwrap();
    }
    fn context(&self) -> InspectionContext {
        InspectionContext {
            session_id: "fixture-session".into(),
            working_dir: Some(self.project.clone()),
            active_agent: Some("global:jcode".into()),
            stored_system: Some(Arc::from("FROZEN SYSTEM")),
            is_selfdev: false,
            capabilities: PromptCapabilities { mermaid: false },
            roster_catalog: None,
        }
    }
    fn open(&self) -> InstructionInspector {
        InstructionInspector::open(
            self.repositories.clone(),
            self.context(),
            &AtomicBool::new(false),
        )
        .unwrap()
    }
}
fn document(id: &str, kind: &str, body: &str) -> String {
    format!("---\nid: {id}\nkind: {kind}\n---\n{body}")
}
fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn detail(
    inspector: &mut InstructionInspector,
    key: &str,
    view: InstructionInspectionView,
) -> InstructionTextPage {
    let reply = inspector.request(
        InstructionInspectionRequest::Detail {
            snapshot: inspector.snapshot.clone(),
            target: InstructionInspectionTarget::Resource(key.into()),
            view,
            revision: None,
        },
        &AtomicBool::new(false),
    );
    let InstructionInspectionResult::Text(page) = reply.result else {
        panic!("{reply:?}")
    };
    page
}

#[test]
fn opening_absent_store_is_read_only_and_shows_uninitialized_state() {
    let fixture = Fixture::new();
    let inspector = fixture.open();
    assert!(
        inspector
            .stores
            .values()
            .any(|store| store.row.health.starts_with("Uninitialized"))
    );
    assert!(!fixture.root.exists());
    assert!(!fixture.temp.path().join("state").exists());
}

#[test]
fn exact_large_unicode_detail_remains_pinned_across_disk_edits_and_rejects_stale_pages() {
    let fixture = Fixture::new();
    fixture.seed();
    let original = document("large", "module", &"α界🙂\n".repeat(200_000));
    std::fs::write(fixture.root.join("modules/large.md"), &original).unwrap();
    let before = git(&fixture.root, &["status", "--porcelain=v2"]);
    let index_before = std::fs::read(fixture.root.join(".git/index")).unwrap();
    let head_before = git(&fixture.root, &["rev-parse", "HEAD"]);
    let mut inspector = fixture.open();
    let key = inspector
        .resources
        .values()
        .find(|resource| resource.row.id == "large")
        .unwrap()
        .row
        .key
        .clone();
    let mut page = detail(&mut inspector, &key, InstructionInspectionView::Source);
    assert!(page.next.is_some());
    assert_eq!(page.total_bytes, original.len());
    assert_eq!(
        index_before,
        std::fs::read(fixture.root.join(".git/index")).unwrap()
    );
    assert_eq!(before, git(&fixture.root, &["status", "--porcelain=v2"]));
    std::fs::write(
        fixture.root.join("modules/large.md"),
        document("large", "module", "NEW"),
    )
    .unwrap();
    let mut complete = page.text.clone();
    while let Some(offset) = page.next {
        let reply = inspector.request(
            InstructionInspectionRequest::Text {
                snapshot: inspector.snapshot.clone(),
                document: page.document.clone(),
                offset,
            },
            &AtomicBool::new(false),
        );
        let InstructionInspectionResult::Text(next) = reply.result else {
            panic!("{reply:?}")
        };
        assert_eq!(next.offset, complete.len());
        complete.push_str(&next.text);
        page = next;
    }
    assert_eq!(complete, original);
    let old_document = page.document;
    detail(&mut inspector, &key, InstructionInspectionView::Source);
    let stale = inspector.request(
        InstructionInspectionRequest::Text {
            snapshot: inspector.snapshot.clone(),
            document: old_document,
            offset: 0,
        },
        &AtomicBool::new(false),
    );
    assert!(matches!(
        stale.result,
        InstructionInspectionResult::Failed(_)
    ));
    assert_eq!(head_before, git(&fixture.root, &["rev-parse", "HEAD"]));
}

#[test]
fn catalog_keeps_project_shadow_invalid_resources_and_every_source_class_visible() {
    let fixture = Fixture::new();
    fixture.seed();
    let project_store = fixture.project.join("store");
    git(
        fixture.temp.path(),
        &[
            "clone",
            fixture.root.to_str().unwrap(),
            project_store.to_str().unwrap(),
        ],
    );
    std::fs::create_dir_all(fixture.project.join(".jcode/skills/external")).unwrap();
    std::fs::write(
        fixture.project.join(".jcode/instructions.toml"),
        toml::to_string(&InstructionProjectConfig::new(
            InstructionProjectRepositoryMode::ExternalLocal {
                path: project_store.clone(),
                branch: None,
            },
        ))
        .unwrap(),
    )
    .unwrap();
    std::fs::remove_file(project_store.join("model-roster.toml")).unwrap();
    std::fs::write(
        project_store.join("modules/shared.md"),
        "---\nid: shared\nkind: module\nunknown: invalid\n---\nBROKEN",
    )
    .unwrap();
    std::fs::create_dir_all(project_store.join("addenda")).unwrap();
    std::fs::write(
        project_store.join("addenda/extra.md"),
        "---\nid: extra\nkind: agent-addendum\ntarget: global:jcode\n---\nADDENDUM",
    )
    .unwrap();
    std::fs::write(fixture.project.join("AGENTS.md"), "ECOSYSTEM").unwrap();
    std::fs::write(fixture.project.join(".jcode/prompt-overlay.md"), "LEGACY").unwrap();
    std::fs::write(
        fixture.project.join(".jcode/skills/external/SKILL.md"),
        "---\nname: External\ndescription: Fixture\n---\nEXTERNAL",
    )
    .unwrap();
    let inspector = fixture.open();
    let shared = inspector
        .resources
        .values()
        .filter(|resource| resource.row.id == "shared")
        .collect::<Vec<_>>();
    assert!(shared.iter().any(|resource| resource.row.scope == "global"
        && !resource.row.effective
        && resource.row.valid));
    assert!(shared.iter().any(|resource| resource.row.scope == "project"
        && resource.row.effective
        && !resource.row.valid));
    for kind in [
        "system",
        "agent",
        "agent-addendum",
        "module",
        "notification",
        "tool-guidance",
        "skill",
        "model-roster",
        "AGENTS.md",
        "legacy-prompt",
        "store-settings",
    ] {
        assert!(
            inspector
                .resources
                .values()
                .any(|resource| resource.row.kind == kind),
            "{kind}"
        );
    }
    for origin in [
        InstructionOrigin::Managed,
        InstructionOrigin::Legacy,
        InstructionOrigin::External,
    ] {
        assert!(
            inspector
                .resources
                .values()
                .any(|resource| resource.row.origin == origin)
        );
    }
    let invalid = inspector.rows(
        &InstructionFilter {
            valid: Some(false),
            ..Default::default()
        },
        0,
    );
    assert!(invalid.rows.iter().all(|row| !row.valid));
    let shadowed = inspector.rows(
        &InstructionFilter {
            effective: Some(false),
            ..Default::default()
        },
        0,
    );
    assert!(shadowed.rows.iter().all(|row| !row.effective));
}

#[test]
fn previews_reuse_composer_and_graph_without_seed_upgrade_or_session_change() {
    let fixture = Fixture::new();
    fixture.seed();
    // An older seed must not be upgraded merely to inspect the current files.
    let mut manifest = fixture
        .repositories
        .load_manifest(&fixture.repositories.global_repository().unwrap())
        .unwrap();
    manifest.seed_version = 1;
    std::fs::write(
        fixture.root.join("instruction-store.toml"),
        toml::to_string(&manifest).unwrap(),
    )
    .unwrap();
    let head = git(&fixture.root, &["rev-parse", "HEAD"]);
    let index = std::fs::read(fixture.root.join(".git/index")).unwrap();
    let mut inspector = fixture.open();
    let key = inspector
        .resources
        .values()
        .find(|resource| resource.row.kind == "agent" && resource.row.id == "jcode")
        .unwrap()
        .row
        .key
        .clone();
    let expected = SystemPromptComposer::from_repository_service(fixture.repositories.clone())
        .preview(SystemPromptActivationRequest {
            working_dir: Some(&fixture.project),
            selection: AgentSelection::parse(Some("global:jcode")).unwrap(),
            is_selfdev: false,
            capabilities: PromptCapabilities { mermaid: false },
            available_skills: &inspector.skills,
        })
        .unwrap()
        .state
        .text;
    assert_eq!(
        detail(&mut inspector, &key, InstructionInspectionView::System).text,
        expected
    );
    assert!(
        detail(
            &mut inspector,
            &key,
            InstructionInspectionView::Dependencies
        )
        .text
        .contains("global:module:shared")
    );
    assert!(
        detail(&mut inspector, &key, InstructionInspectionView::Metadata)
            .text
            .contains("availability: both")
    );
    assert_eq!(head, git(&fixture.root, &["rev-parse", "HEAD"]));
    assert_eq!(
        index,
        std::fs::read(fixture.root.join(".git/index")).unwrap()
    );
    assert_eq!(
        fixture
            .repositories
            .load_manifest(&fixture.repositories.global_repository().unwrap())
            .unwrap()
            .seed_version,
        1
    );
    assert_eq!(
        inspector.context.stored_system.as_deref(),
        Some("FROZEN SYSTEM")
    );
}

#[test]
fn git_history_and_revision_comparison_are_pinned_and_working_diff_is_complete() {
    let fixture = Fixture::new();
    fixture.seed();
    let first = git(&fixture.root, &["rev-parse", "HEAD"]);
    std::fs::write(
        fixture.root.join("modules/shared.md"),
        document("shared", "module", "SECOND"),
    )
    .unwrap();
    git(&fixture.root, &["add", "modules/shared.md"]);
    git(&fixture.root, &["commit", "-m", "second"]);
    let mut inspector = fixture.open();
    let key = inspector
        .resources
        .values()
        .find(|resource| resource.row.kind == "module" && resource.row.id == "shared")
        .unwrap()
        .row
        .key
        .clone();
    let target = InstructionInspectionTarget::Resource(key.clone());
    let history = inspector.history(&target, 0).unwrap();
    assert_eq!(history.commits.len(), 2);
    let second = history.commits[0].commit.clone();
    assert_eq!(
        inspector
            .revision(
                &target,
                &InstructionRevisionSelection {
                    from: first.clone(),
                    to: None
                }
            )
            .unwrap()
            .1,
        document("shared", "module", "SHARED")
    );
    assert!(
        inspector
            .revision(
                &target,
                &InstructionRevisionSelection {
                    from: first,
                    to: Some(second)
                }
            )
            .unwrap()
            .1
            .contains("+SECOND")
    );
    std::fs::write(
        fixture.root.join("modules/shared.md"),
        document("shared", "module", "THIRD"),
    )
    .unwrap();
    assert!(
        detail(&mut inspector, &key, InstructionInspectionView::WorkingDiff)
            .text
            .contains("+THIRD")
    );
    git(&fixture.root, &["add", "modules/shared.md"]);
    git(&fixture.root, &["commit", "-m", "third"]);
    assert_eq!(
        inspector.history(&target, 0).unwrap().commits,
        history.commits
    );
}

#[tokio::test]
async fn worker_cancel_and_cross_session_requests_cannot_publish_old_context() {
    let fixture = Fixture::new();
    let mut worker = InspectionWorker::default();
    let context = fixture.context();
    let open = worker.submit(
        fixture.repositories.clone(),
        context.session_id.clone(),
        move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            Ok(context)
        },
        InstructionInspectionRequest::Open {
            filter: Default::default(),
        },
    );
    let closed = worker.submit(
        fixture.repositories.clone(),
        "fixture-session".into(),
        || unreachable!(),
        InstructionInspectionRequest::Close,
    );
    assert!(matches!(
        closed.await.unwrap().result,
        InstructionInspectionResult::Closed
    ));
    assert!(open.await.is_err());
    assert!(!fixture.root.exists());
}

#[test]
fn invalid_external_skills_and_nonregular_managed_resources_remain_visible() {
    let fixture = Fixture::new();
    fixture.seed();
    std::fs::create_dir_all(fixture.root.join("modules/not-a-file.md")).unwrap();
    std::fs::create_dir_all(fixture.project.join(".jcode/skills/invalid")).unwrap();
    std::fs::write(
        fixture.project.join(".jcode/skills/invalid/SKILL.md"),
        [0xff],
    )
    .unwrap();
    let inspector = fixture.open();
    assert!(
        inspector
            .resources
            .values()
            .any(|resource| resource.path.ends_with("not-a-file.md") && !resource.row.valid)
    );
    assert!(
        inspector
            .resources
            .values()
            .any(|resource| resource.path.ends_with("invalid/SKILL.md") && !resource.row.valid)
    );
    assert!(
        inspector
            .resources
            .values()
            .any(|resource| resource.row.id == "shared" && resource.row.valid)
    );
}

#[cfg(unix)]
#[test]
fn unsafe_managed_symlinks_do_not_render_or_expose_global_fallback() {
    let fixture = Fixture::new();
    fixture.seed();
    let outside = fixture.temp.path().join("outside.md");
    std::fs::write(&outside, document("linked", "module", "OUTSIDE")).unwrap();
    std::os::unix::fs::symlink(&outside, fixture.root.join("modules/linked.md")).unwrap();
    let inspector = fixture.open();
    assert!(
        inspector
            .resources
            .values()
            .any(|resource| resource.row.id == "linked" && !resource.row.valid)
    );
    assert!(
        InstructionRuntime::discover(inspector.sources.clone())
            .render(
                &InstructionSelector::global(InstructionKind::Module, "linked").unwrap(),
                &()
            )
            .is_err()
    );
    let project_root = fixture.temp.path().join("project-store");
    std::fs::create_dir_all(&project_root).unwrap();
    std::os::unix::fs::symlink(fixture.root.join("modules"), project_root.join("modules")).unwrap();
    let runtime = InstructionRuntime::discover(
        InstructionSources::new(&fixture.root).with_project_root(project_root),
    );
    assert!(
        runtime
            .render(
                &InstructionSelector::unqualified(InstructionKind::Module, "shared").unwrap(),
                &()
            )
            .is_err()
    );
    assert!(
        runtime
            .render(
                &InstructionSelector::global(InstructionKind::Module, "shared").unwrap(),
                &()
            )
            .is_ok()
    );
}

#[test]
fn registered_empty_contract_available_values_and_isolated_preview_are_truthful() {
    let fixture = Fixture::new();
    fixture.seed();
    std::fs::write(
        fixture.root.join("notifications/todo-auto-poke.md"),
        document("todo-auto-poke", "notification", ""),
    )
    .unwrap();
    std::fs::write(
        fixture.root.join("system/available-skills.md"),
        "---\nid: available-skills\nkind: system\ntemplate: handlebars\n---\n{{skills}}",
    )
    .unwrap();
    std::fs::write(fixture.root.join("agents/isolated.md"), "---\nid: isolated\nkind: agent\nname: Isolated\ndescription: Fixture\navailability: isolated\n---\nISOLATED-BODY").unwrap();
    let mut inspector = fixture.open();
    let empty = inspector
        .resources
        .values()
        .find(|resource| resource.row.id == "todo-auto-poke")
        .unwrap();
    assert!(!empty.row.valid);
    let skill_key = inspector
        .resources
        .values()
        .find(|resource| resource.row.id == "available-skills")
        .unwrap()
        .row
        .key
        .clone();
    assert!(
        detail(
            &mut inspector,
            &skill_key,
            InstructionInspectionView::Rendered
        )
        .text
        .contains("Sample")
    );
    let isolated = inspector
        .resources
        .values()
        .find(|resource| resource.row.id == "isolated")
        .unwrap()
        .row
        .key
        .clone();
    assert_eq!(
        detail(
            &mut inspector,
            &isolated,
            InstructionInspectionView::Rendered
        )
        .text,
        "ISOLATED-BODY"
    );
    let before = git(&fixture.root, &["rev-parse", "HEAD"]);
    let result = inspector.request(
        InstructionInspectionRequest::Detail {
            snapshot: inspector.snapshot.clone(),
            target: InstructionInspectionTarget::Resource(isolated),
            view: InstructionInspectionView::System,
            revision: None,
        },
        &AtomicBool::new(false),
    );
    assert!(matches!(
        result.result,
        InstructionInspectionResult::Failed(_)
    ));
    assert_eq!(before, git(&fixture.root, &["rev-parse", "HEAD"]));
}

#[test]
fn repository_history_paging_has_no_hidden_tail_and_detached_inspection_is_readonly() {
    let fixture = Fixture::new();
    fixture.seed();
    for index in 0..66 {
        git(
            &fixture.root,
            &["commit", "--allow-empty", "-m", &format!("fixture-{index}")],
        );
    }
    git(&fixture.root, &["checkout", "--detach", "HEAD"]);
    let before = std::fs::read(fixture.root.join(".git/index")).unwrap();
    let inspector = fixture.open();
    assert!(inspector.stores["global"].row.detached);
    let target = InstructionInspectionTarget::Repository("global".into());
    let first = inspector.history(&target, 0).unwrap();
    assert_eq!(first.commits.len(), ROW_PAGE_SIZE);
    let second = inspector.history(&target, first.next.unwrap()).unwrap();
    assert_eq!(second.commits.len(), 3);
    assert!(second.next.is_none());
    assert_ne!(
        first.commits.last().unwrap().commit,
        second.commits.first().unwrap().commit
    );
    assert_eq!(
        before,
        std::fs::read(fixture.root.join(".git/index")).unwrap()
    );
}
