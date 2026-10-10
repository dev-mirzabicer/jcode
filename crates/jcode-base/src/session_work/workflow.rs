//! The workflow file grammar: parse, validate and render.
//!
//! ```text
//! - [x] ground: Understand the sidebar and its constraints {research}
//! - [>] proto: Prototype three sidebars {prototype/frontend}
//!   - [x] proto-a: Minimal rail
//!   - [>] proto-b: Dense panel
//! - [?] after: Decide the rest after Mirza picks one
//! - [-] perf: Performance pass
//!   > skipped: CSS-only change
//! ```
//!
//! The parser is strict and reports every problem with its line. Accepted
//! text is stored exactly as written; `render` produces the canonical form.
use jcode_session_work_types::{ModuleId, ModuleStatus, ModuleTypeRef, Workflow, WorkflowModule};
use std::fmt;

/// One grammar or rule violation. `line` is 1-based; whole-file problems have none.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowIssue {
    pub line: Option<usize>,
    pub message: String,
}

/// Every problem found in a rejected workflow text, in line order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowErrors(pub Vec<WorkflowIssue>);

impl fmt::Display for WorkflowErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("workflow.md was not changed:")?;
        for issue in &self.0 {
            match issue.line {
                Some(line) => write!(f, "\n- line {line}: {}", issue.message)?,
                None => write!(f, "\n- {}", issue.message)?,
            }
        }
        Ok(())
    }
}

impl std::error::Error for WorkflowErrors {}

struct Parsed {
    depth: usize,
    line: usize,
    module: WorkflowModule,
}

/// Parse and validate a complete workflow text.
pub fn parse_workflow(text: &str) -> Result<Workflow, WorkflowErrors> {
    let mut issues = Vec::new();
    let mut flat: Vec<Parsed> = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        let line = index + 1;
        let content = raw.strip_suffix('\r').unwrap_or(raw);
        if content.trim().is_empty() {
            continue;
        }
        let indent = content.len() - content.trim_start_matches([' ', '\t']).len();
        if content[..indent].contains('\t') {
            issues.push(issue(
                line,
                "use spaces, not tabs, to nest modules and notes",
            ));
            continue;
        }
        let rest = &content[indent..];
        if let Some(note) = rest.strip_prefix('>') {
            match flat.last_mut() {
                Some(last) if indent == last.depth * 2 + 2 => {
                    last.module.notes.push(note.trim().to_string());
                }
                Some(last) => issues.push(issue(
                    line,
                    &format!(
                        "indent this note {} spaces, two deeper than its module `{}`",
                        last.depth * 2 + 2,
                        last.module.id
                    ),
                )),
                None => issues.push(issue(line, "a note must follow the module it belongs to")),
            }
            continue;
        }
        if !rest.starts_with("- [") {
            issues.push(issue(
                line,
                "expected a module (`- [ ] id: title`) or a note (`> text`)",
            ));
            continue;
        }
        if indent % 2 != 0 {
            issues.push(issue(line, "nest modules with two spaces per level"));
            continue;
        }
        let depth = indent / 2;
        let parent_depth = flat.last().map(|last| last.depth);
        if depth > parent_depth.map_or(0, |parent| parent + 1) {
            issues.push(issue(
                line,
                "this module is nested more than one level below the module above it",
            ));
            continue;
        }
        match parse_module_line(rest) {
            Ok(module) => flat.push(Parsed {
                depth,
                line,
                module,
            }),
            Err(message) => issues.push(issue(line, &message)),
        }
    }
    if flat.is_empty() && issues.is_empty() {
        issues.push(WorkflowIssue {
            line: None,
            message: "a workflow needs at least one module. A workflow can't be emptied: skip the modules you drop and give the reason".into(),
        });
    }
    if !issues.is_empty() {
        return Err(WorkflowErrors(issues));
    }
    let lines: Vec<(String, usize)> = flat
        .iter()
        .map(|parsed| (parsed.module.id.to_string(), parsed.line))
        .collect();
    let workflow = Workflow {
        modules: build_tree(flat),
    };
    validate(&workflow, &lines)?;
    Ok(workflow)
}

fn issue(line: usize, message: &str) -> WorkflowIssue {
    WorkflowIssue {
        line: Some(line),
        message: message.to_string(),
    }
}

fn parse_module_line(rest: &str) -> Result<WorkflowModule, String> {
    let after_open = &rest[3..];
    let mut chars = after_open.char_indices();
    let (_, marker) = chars
        .next()
        .ok_or("expected a marker such as `[ ]` after `- `")?;
    let status = ModuleStatus::from_marker(marker)
        .ok_or_else(|| format!("unknown marker `[{marker}]`; use [ ], [>], [x], [-] or [?]"))?;
    let after_marker = &after_open[marker.len_utf8()..];
    let body = after_marker
        .strip_prefix("] ")
        .ok_or("expected `] ` after the marker, as in `- [ ] id: title`")?;
    let (id, title) = body
        .split_once(':')
        .ok_or("expected `id: title` after the marker")?;
    let id = ModuleId::parse(id.trim_end()).map_err(|message| {
        if id.trim().is_empty() {
            "a module needs an ID before its title, as in `- [ ] id: title`".to_string()
        } else {
            message
        }
    })?;
    if id.as_str().len() != body.split_once(':').map_or(0, |(raw, _)| raw.len()) {
        return Err(format!(
            "write the module ID directly before the colon, as in `{id}: title`"
        ));
    }
    let title = title
        .strip_prefix(' ')
        .ok_or_else(|| format!("put one space after `{id}:`"))?;
    let (title, module_type) = split_type(title)?;
    let title = title.trim_end();
    if title.trim().is_empty() {
        return Err(format!("module `{id}` needs a title after `{id}: `"));
    }
    Ok(WorkflowModule {
        id,
        title: title.to_string(),
        module_type,
        status,
        notes: Vec::new(),
        children: Vec::new(),
    })
}

fn split_type(title: &str) -> Result<(&str, Option<ModuleTypeRef>), String> {
    let trimmed = title.trim_end();
    let Some(without_close) = trimmed.strip_suffix('}') else {
        return Ok((title, None));
    };
    let Some(open) = without_close.rfind('{') else {
        return Ok((title, None));
    };
    let label = &without_close[open + 1..];
    let before = &without_close[..open];
    if !before.ends_with(' ') {
        return Ok((title, None));
    }
    let token = |value: &str| {
        let mut bytes = value.bytes();
        bytes
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
            && bytes.all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
            })
    };
    let (name, subtype) = match label.split_once('/') {
        Some((name, subtype)) => (name, Some(subtype)),
        None => (label, None),
    };
    if !token(name) || subtype.is_some_and(|subtype| !token(subtype)) {
        return Err(format!(
            "module type `{{{label}}}` must be `{{type}}` or `{{type/subtype}}` in lowercase letters, digits, `-` or `_`"
        ));
    }
    Ok((
        before,
        Some(ModuleTypeRef {
            name: name.to_string(),
            subtype: subtype.map(str::to_string),
        }),
    ))
}

fn build_tree(flat: Vec<Parsed>) -> Vec<WorkflowModule> {
    // Each stack entry holds a module whose children are still being read.
    let mut roots = Vec::new();
    let mut stack: Vec<(usize, WorkflowModule)> = Vec::new();
    fn close(
        stack: &mut Vec<(usize, WorkflowModule)>,
        roots: &mut Vec<WorkflowModule>,
        depth: usize,
    ) {
        while stack.last().is_some_and(|(open, _)| *open >= depth) {
            let (_, module) = stack.pop().expect("checked");
            match stack.last_mut() {
                Some((_, parent)) => parent.children.push(module),
                None => roots.push(module),
            }
        }
    }
    for parsed in flat {
        close(&mut stack, &mut roots, parsed.depth);
        stack.push((parsed.depth, parsed.module));
    }
    close(&mut stack, &mut roots, 0);
    roots
}

/// The status a parent must show, given its children.
pub fn derived_parent_status(children: &[WorkflowModule]) -> ModuleStatus {
    if children
        .iter()
        .any(|child| child.status == ModuleStatus::Active)
    {
        ModuleStatus::Active
    } else if children.iter().all(|child| child.status.is_finished()) {
        if children
            .iter()
            .all(|child| child.status == ModuleStatus::Skipped)
        {
            ModuleStatus::Skipped
        } else {
            ModuleStatus::Done
        }
    } else if children
        .iter()
        .all(|child| child.status == ModuleStatus::Undetermined)
    {
        ModuleStatus::Undetermined
    } else {
        ModuleStatus::Pending
    }
}

fn validate(workflow: &Workflow, lines: &[(String, usize)]) -> Result<(), WorkflowErrors> {
    let line_of = |id: &ModuleId| {
        lines
            .iter()
            .find(|(candidate, _)| candidate == id.as_str())
            .map(|(_, line)| *line)
    };
    let mut issues = Vec::new();
    let mut seen: Vec<(&str, usize)> = Vec::new();
    for (index, (id, line)) in lines.iter().enumerate() {
        if let Some((_, first)) = seen.iter().find(|(seen, _)| seen == id) {
            issues.push(issue(
                *line,
                &format!("module ID `{id}` is already used on line {first}; IDs must be unique"),
            ));
        } else {
            seen.push((id, lines[index].1));
        }
    }
    let mut active = Vec::new();
    for (_, module) in workflow.walk() {
        let line = line_of(&module.id);
        if module.children.is_empty() {
            if module.status == ModuleStatus::Active {
                active.push((module.id.clone(), line));
            }
            if module.status == ModuleStatus::Skipped && module.skip_reason().is_none() {
                issues.push(WorkflowIssue {
                    line,
                    message: format!(
                        "skipped module `{}` needs a `> skipped: <reason>` note",
                        module.id
                    ),
                });
            }
        } else {
            let expected = derived_parent_status(&module.children);
            if module.status != expected {
                issues.push(WorkflowIssue {
                    line,
                    message: format!(
                        "`{}` is [{}] but its children make it [{}]; a parent's marker follows its children",
                        module.id,
                        module.status.marker(),
                        expected.marker()
                    ),
                });
            }
        }
    }
    if active.len() > 1 {
        let listed = active
            .iter()
            .map(|(id, line)| match line {
                Some(line) => format!("`{id}` (line {line})"),
                None => format!("`{id}`"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        issues.push(WorkflowIssue {
            line: active[1].1,
            message: format!(
                "only one module can be active at a time; {listed} are all [>]. Mark the others [ ], [x] or [-]"
            ),
        });
    }
    if issues.is_empty() {
        Ok(())
    } else {
        issues.sort_by_key(|issue| issue.line.unwrap_or(0));
        Err(WorkflowErrors(issues))
    }
}

/// The canonical text of a workflow.
pub fn render_workflow(workflow: &Workflow) -> String {
    let mut out = String::new();
    for (depth, module) in workflow.walk() {
        let indent = "  ".repeat(depth);
        out.push_str(&format!(
            "{indent}- [{}] {}: {}",
            module.status.marker(),
            module.id,
            module.title
        ));
        if let Some(module_type) = &module.module_type {
            out.push_str(&format!(" {{{module_type}}}"));
        }
        out.push('\n');
        for note in &module.notes {
            out.push_str(&format!("{indent}  > {note}\n"));
        }
    }
    out
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;
