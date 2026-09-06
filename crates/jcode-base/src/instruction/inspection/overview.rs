//! Human-facing summaries of typed domain state, not another policy resolver.
use super::*;

pub(super) fn repository_overview(store: &Repository) -> String {
    let mut out = format!(
        "REPOSITORY OVERVIEW\n\nType: {}\nLocation: {}\nState: {}\n\n",
        store.row.kind, store.row.root, store.row.health
    );
    let Some(state) = &store.state else {
        out.push_str(&store.detail);
        return out;
    };
    out.push_str(&format!(
        "Branch: {}\nHEAD: {}\n",
        state.branch.as_deref().unwrap_or(if state.detached {
            "Detached HEAD (inspection is available; saving requires a branch)"
        } else {
            "No branch / no commits"
        }),
        state.head.as_deref().unwrap_or("No commit")
    ));
    if let Some(upstream) = &state.upstream {
        out.push_str(&format!("\nUPSTREAM\nTracking: {}\nRemote: {}\nBranch: {}\nAhead: {} commits\nBehind: {} commits\nThese counts use local tracking refs. Inspection does not fetch.\n",upstream.reference,upstream.remote.as_deref().unwrap_or("Not configured"),upstream.branch.as_deref().unwrap_or("Not configured"),upstream.ahead,upstream.behind));
    } else {
        out.push_str("\nUpstream: not configured\n");
    }
    out.push_str("\nWORKING TREE\n");
    if state.changes.is_empty() {
        out.push_str("Clean. No staged, unstaged or untracked changes.\n");
    }
    for change in &state.changes {
        out.push_str(&format!(
            "{}\n  Index: {} | Working file: {}{}\n",
            change.path.display(),
            delta(change.index),
            delta(change.worktree),
            if change.conflicted { " | CONFLICT" } else { "" }
        ));
        if let Some(original) = &change.original_path {
            out.push_str(&format!("  Original path: {}\n", original.display()));
        }
    }
    if !state.conflicts.is_empty() {
        out.push_str("\nUNRESOLVED CONFLICTS\n");
        for path in &state.conflicts {
            out.push_str(&format!("{}\n", path.display()));
        }
    }
    if let Some(parent) = &state.parent_gitlink {
        out.push_str(&format!("\nPARENT PROJECT POINTER\nPath: {}\n.gitmodules changed: {}\nGitlink changed: {}\nRecorded commit: {}\nChecked-out commit: {}\nThe parent project is never committed by this manager.\n",parent.path.display(),yes(parent.gitmodules_changed),yes(parent.gitlink_changed),parent.recorded_commit.as_deref().unwrap_or("None"),parent.checked_out_commit.as_deref().unwrap_or("None")));
    }
    if let Some(lease) = &state.active_mutation {
        out.push_str(&format!("\nACTIVE MUTATION LEASE\nOperation: {}\nProcess: {}\nAcquired: {}\nDiagnostic deadline: {}\nRead-only inspection remains available.\n",lease.operation_id,lease.pid,lease.acquired_at,lease.expires_at));
    } else {
        out.push_str("\nMutation lease: none active\n");
    }
    for warning in &state.configuration_warnings {
        out.push_str(&format!("\nCONFIGURATION WARNING\n{warning}\n"));
    }
    if let Some(reference) = &store.reference {
        if let Some(path) = &reference.project_config_path {
            out.push_str(&format!("\nConfiguration file: {}\n", path.display()));
        }
        if let Some(branch) = &reference.configured_branch {
            out.push_str(&format!("Configured branch: {branch}\n"));
        }
    }
    out
}
fn delta(value: Option<GitDelta>) -> &'static str {
    match value {
        None => "unchanged",
        Some(GitDelta::Added) => "added",
        Some(GitDelta::Modified) => "modified",
        Some(GitDelta::Deleted) => "deleted",
        Some(GitDelta::Renamed) => "renamed",
        Some(GitDelta::Copied) => "copied",
        Some(GitDelta::TypeChanged) => "type changed",
        Some(GitDelta::Unmerged) => "unmerged",
        Some(GitDelta::Untracked) => "untracked",
        Some(GitDelta::Unknown) => "unknown",
    }
}
pub(super) fn yes(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
pub(super) fn scope_policy(value: ConsumerScopePolicy) -> &'static str {
    match value {
        ConsumerScopePolicy::GlobalOnly => "Global only",
        ConsumerScopePolicy::ProjectOnly => "Project only",
        ConsumerScopePolicy::ProjectThenGlobal => "Project definition first, then global",
    }
}
pub(super) fn document_overview(document: &InstructionDocument) -> String {
    let mut out = format!(
        "\nINSTRUCTION METADATA\nTemplate: {}\nDescription: {}\n",
        match document.template_mode {
            TemplateMode::Plain => "Plain text (literal)",
            TemplateMode::Handlebars => "Restricted Handlebars",
        },
        document
            .metadata
            .description
            .as_deref()
            .unwrap_or("Not provided")
    );
    if let Some(agent) = &document.metadata.agent {
        out.push_str(&format!(
            "Availability: {}\n",
            match agent.availability {
                AgentAvailability::Primary => "Primary sessions",
                AgentAvailability::Isolated => "Isolated executions",
                AgentAvailability::Both => "Primary and isolated executions",
            }
        ));
    }
    if let Some(addendum) = &document.metadata.addendum {
        out.push_str(&format!("Addendum target: {}\n", addendum.target));
    }
    if !document.metadata.includes.is_empty() {
        out.push_str("Included modules:\n");
        for include in &document.metadata.includes {
            out.push_str(&format!("  {include}\n"));
        }
    }
    if let Some(tools) = &document.metadata.allowed_tools {
        out.push_str(&format!(
            "Declared allowed tools: {}\n",
            if tools.is_empty() {
                "None".into()
            } else {
                tools.join(", ")
            }
        ));
    }
    out
}
pub(super) fn roster_entry(alias: &str, entry: &crate::model_roster::ModelRosterEntry) -> String {
    let mut out = format!(
        "\nMODEL ALIAS: {alias}\n\n{}\n\nCandidate order:\n",
        entry.description
    );
    for (index, model) in entry.models.iter().enumerate() {
        out.push_str(&format!("  {}. {model}\n", index + 1));
    }
    out.push_str(&format!(
        "\nDefault effort: {}\n",
        entry
            .default_effort
            .as_deref()
            .unwrap_or("Provider/model default")
    ));
    if let Some(notes) = &entry.notes {
        out.push_str(&format!(
            "\nHuman-only notes (not model discovery):\n{notes}\n"
        ));
    }
    out
}
pub(super) fn resolution_summary(
    resolution: &crate::model_roster::ModelRosterResolution,
) -> String {
    format!(
        "Model: {}\nProvider: {}\nAuthentication / route: {}\nEffort: {}\nRequested alias: {}\nExplicit model override: {}\nExplicit effort override: {}\n",
        resolution.selected_model(),
        resolution.provider_key(),
        resolution.route_api_method(),
        resolution
            .selected_effort()
            .unwrap_or("Provider default / not applicable"),
        resolution.requested_alias().unwrap_or("Concrete selection"),
        yes(resolution.used_model_override()),
        yes(resolution.used_effort_override())
    )
}
