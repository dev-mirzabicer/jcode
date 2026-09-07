use super::metadata::selector;
use super::*;

pub(super) fn previews(
    sources: InstructionSources,
    request: &InstructionCommitRequest,
) -> Result<(Vec<String>, Vec<InstructionEditPreview>)> {
    let edit_root = sources
        .project_root
        .as_ref()
        .unwrap_or(&sources.global_root)
        .clone();
    let edited_scope = if sources.project_root.is_some() {
        InstructionScope::Project
    } else {
        InstructionScope::Global
    };
    let runtime = InstructionRuntime::discover(sources);
    let resources = runtime.resources();
    let paths = request
        .mutations
        .iter()
        .flat_map(InstructionFileMutation::affected_paths)
        .collect::<BTreeSet<_>>();
    let mut affected = resources
        .iter()
        .filter(|summary| {
            summary.resource.scope == edited_scope
                && summary.paths.iter().any(|path| {
                    path.strip_prefix(&edit_root)
                        .is_ok_and(|path| paths.iter().any(|target| target.as_path() == path))
                })
        })
        .map(|summary| summary.resource.clone())
        .collect::<BTreeSet<_>>();
    // Reverse reachability is derived from the runtime graph, including addenda.
    loop {
        let mut added = false;
        for summary in &resources {
            if affected.contains(&summary.resource) {
                continue;
            }
            if let Ok(graph) = runtime.validate_graph(&selector(&summary.resource))
                && graph
                    .render_dependencies
                    .values()
                    .chain(graph.validation_dependencies.values())
                    .flatten()
                    .any(|resource| affected.contains(resource))
            {
                added |= affected.insert(summary.resource.clone());
            }
        }
        if !added {
            break;
        }
    }
    let contracts = registrations()?;
    let mut errors = Vec::new();
    let mut previews = Vec::new();
    for resource in &affected {
        let selected = selector(resource);
        let matching = contracts
            .iter()
            .filter(|contract| contract.id == resource.id && contract.kind == resource.kind)
            .collect::<Vec<_>>();
        let values = notification::preview_values(resource.id.as_str())
            .map_err(|error| fail("typed preview values", error))?
            .or(workflow::preview_values(resource.id.as_str())
                .map_err(|error| fail("typed preview values", error))?)
            .or(
                super::super::composition::composition_preview_values(resource.id.as_str())
                    .map_err(|error| fail("typed preview values", error))?,
            );
        let result = if let Some(contract) = matching.first() {
            let mut scoped = (*contract).clone();
            scoped.scope_policy = if resource.scope == InstructionScope::Global {
                ConsumerScopePolicy::GlobalOnly
            } else {
                ConsumerScopePolicy::ProjectOnly
            };
            runtime.render_registered(&scoped, &values.unwrap_or_else(|| serde_json::json!({})))
        } else {
            let values = if resource.kind == InstructionKind::Module {
                module_sample(&runtime, resource)?
            } else {
                serde_json::json!({})
            };
            runtime.render(&selected, &values)
        };
        match result {
            Ok(rendered) => previews.push(InstructionEditPreview {
                title: format!("{resource} · synthetic typed preview, not active instructions"),
                content: rendered.text,
            }),
            Err(error) => errors.push(format!("{resource}: {error}")),
        }
    }
    // Required singleton deletion cannot be smuggled through raw source repair.
    for contract in contracts {
        if contract.required
            && paths
                .iter()
                .any(|path| path.as_path() == contract.default_relative_path)
        {
            let mut scoped = contract;
            scoped.scope_policy = if edited_scope == InstructionScope::Global {
                ConsumerScopePolicy::GlobalOnly
            } else {
                ConsumerScopePolicy::ProjectThenGlobal
            };
            if let Err(error) = runtime.validate_registered_graph(&scoped) {
                errors.push(error.to_string());
            }
        }
    }
    Ok((errors, previews))
}

fn module_sample(
    runtime: &InstructionRuntime,
    resource: &InstructionResourceRef,
) -> Result<serde_json::Value> {
    let graph = runtime
        .validate_graph(&selector(resource))
        .map_err(|error| fail("module graph", error))?;
    let resources = std::iter::once(resource)
        .chain(graph.render_dependencies.keys())
        .collect::<BTreeSet<_>>();
    let mut values = serde_json::json!({});
    for resource in resources {
        let document = runtime
            .resolve(&selector(resource))
            .map_err(|error| fail("module source", error))?;
        if document.template_mode != TemplateMode::Handlebars {
            continue;
        }
        for segment in super::super::template::parse_restricted_template(resource, &document.body)
            .map_err(|error| fail("module template", error))?
        {
            if let super::super::template::TemplateSegment::Expression(expression) = segment {
                let name = expression.trim_matches(['{', '}']).trim();
                if name.starts_with('!') {
                    continue;
                }
                let mut cursor = &mut values;
                let parts = name.split('.').collect::<Vec<_>>();
                for (index, part) in parts.iter().enumerate() {
                    if !cursor.is_object() {
                        *cursor = serde_json::json!({});
                    }
                    let object = cursor
                        .as_object_mut()
                        .ok_or_else(|| fail("module preview", "Cannot build synthetic values"))?;
                    cursor = object
                        .entry((*part).to_string())
                        .or_insert(serde_json::Value::Null);
                    if index + 1 == parts.len() {
                        *cursor = serde_json::Value::String(format!("[synthetic:{name}]"));
                    }
                }
            }
        }
    }
    Ok(values)
}
