use super::template::{TemplateSegment, parse_restricted_template};
use super::*;
use handlebars::Handlebars;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

#[derive(Clone, Debug)]
struct CatalogCandidate {
    path: PathBuf,
    parsed: Result<InstructionDocument, String>,
}

#[derive(Clone, Debug, Default)]
struct ScopedCandidates {
    global: Vec<CatalogCandidate>,
    project: Vec<CatalogCandidate>,
}

impl ScopedCandidates {
    fn for_scope(&self, scope: InstructionScope) -> &[CatalogCandidate] {
        match scope {
            InstructionScope::Global => &self.global,
            InstructionScope::Project => &self.project,
        }
    }

    fn for_scope_mut(&mut self, scope: InstructionScope) -> &mut Vec<CatalogCandidate> {
        match scope {
            InstructionScope::Global => &mut self.global,
            InstructionScope::Project => &mut self.project,
        }
    }
}

#[derive(Clone, Debug)]
pub struct InstructionRuntime {
    sources: InstructionSources,
    entries: BTreeMap<(InstructionKind, InstructionId), ScopedCandidates>,
    diagnostics: Vec<InstructionDiagnostic>,
    external_agents: Vec<ExternalAgentsInstruction>,
}

impl InstructionRuntime {
    /// Discover all known resource families. Invalid documents are retained as
    /// scoped catalog entries so an invalid project redefinition shadows global
    /// instead of silently falling back, while unrelated valid resources remain
    /// usable.
    pub fn discover(sources: InstructionSources) -> Self {
        let mut runtime = Self {
            sources,
            entries: BTreeMap::new(),
            diagnostics: Vec::new(),
            external_agents: Vec::new(),
        };
        runtime.discover_scope(
            InstructionScope::Global,
            runtime.sources.global_root.clone(),
        );
        if let Some(project_root) = runtime.sources.project_root.clone() {
            runtime.discover_scope(InstructionScope::Project, project_root);
        }
        runtime.load_external_agents();
        runtime
    }

    pub fn diagnostics(&self) -> &[InstructionDiagnostic] {
        &self.diagnostics
    }

    pub fn external_agents(&self) -> &[ExternalAgentsInstruction] {
        &self.external_agents
    }

    /// Dedicated ecosystem input rendered global-first and then project. Empty
    /// present files remain present and contribute no prose after their heading.
    pub fn render_external_agents(&self) -> String {
        self.external_agents
            .iter()
            .map(|instruction| {
                let heading = match instruction.scope {
                    InstructionScope::Global => "# Global Instructions (~/AGENTS.md)",
                    InstructionScope::Project => "# Project Instructions (AGENTS.md)",
                };
                if instruction.content.is_empty() {
                    heading.to_string()
                } else {
                    format!("{heading}\n\n{}", instruction.content.trim())
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn resources(&self) -> Vec<InstructionResourceSummary> {
        let mut out = Vec::new();
        for ((kind, id), scopes) in &self.entries {
            for scope in [InstructionScope::Global, InstructionScope::Project] {
                let candidates = scopes.for_scope(scope);
                if candidates.is_empty() {
                    continue;
                }
                let state = if candidates.len() > 1 {
                    ResourceValidationState::Ambiguous
                } else {
                    match &candidates[0].parsed {
                        Ok(_) => ResourceValidationState::Valid,
                        Err(detail) => ResourceValidationState::Invalid(detail.clone()),
                    }
                };
                out.push(InstructionResourceSummary {
                    resource: InstructionResourceRef {
                        scope,
                        kind: *kind,
                        id: id.clone(),
                    },
                    paths: candidates
                        .iter()
                        .map(|candidate| candidate.path.clone())
                        .collect(),
                    state,
                });
            }
        }
        out
    }

    pub fn resolve(
        &self,
        selector: &InstructionSelector,
    ) -> Result<&InstructionDocument, InstructionError> {
        let Some(scopes) = self.entries.get(&(selector.kind, selector.id.clone())) else {
            return Err(InstructionError::ResourceNotFound {
                selector: selector.clone(),
            });
        };
        let scope = match selector.scope {
            InstructionScopeSelector::Global => InstructionScope::Global,
            InstructionScopeSelector::Project => InstructionScope::Project,
            InstructionScopeSelector::Unqualified if !scopes.project.is_empty() => {
                InstructionScope::Project
            }
            InstructionScopeSelector::Unqualified => InstructionScope::Global,
        };
        self.resolve_scoped(selector, scope, scopes.for_scope(scope))
    }

    pub fn render<T: Serialize>(
        &self,
        selector: &InstructionSelector,
        values: &T,
    ) -> Result<RenderedInstruction, InstructionError> {
        let root = self.document_ref(self.resolve(selector)?);
        self.render_root(root, values)
    }

    pub fn render_agent<T: Serialize>(
        &self,
        selector: &InstructionSelector,
        requested: AgentAvailability,
        values: &T,
    ) -> Result<RenderedInstruction, InstructionError> {
        if selector.kind != InstructionKind::Agent {
            return Err(InstructionError::InvalidSelector {
                value: selector.to_string(),
                reason: "render_agent requires an agent selector".to_string(),
            });
        }
        let document = self.resolve(selector)?;
        let actual = document
            .metadata
            .agent
            .as_ref()
            .map(|metadata| metadata.availability)
            .ok_or_else(|| InstructionError::InvalidResource {
                resource: self.document_ref(document),
                path: document.path.clone(),
                detail: "agent metadata is unavailable".to_string(),
            })?;
        let available = matches!(actual, AgentAvailability::Both) || actual == requested;
        if !available {
            return Err(InstructionError::AgentUnavailable {
                resource: self.document_ref(document),
                requested,
                actual,
            });
        }
        self.render_root(self.document_ref(document), values)
    }

    pub fn render_registered<T: Serialize>(
        &self,
        registration: &ConsumerRegistration,
        values: &T,
    ) -> Result<RenderedInstruction, InstructionError> {
        let selector = InstructionSelector {
            scope: match registration.scope_policy {
                ConsumerScopePolicy::GlobalOnly => InstructionScopeSelector::Global,
                ConsumerScopePolicy::ProjectThenGlobal => InstructionScopeSelector::Unqualified,
            },
            kind: registration.kind,
            id: registration.id.clone(),
        };
        let document = match self.resolve(&selector) {
            Ok(document) => document,
            Err(InstructionError::ResourceNotFound { .. }) if registration.required => {
                let root = match registration.scope_policy {
                    ConsumerScopePolicy::GlobalOnly => &self.sources.global_root,
                    ConsumerScopePolicy::ProjectThenGlobal => self
                        .sources
                        .project_root
                        .as_ref()
                        .unwrap_or(&self.sources.global_root),
                };
                return Err(InstructionError::RegisteredResourceMissing {
                    key: registration.key.clone(),
                    selector,
                    expected_path: root.join(&registration.default_relative_path),
                });
            }
            Err(error) => return Err(error),
        };
        if document.body.is_empty() && !registration.empty_is_meaningful {
            return Err(InstructionError::EmptyResourceNotAllowed {
                key: registration.key.clone(),
                resource: self.document_ref(document),
            });
        }
        self.render_root(self.document_ref(document), values)
    }

    fn resolve_scoped<'a>(
        &'a self,
        selector: &InstructionSelector,
        scope: InstructionScope,
        candidates: &'a [CatalogCandidate],
    ) -> Result<&'a InstructionDocument, InstructionError> {
        if candidates.is_empty() {
            return Err(InstructionError::ResourceNotFound {
                selector: selector.clone(),
            });
        }
        if candidates.len() > 1 {
            return Err(InstructionError::AmbiguousResource {
                selector: selector.clone(),
                scope,
                paths: candidates
                    .iter()
                    .map(|candidate| candidate.path.clone())
                    .collect(),
            });
        }
        match &candidates[0].parsed {
            Ok(document) => Ok(document),
            Err(detail) => Err(InstructionError::InvalidResource {
                resource: InstructionResourceRef {
                    scope,
                    kind: selector.kind,
                    id: selector.id.clone(),
                },
                path: candidates[0].path.clone(),
                detail: detail.clone(),
            }),
        }
    }

    fn render_root<T: Serialize>(
        &self,
        root: InstructionResourceRef,
        values: &T,
    ) -> Result<RenderedInstruction, InstructionError> {
        let values =
            serde_json::to_value(values).map_err(|error| InstructionError::Serialization {
                detail: error.to_string(),
            })?;
        let plan = self.build_plan(&root)?;
        let mut rendered: BTreeMap<InstructionResourceRef, String> = BTreeMap::new();
        let mut handlebars = Handlebars::new();
        handlebars.set_strict_mode(true);
        handlebars.register_escape_fn(handlebars::no_escape);

        for resource in &plan.order {
            let node = plan.nodes.get(resource).expect("planned node exists");
            let mut parts = Vec::new();
            for include in &node.includes {
                let text = rendered
                    .get(include)
                    .expect("dependency rendered before consumer");
                if !text.is_empty() {
                    parts.push(text.clone());
                }
            }
            let body = match node.document.template_mode {
                TemplateMode::Plain => node.document.body.clone(),
                TemplateMode::Handlebars => {
                    render_segments(&handlebars, resource, &node.segments, &rendered, &values)?
                }
            };
            if !body.is_empty() {
                parts.push(body);
            }
            rendered.insert(resource.clone(), parts.join("\n\n"));
        }

        let text = rendered.remove(&root).expect("root rendered");
        Ok(RenderedInstruction {
            root,
            text,
            graph: plan.graph,
        })
    }

    fn build_plan(&self, root: &InstructionResourceRef) -> Result<RenderPlan, InstructionError> {
        #[derive(Clone, Copy, Eq, PartialEq)]
        enum Visit {
            Visiting,
            Done,
        }

        let mut visits: BTreeMap<InstructionResourceRef, Visit> = BTreeMap::new();
        let mut parents: BTreeMap<InstructionResourceRef, InstructionResourceRef> = BTreeMap::new();
        let mut stack = vec![(root.clone(), false)];
        let mut nodes = BTreeMap::new();
        let mut order = Vec::new();
        let mut graph = InstructionGraph::default();

        while let Some((resource, expanded)) = stack.pop() {
            if expanded {
                visits.insert(resource.clone(), Visit::Done);
                order.push(resource);
                continue;
            }
            match visits.get(&resource) {
                Some(Visit::Done) => continue,
                Some(Visit::Visiting) => continue,
                None => {}
            }
            visits.insert(resource.clone(), Visit::Visiting);
            let selector = InstructionSelector {
                scope: match resource.scope {
                    InstructionScope::Global => InstructionScopeSelector::Global,
                    InstructionScope::Project => InstructionScopeSelector::Project,
                },
                kind: resource.kind,
                id: resource.id.clone(),
            };
            let document = self.resolve(&selector)?.clone();
            let planned = self.plan_document(&resource, &document)?;
            graph
                .render_dependencies
                .insert(resource.clone(), planned.render_dependencies.clone());
            graph
                .validation_dependencies
                .insert(resource.clone(), planned.validation_dependencies.clone());
            for dependency in planned
                .render_dependencies
                .iter()
                .chain(&planned.validation_dependencies)
            {
                graph
                    .reverse_consumers
                    .entry(dependency.clone())
                    .or_default()
                    .push(resource.clone());
            }
            nodes.insert(
                resource.clone(),
                PlannedNode {
                    document,
                    segments: planned.segments,
                    includes: planned.includes,
                },
            );

            stack.push((resource.clone(), true));
            for dependency in planned.render_dependencies.into_iter().rev() {
                match visits.get(&dependency) {
                    Some(Visit::Done) => {}
                    Some(Visit::Visiting) => {
                        return Err(InstructionError::DependencyCycle {
                            chain: cycle_chain(&resource, &dependency, &parents),
                        });
                    }
                    None => {
                        parents.insert(dependency.clone(), resource.clone());
                        stack.push((dependency, false));
                    }
                }
            }
        }

        for consumers in graph.reverse_consumers.values_mut() {
            consumers.sort();
            consumers.dedup();
        }
        Ok(RenderPlan {
            nodes,
            order,
            graph,
        })
    }

    fn plan_document(
        &self,
        resource: &InstructionResourceRef,
        document: &InstructionDocument,
    ) -> Result<PlannedDocument, InstructionError> {
        let includes = document
            .metadata
            .includes
            .iter()
            .map(|selector| {
                self.resolve(selector)
                    .map(|document| self.document_ref(document))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut render_dependencies = includes.clone();
        let mut validation_dependencies = Vec::new();
        if let Some(addendum) = &document.metadata.addendum {
            validation_dependencies.push(self.document_ref(self.resolve(&addendum.target)?));
        }
        let segments = match document.template_mode {
            TemplateMode::Plain => vec![PlannedSegment::Text(document.body.clone())],
            TemplateMode::Handlebars => parse_restricted_template(resource, &document.body)?
                .into_iter()
                .map(|segment| match segment {
                    TemplateSegment::Text(text) => Ok(PlannedSegment::Text(text)),
                    TemplateSegment::Expression(expression) => {
                        Ok(PlannedSegment::Expression(expression))
                    }
                    TemplateSegment::Partial(selector) => {
                        let dependency = self.document_ref(self.resolve(&selector)?);
                        render_dependencies.push(dependency.clone());
                        Ok(PlannedSegment::Partial(dependency))
                    }
                })
                .collect::<Result<Vec<_>, InstructionError>>()?,
        };
        let mut seen = BTreeSet::new();
        render_dependencies.retain(|dependency| seen.insert(dependency.clone()));
        let mut seen_validation = BTreeSet::new();
        validation_dependencies.retain(|dependency| seen_validation.insert(dependency.clone()));
        Ok(PlannedDocument {
            segments,
            includes,
            render_dependencies,
            validation_dependencies,
        })
    }

    fn document_ref(&self, document: &InstructionDocument) -> InstructionResourceRef {
        InstructionResourceRef {
            scope: document.scope,
            kind: document.kind,
            id: document.id.clone(),
        }
    }

    fn discover_scope(&mut self, scope: InstructionScope, root: PathBuf) {
        for kind in [
            InstructionKind::System,
            InstructionKind::Agent,
            InstructionKind::AgentAddendum,
            InstructionKind::Module,
            InstructionKind::Notification,
            InstructionKind::ToolGuidance,
        ] {
            let directory = root.join(kind.directory());
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    self.diagnostics.push(InstructionDiagnostic {
                        scope,
                        path: directory,
                        detail: error.to_string(),
                    });
                    continue;
                }
            };
            let mut paths = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "md"))
                .collect::<Vec<_>>();
            paths.sort();
            for path in paths {
                self.discover_file(scope, kind, path);
            }
        }

        let skills_root = root.join(InstructionKind::Skill.directory());
        let entries = match fs::read_dir(&skills_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                self.diagnostics.push(InstructionDiagnostic {
                    scope,
                    path: skills_root,
                    detail: error.to_string(),
                });
                return;
            }
        };
        let mut paths = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("SKILL.md"))
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            self.discover_file(scope, InstructionKind::Skill, path);
        }
    }

    fn discover_file(
        &mut self,
        scope: InstructionScope,
        expected_kind: InstructionKind,
        path: PathBuf,
    ) {
        let fallback_id = fallback_id(&path, expected_kind);
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                let detail = format!("could not read complete UTF-8 resource: {error}");
                self.diagnostics.push(InstructionDiagnostic {
                    scope,
                    path: path.clone(),
                    detail: detail.clone(),
                });
                if let Some(id) = fallback_id {
                    self.entries
                        .entry((expected_kind, id))
                        .or_default()
                        .for_scope_mut(scope)
                        .push(CatalogCandidate {
                            path,
                            parsed: Err(detail),
                        });
                }
                return;
            }
        };
        let parsed = parse_document(scope, expected_kind, &path, &source);
        let candidate_id = parsed
            .as_ref()
            .map(|document| document.id.clone())
            .ok()
            .or_else(|| frontmatter_candidate_id(expected_kind, &source))
            .or(fallback_id);
        let Some(id) = candidate_id else {
            let detail = parsed
                .err()
                .unwrap_or_else(|| "resource identity is unavailable".to_string());
            self.diagnostics.push(InstructionDiagnostic {
                scope,
                path,
                detail,
            });
            return;
        };
        if let Err(detail) = &parsed {
            self.diagnostics.push(InstructionDiagnostic {
                scope,
                path: path.clone(),
                detail: detail.clone(),
            });
        }
        self.entries
            .entry((expected_kind, id))
            .or_default()
            .for_scope_mut(scope)
            .push(CatalogCandidate { path, parsed });
    }

    fn load_external_agents(&mut self) {
        let sources = [
            (
                InstructionScope::Global,
                self.sources.global_agents_md.clone(),
            ),
            (
                InstructionScope::Project,
                self.sources.project_agents_md.clone(),
            ),
        ];
        for (scope, path) in sources {
            let Some(path) = path else { continue };
            match fs::read_to_string(&path) {
                Ok(content) => self.external_agents.push(ExternalAgentsInstruction {
                    scope,
                    path,
                    content,
                }),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => self.diagnostics.push(InstructionDiagnostic {
                    scope,
                    path,
                    detail: error.to_string(),
                }),
            }
        }
    }
}

#[derive(Clone, Debug)]
struct RenderPlan {
    nodes: BTreeMap<InstructionResourceRef, PlannedNode>,
    order: Vec<InstructionResourceRef>,
    graph: InstructionGraph,
}

#[derive(Clone, Debug)]
struct PlannedNode {
    document: InstructionDocument,
    segments: Vec<PlannedSegment>,
    includes: Vec<InstructionResourceRef>,
}

#[derive(Clone, Debug)]
struct PlannedDocument {
    segments: Vec<PlannedSegment>,
    includes: Vec<InstructionResourceRef>,
    render_dependencies: Vec<InstructionResourceRef>,
    validation_dependencies: Vec<InstructionResourceRef>,
}

#[derive(Clone, Debug)]
enum PlannedSegment {
    Text(String),
    Expression(String),
    Partial(InstructionResourceRef),
}

fn render_segments(
    handlebars: &Handlebars<'_>,
    resource: &InstructionResourceRef,
    segments: &[PlannedSegment],
    rendered: &BTreeMap<InstructionResourceRef, String>,
    values: &Value,
) -> Result<String, InstructionError> {
    let mut output = String::new();
    for segment in segments {
        match segment {
            PlannedSegment::Text(text) => output.push_str(text),
            PlannedSegment::Expression(expression) => {
                let value = handlebars
                    .render_template(expression, values)
                    .map_err(|error| InstructionError::Render {
                        resource: resource.clone(),
                        detail: error.to_string(),
                    })?;
                output.push_str(&value);
            }
            PlannedSegment::Partial(dependency) => output.push_str(
                rendered
                    .get(dependency)
                    .expect("partial dependency rendered before consumer"),
            ),
        }
    }
    Ok(output)
}

fn cycle_chain(
    current: &InstructionResourceRef,
    dependency: &InstructionResourceRef,
    parents: &BTreeMap<InstructionResourceRef, InstructionResourceRef>,
) -> Vec<InstructionResourceRef> {
    let mut chain = vec![dependency.clone(), current.clone()];
    let mut cursor = current;
    let mut guard = 0usize;
    while cursor != dependency && guard <= parents.len() {
        let Some(parent) = parents.get(cursor) else {
            break;
        };
        chain.push(parent.clone());
        cursor = parent;
        guard += 1;
    }
    chain.reverse();
    if chain.last() != Some(dependency) {
        chain.push(dependency.clone());
    }
    chain
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct ResourceFrontmatter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<InstructionKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    availability: Option<AgentAvailability>,
    #[serde(default, skip_serializing_if = "is_plain")]
    template: TemplateMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    includes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    #[serde(
        default,
        rename = "allowed-tools",
        skip_serializing_if = "Option::is_none"
    )]
    allowed_tools: Option<AllowedTools>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum AllowedTools {
    List(Vec<String>),
    Csv(String),
}

fn is_plain(mode: &TemplateMode) -> bool {
    *mode == TemplateMode::Plain
}

fn parse_document(
    scope: InstructionScope,
    expected_kind: InstructionKind,
    path: &Path,
    source: &str,
) -> Result<InstructionDocument, String> {
    let (frontmatter_source, body) = split_frontmatter(source)?;
    let raw: ResourceFrontmatter =
        serde_yaml::from_str(frontmatter_source).map_err(|error| error.to_string())?;
    if let Some(actual_kind) = raw.kind
        && actual_kind != expected_kind
    {
        return Err(format!(
            "frontmatter kind {actual_kind} does not match {} directory",
            expected_kind.directory()
        ));
    }
    validate_kind_specific_frontmatter(&raw, expected_kind)?;
    let id_value = raw
        .id
        .as_deref()
        .or_else(|| {
            (expected_kind == InstructionKind::Skill)
                .then_some(raw.name.as_deref())
                .flatten()
        })
        .ok_or_else(|| {
            "frontmatter requires id (skills may use name as their stable ID)".to_string()
        })?;
    let id = InstructionId::parse(id_value.to_string()).map_err(|error| error.to_string())?;
    let includes = raw
        .includes
        .iter()
        .map(|reference| InstructionSelector::parse(InstructionKind::Module, reference))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let agent = if expected_kind == InstructionKind::Agent {
        Some(AgentMetadata {
            availability: raw
                .availability
                .ok_or_else(|| "agent frontmatter requires availability".to_string())?,
        })
    } else {
        None
    };
    if expected_kind == InstructionKind::Agent {
        nonempty(raw.name.as_deref(), "agent name")?;
        nonempty(raw.description.as_deref(), "agent description")?;
    }
    let addendum = if expected_kind == InstructionKind::AgentAddendum {
        Some(AddendumMetadata {
            target: InstructionSelector::parse(
                InstructionKind::Agent,
                raw.target
                    .as_deref()
                    .ok_or_else(|| "agent addendum frontmatter requires target".to_string())?,
            )
            .map_err(|error| error.to_string())?,
        })
    } else {
        None
    };
    let allowed_tools = raw.allowed_tools.map(|allowed| match allowed {
        AllowedTools::List(tools) => tools,
        AllowedTools::Csv(tools) => tools
            .split(',')
            .map(str::trim)
            .filter(|tool| !tool.is_empty())
            .map(str::to_string)
            .collect(),
    });
    Ok(InstructionDocument {
        id,
        kind: expected_kind,
        scope,
        template_mode: raw.template,
        metadata: InstructionMetadata {
            display_name: raw.name.clone(),
            description: raw.description.clone(),
            agent,
            addendum,
            includes,
            allowed_tools,
        },
        body: body.to_string(),
        path: path.to_path_buf(),
    })
}

fn validate_kind_specific_frontmatter(
    raw: &ResourceFrontmatter,
    expected_kind: InstructionKind,
) -> Result<(), String> {
    if raw.availability.is_some() && expected_kind != InstructionKind::Agent {
        return Err(format!(
            "frontmatter field 'availability' is only valid for agent resources, not {expected_kind}"
        ));
    }
    if raw.target.is_some() && expected_kind != InstructionKind::AgentAddendum {
        return Err(format!(
            "frontmatter field 'target' is only valid for agent-addendum resources, not {expected_kind}"
        ));
    }
    if raw.allowed_tools.is_some() && expected_kind != InstructionKind::Skill {
        return Err(format!(
            "frontmatter field 'allowed-tools' is only valid for skill resources, not {expected_kind}"
        ));
    }
    Ok(())
}

fn nonempty(value: Option<&str>, label: &str) -> Result<String, String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{label} must be non-empty"))
}

fn split_frontmatter(source: &str) -> Result<(&str, &str), String> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let (after_open, newline_len) = if let Some(rest) = source.strip_prefix("---\r\n") {
        (rest, 2usize)
    } else if let Some(rest) = source.strip_prefix("---\n") {
        (rest, 1usize)
    } else {
        return Err("missing opening YAML frontmatter delimiter".to_string());
    };

    let mut offset = 0usize;
    for line in after_open.split_inclusive(['\n']) {
        let line_without_newline = line.trim_end_matches(['\r', '\n']);
        if line_without_newline == "---" {
            let frontmatter = &after_open[..offset];
            let body_start = offset + line.len();
            let mut body = &after_open[body_start..];
            if body.starts_with("\r\n") {
                body = &body[2..];
            } else if body.starts_with('\n') {
                body = &body[1..];
            }
            return Ok((frontmatter.trim_end_matches(['\r', '\n']), body));
        }
        offset += line.len();
    }
    let _ = newline_len;
    Err("missing closing YAML frontmatter delimiter".to_string())
}

fn frontmatter_candidate_id(kind: InstructionKind, source: &str) -> Option<InstructionId> {
    let (frontmatter, _) = split_frontmatter(source).ok()?;
    let value: serde_yaml::Value = serde_yaml::from_str(frontmatter).ok()?;
    let mapping = value.as_mapping()?;
    let id = mapping
        .get(serde_yaml::Value::String("id".to_string()))
        .and_then(serde_yaml::Value::as_str)
        .or_else(|| {
            (kind == InstructionKind::Skill)
                .then(|| {
                    mapping
                        .get(serde_yaml::Value::String("name".to_string()))
                        .and_then(serde_yaml::Value::as_str)
                })
                .flatten()
        })?;
    InstructionId::parse(id.to_string()).ok()
}

fn fallback_id(path: &Path, kind: InstructionKind) -> Option<InstructionId> {
    let value = if kind == InstructionKind::Skill {
        path.parent()?.file_name()?.to_str()?
    } else {
        path.file_stem()?.to_str()?
    };
    InstructionId::parse(value.to_string()).ok()
}

pub(super) fn serialize_document(
    document: &InstructionDocument,
) -> Result<String, InstructionError> {
    let allowed_tools = document
        .metadata
        .allowed_tools
        .clone()
        .map(AllowedTools::List);
    let raw = ResourceFrontmatter {
        id: Some(document.id.to_string()),
        name: document.metadata.display_name.clone(),
        description: document.metadata.description.clone(),
        kind: Some(document.kind),
        availability: document
            .metadata
            .agent
            .as_ref()
            .map(|agent| agent.availability),
        template: document.template_mode,
        includes: document
            .metadata
            .includes
            .iter()
            .map(selector_source)
            .collect(),
        target: document
            .metadata
            .addendum
            .as_ref()
            .map(|addendum| selector_source(&addendum.target)),
        allowed_tools,
    };
    let yaml = serde_yaml::to_string(&raw).map_err(|error| InstructionError::Serialization {
        detail: error.to_string(),
    })?;
    let yaml = yaml.strip_prefix("---\n").unwrap_or(&yaml);
    Ok(format!("---\n{}---\n\n{}", yaml, document.body))
}

fn selector_source(selector: &InstructionSelector) -> String {
    match selector.scope {
        InstructionScopeSelector::Unqualified => selector.id.to_string(),
        InstructionScopeSelector::Global => format!("global:{}", selector.id),
        InstructionScopeSelector::Project => format!("project:{}", selector.id),
    }
}
