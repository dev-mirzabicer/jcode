use super::*;
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    global: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let global = temp.path().join("global");
        let project = temp.path().join("project");
        fs::create_dir_all(&global).expect("global root");
        fs::create_dir_all(&project).expect("project root");
        Self {
            _temp: temp,
            global,
            project,
        }
    }

    fn runtime(&self) -> InstructionRuntime {
        InstructionRuntime::discover(
            InstructionSources::new(&self.global).with_project_root(&self.project),
        )
    }

    fn write(
        &self,
        scope: InstructionScope,
        kind: InstructionKind,
        name: &str,
        source: &str,
    ) -> PathBuf {
        let root = match scope {
            InstructionScope::Global => &self.global,
            InstructionScope::Project => &self.project,
        };
        let path = if kind == InstructionKind::Skill {
            root.join(kind.directory()).join(name).join("SKILL.md")
        } else {
            root.join(kind.directory()).join(format!("{name}.md"))
        };
        fs::create_dir_all(path.parent().expect("parent")).expect("resource directory");
        fs::write(&path, source).expect("write resource");
        path
    }
}

fn source(id: &str, kind: InstructionKind, extra: &str, body: &str) -> String {
    format!(
        "---\nid: {id}\nkind: {kind}\n{extra}---\n\n{body}",
        kind = kind.to_string()
    )
}

fn selector(kind: InstructionKind, id: &str) -> InstructionSelector {
    InstructionSelector::unqualified(kind, id).expect("selector")
}

#[test]
fn plain_content_is_literal_and_handlebars_uses_typed_values_and_scoped_partials() {
    let fixture = Fixture::new();
    fixture.write(
        InstructionScope::Global,
        InstructionKind::Module,
        "shared",
        &source(
            "shared",
            InstructionKind::Module,
            "template: handlebars\n",
            "global {{person.name}}",
        ),
    );
    fixture.write(
        InstructionScope::Project,
        InstructionKind::Module,
        "shared",
        &source(
            "shared",
            InstructionKind::Module,
            "",
            "project {{person.name}}",
        ),
    );
    fixture.write(
        InstructionScope::Global,
        InstructionKind::System,
        "root",
        &source(
            "root",
            InstructionKind::System,
            "template: handlebars\n",
            "A={{> shared}}; B={{> global:shared}}; C={{person.name}}",
        ),
    );

    let rendered = fixture
        .runtime()
        .render(
            &selector(InstructionKind::System, "root"),
            &json!({"person": {"name": "Ada & Co"}}),
        )
        .expect("render");
    assert_eq!(
        rendered.text,
        "A=project {{person.name}}; B=global Ada & Co; C=Ada & Co"
    );
}

#[test]
fn invalid_project_definition_shadows_global_without_blocking_unrelated_resources() {
    let fixture = Fixture::new();
    fixture.write(
        InstructionScope::Global,
        InstructionKind::Module,
        "shared",
        &source("shared", InstructionKind::Module, "", "global"),
    );
    fixture.write(
        InstructionScope::Project,
        InstructionKind::Module,
        "shared",
        "---\nid: shared\nkind: module\ntemplate: nope\n---\n\nbroken",
    );
    fixture.write(
        InstructionScope::Global,
        InstructionKind::Module,
        "healthy",
        &source("healthy", InstructionKind::Module, "", "healthy"),
    );

    let runtime = fixture.runtime();
    let error = runtime
        .render(&selector(InstructionKind::Module, "shared"), &json!({}))
        .expect_err("project invalid resource must shadow global");
    assert!(matches!(error, InstructionError::InvalidResource { .. }));
    let global = runtime
        .render(
            &InstructionSelector::global(InstructionKind::Module, "shared").unwrap(),
            &json!({}),
        )
        .expect("explicit global");
    assert_eq!(global.text, "global");
    assert_eq!(
        runtime
            .render(&selector(InstructionKind::Module, "healthy"), &json!({}))
            .unwrap()
            .text,
        "healthy"
    );
}

#[test]
fn empty_missing_registered_and_deleted_user_resources_have_distinct_outcomes() {
    let fixture = Fixture::new();
    fixture.write(
        InstructionScope::Global,
        InstructionKind::Notification,
        "empty",
        &source("empty", InstructionKind::Notification, "", ""),
    );
    let runtime = fixture.runtime();
    assert_eq!(
        runtime
            .render(
                &selector(InstructionKind::Notification, "empty"),
                &json!({})
            )
            .unwrap()
            .text,
        ""
    );

    let mut empty_registration = ConsumerRegistration::new(
        "synthetic-empty",
        "empty",
        InstructionKind::Notification,
        "notifications/empty.md",
        "synthetic owner",
        "synthetic test",
    )
    .unwrap();
    empty_registration.empty_is_meaningful = false;
    assert!(matches!(
        runtime.render_registered(&empty_registration, &json!({})),
        Err(InstructionError::EmptyResourceNotAllowed { .. })
    ));

    let missing_registration = ConsumerRegistration::new(
        "required-singleton",
        "missing",
        InstructionKind::Notification,
        "notifications/missing.md",
        "synthetic owner",
        "synthetic test",
    )
    .unwrap();
    assert!(matches!(
        runtime.render_registered(&missing_registration, &json!({})),
        Err(InstructionError::RegisteredResourceMissing { .. })
    ));
    assert!(matches!(
        runtime.render(
            &selector(InstructionKind::Notification, "missing"),
            &json!({})
        ),
        Err(InstructionError::ResourceNotFound { .. })
    ));
}

#[test]
fn deep_finite_graph_and_large_source_render_without_product_caps() {
    let fixture = Fixture::new();
    const DEPTH: usize = 600;
    for index in 0..DEPTH {
        let id = format!("m{index:04}");
        let body = if index + 1 == DEPTH {
            "END".to_string()
        } else {
            format!("{index}>{{{{> m{:04}}}}}", index + 1)
        };
        fixture.write(
            InstructionScope::Global,
            InstructionKind::Module,
            &id,
            &source(
                &id,
                InstructionKind::Module,
                "template: handlebars\n",
                &body,
            ),
        );
    }
    let large = "x".repeat(2_000_000);
    fixture.write(
        InstructionScope::Global,
        InstructionKind::System,
        "large",
        &source("large", InstructionKind::System, "", &large),
    );

    let runtime = fixture.runtime();
    let deep = runtime
        .render(&selector(InstructionKind::Module, "m0000"), &json!({}))
        .expect("deep finite render");
    assert!(deep.text.starts_with("0>1>2>"));
    assert!(deep.text.ends_with("END"));
    assert_eq!(deep.graph.dependencies.len(), DEPTH);
    let large_render = runtime
        .render(&selector(InstructionKind::System, "large"), &json!({}))
        .expect("large render");
    assert_eq!(large_render.text.len(), large.len());
    assert_eq!(large_render.text, large);
}

#[test]
fn cycles_unknown_values_missing_dependencies_and_helpers_fail_without_partial_output() {
    let fixture = Fixture::new();
    fixture.write(
        InstructionScope::Global,
        InstructionKind::Module,
        "a",
        &source(
            "a",
            InstructionKind::Module,
            "template: handlebars\n",
            "a{{> b}}",
        ),
    );
    fixture.write(
        InstructionScope::Global,
        InstructionKind::Module,
        "b",
        &source(
            "b",
            InstructionKind::Module,
            "template: handlebars\n",
            "b{{> a}}",
        ),
    );
    fixture.write(
        InstructionScope::Global,
        InstructionKind::System,
        "unknown",
        &source(
            "unknown",
            InstructionKind::System,
            "template: handlebars\n",
            "prefix {{missing.value}} suffix",
        ),
    );
    fixture.write(
        InstructionScope::Global,
        InstructionKind::System,
        "helper",
        &source(
            "helper",
            InstructionKind::System,
            "template: handlebars\n",
            "{{#if value}}forbidden{{/if}}",
        ),
    );
    fixture.write(
        InstructionScope::Global,
        InstructionKind::System,
        "missing-partial",
        &source(
            "missing-partial",
            InstructionKind::System,
            "template: handlebars\n",
            "{{> absent}}",
        ),
    );
    let runtime = fixture.runtime();
    assert!(matches!(
        runtime.render(&selector(InstructionKind::Module, "a"), &json!({})),
        Err(InstructionError::DependencyCycle { .. })
    ));
    assert!(matches!(
        runtime.render(&selector(InstructionKind::System, "unknown"), &json!({})),
        Err(InstructionError::Render { .. })
    ));
    assert!(matches!(
        runtime.render(
            &selector(InstructionKind::System, "helper"),
            &json!({"value": true})
        ),
        Err(InstructionError::RestrictedTemplate { .. })
    ));
    assert!(matches!(
        runtime.render(
            &selector(InstructionKind::System, "missing-partial"),
            &json!({})
        ),
        Err(InstructionError::ResourceNotFound { .. })
    ));
}

#[test]
fn duplicate_ids_are_ambiguous_and_catalog_state_remains_inspectable() {
    let fixture = Fixture::new();
    for name in ["first", "second"] {
        fixture.write(
            InstructionScope::Global,
            InstructionKind::Module,
            name,
            &source("duplicate", InstructionKind::Module, "", name),
        );
    }
    let runtime = fixture.runtime();
    assert!(matches!(
        runtime.render(&selector(InstructionKind::Module, "duplicate"), &json!({})),
        Err(InstructionError::AmbiguousResource { .. })
    ));
    let summary = runtime
        .resources()
        .into_iter()
        .find(|summary| summary.resource.id.as_str() == "duplicate")
        .expect("catalog summary");
    assert_eq!(summary.state, ResourceValidationState::Ambiguous);
    assert_eq!(summary.paths.len(), 2);
}

#[test]
fn dedicated_agents_sources_are_global_then_project_and_do_not_join_the_catalog() {
    let fixture = Fixture::new();
    let global_agents = fixture._temp.path().join("global-AGENTS.md");
    let project_agents = fixture._temp.path().join("project-AGENTS.md");
    fs::write(&global_agents, "global body\n").unwrap();
    fs::write(&project_agents, "project body\n").unwrap();
    let runtime = InstructionRuntime::discover(
        InstructionSources::new(&fixture.global)
            .with_project_root(&fixture.project)
            .with_global_agents_md(&global_agents)
            .with_project_agents_md(&project_agents),
    );
    assert_eq!(runtime.external_agents().len(), 2);
    assert_eq!(
        runtime.render_external_agents(),
        "# Global Instructions (~/AGENTS.md)\n\nglobal body\n\n# Project Instructions (AGENTS.md)\n\nproject body"
    );
    assert!(runtime.resources().is_empty());
}

#[derive(Serialize)]
struct NoticeValues<'a> {
    subject: &'a str,
}

#[test]
fn registered_consumers_fix_the_value_type_and_leave_delivery_with_the_owner() {
    let fixture = Fixture::new();
    fixture.write(
        InstructionScope::Global,
        InstructionKind::Notification,
        "notice",
        &source(
            "notice",
            InstructionKind::Notification,
            "template: handlebars\n",
            "Notice for {{subject}}",
        ),
    );
    let registration = ConsumerRegistration::new(
        "synthetic.notice",
        "notice",
        InstructionKind::Notification,
        "notifications/notice.md",
        "synthetic subsystem",
        "The subsystem retains role, framing, timing, and persistence.",
    )
    .unwrap();
    let consumer = InstructionConsumer::<NoticeValues<'_>>::new(registration);
    let rendered = consumer
        .render(&fixture.runtime(), &NoticeValues { subject: "build" })
        .expect("registered render");
    assert_eq!(rendered.text, "Notice for build");
    assert_eq!(
        consumer.registration().delivery_owner,
        "synthetic subsystem"
    );
}

#[test]
fn every_resource_kind_parses_and_semantically_round_trips() {
    let fixture = Fixture::new();
    let cases = [
        (
            InstructionKind::System,
            "system-one",
            "name: System one\ndescription: synthetic\n",
        ),
        (
            InstructionKind::Agent,
            "agent-one",
            "name: Agent one\ndescription: synthetic agent\navailability: both\nincludes: [global:shared]\n",
        ),
        (
            InstructionKind::AgentAddendum,
            "addendum-one",
            "name: Addendum one\ntarget: global:agent-one\n",
        ),
        (InstructionKind::Module, "shared", "name: Shared module\n"),
        (
            InstructionKind::Notification,
            "notice-one",
            "description: synthetic notice\n",
        ),
        (
            InstructionKind::ToolGuidance,
            "tool-one",
            "description: synthetic tool guidance\n",
        ),
        (
            InstructionKind::Skill,
            "skill-one",
            "name: skill-one\ndescription: synthetic skill\nallowed-tools: read, bash\n",
        ),
    ];
    for (kind, id, extra) in cases {
        fixture.write(
            InstructionScope::Global,
            kind,
            id,
            &source(id, kind, extra, "synthetic {{literal}} body"),
        );
    }
    let runtime = fixture.runtime();
    for (kind, id, _) in cases {
        let document = runtime
            .resolve(&selector(kind, id))
            .expect("parsed document");
        let markdown = document.to_markdown().expect("serialize document");
        let roundtrip_root = fixture._temp.path().join(format!("roundtrip-{id}"));
        let path = if kind == InstructionKind::Skill {
            roundtrip_root
                .join(kind.directory())
                .join(id)
                .join("SKILL.md")
        } else {
            roundtrip_root
                .join(kind.directory())
                .join(format!("{id}.md"))
        };
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, markdown).unwrap();
        let roundtrip = InstructionRuntime::discover(InstructionSources::new(&roundtrip_root));
        let reparsed = roundtrip
            .resolve(&selector(kind, id))
            .expect("reparsed document");
        assert_eq!(reparsed.id, document.id);
        assert_eq!(reparsed.kind, document.kind);
        assert_eq!(reparsed.template_mode, document.template_mode);
        assert_eq!(reparsed.metadata, document.metadata);
        assert_eq!(reparsed.body, document.body);
    }
}

#[test]
fn agent_metadata_and_availability_are_enforced_without_session_activation() {
    let fixture = Fixture::new();
    fixture.write(
        InstructionScope::Global,
        InstructionKind::Agent,
        "worker",
        &source(
            "worker",
            InstructionKind::Agent,
            "name: Worker\ndescription: synthetic worker\navailability: isolated\n",
            "worker body",
        ),
    );
    let runtime = fixture.runtime();
    let agent = selector(InstructionKind::Agent, "worker");
    assert_eq!(
        runtime
            .render_agent(&agent, AgentAvailability::Isolated, &json!({}))
            .unwrap()
            .text,
        "worker body"
    );
    assert!(matches!(
        runtime.render_agent(&agent, AgentAvailability::Primary, &json!({})),
        Err(InstructionError::AgentUnavailable { .. })
    ));
}

#[test]
fn metadata_includes_render_in_order_and_reverse_consumers_are_derived() {
    let fixture = Fixture::new();
    fixture.write(
        InstructionScope::Global,
        InstructionKind::Module,
        "first",
        &source("first", InstructionKind::Module, "", "first"),
    );
    fixture.write(
        InstructionScope::Global,
        InstructionKind::Module,
        "second",
        &source("second", InstructionKind::Module, "", "second"),
    );
    fixture.write(
        InstructionScope::Global,
        InstructionKind::System,
        "root",
        &source(
            "root",
            InstructionKind::System,
            "includes: [first, second]\n",
            "root",
        ),
    );
    let rendered = fixture
        .runtime()
        .render(&selector(InstructionKind::System, "root"), &json!({}))
        .unwrap();
    assert_eq!(rendered.text, "first\n\nsecond\n\nroot");
    let first = InstructionResourceRef {
        scope: InstructionScope::Global,
        kind: InstructionKind::Module,
        id: InstructionId::parse("first").unwrap(),
    };
    assert_eq!(
        rendered.graph.reverse_consumers[&first],
        vec![rendered.root]
    );
}

#[test]
fn addendum_targets_are_validated_and_appear_in_the_dependency_graph() {
    let fixture = Fixture::new();
    fixture.write(
        InstructionScope::Global,
        InstructionKind::Agent,
        "base-agent",
        &source(
            "base-agent",
            InstructionKind::Agent,
            "name: Base agent\ndescription: synthetic\navailability: both\n",
            "base",
        ),
    );
    fixture.write(
        InstructionScope::Project,
        InstructionKind::AgentAddendum,
        "project-addendum",
        &source(
            "project-addendum",
            InstructionKind::AgentAddendum,
            "target: global:base-agent\n",
            "addendum",
        ),
    );
    let runtime = fixture.runtime();
    let rendered = runtime
        .render(
            &InstructionSelector::project(
                InstructionKind::AgentAddendum,
                "project-addendum",
            )
            .unwrap(),
            &json!({}),
        )
        .unwrap();
    assert_eq!(rendered.text, "addendum");
    assert_eq!(rendered.graph.dependencies[&rendered.root].len(), 1);
}

#[test]
fn malformed_unidentified_resource_is_diagnostic_only() {
    let fixture = Fixture::new();
    let path = fixture
        .global
        .join(InstructionKind::System.directory())
        .join("INVALID NAME.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "not frontmatter").unwrap();
    fixture.write(
        InstructionScope::Global,
        InstructionKind::System,
        "healthy",
        &source("healthy", InstructionKind::System, "", "healthy"),
    );
    let runtime = fixture.runtime();
    assert_eq!(runtime.diagnostics().len(), 1);
    assert_eq!(
        runtime
            .render(&selector(InstructionKind::System, "healthy"), &json!({}))
            .unwrap()
            .text,
        "healthy"
    );
}
