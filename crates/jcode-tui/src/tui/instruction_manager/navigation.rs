//! Type/scope navigation is separate from storage administration and active snapshots.
use super::editing::EditAction;
use super::menu::{MenuAction, MenuItem};
use super::*;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Destination {
    #[default]
    Catalog,
    Repositories,
    Session,
}
pub(super) const CATEGORIES: [(&str, Option<&str>); 11] = [
    ("Agents", Some("agent")),
    ("Skills", Some("skill")),
    ("Project additions", Some("agent-addendum")),
    ("Shared modules", Some("module")),
    ("Notifications", Some("notification")),
    ("System / workflow guidance", Some("system")),
    ("Tool guidance", Some("tool-guidance")),
    ("Model roster", Some("model-roster")),
    ("AGENTS.md guidance", Some("AGENTS.md")),
    ("Imports & original files", None),
    ("All types / search", None),
];
pub(super) const REPOSITORIES: usize = 11;
pub(super) const SESSION: usize = 12;

impl InstructionManager {
    pub(super) fn category_label(&self) -> &str {
        match self.destination {
            Destination::Repositories => "Repositories & sync",
            Destination::Session => "Current session instructions",
            Destination::Catalog => {
                if self.filter.origin == Some(InstructionOrigin::Legacy) {
                    "Imports & original files"
                } else {
                    CATEGORIES
                        .iter()
                        .take(9)
                        .find(|(_, kind)| kind.map(str::to_string) == self.filter.kind)
                        .map_or("All types / search", |(name, _)| *name)
                }
            }
        }
    }
    pub(super) fn scope_label(&self) -> &str {
        match self.filter.scope.as_deref() {
            Some("global") => "Global",
            Some("project") => "Project",
            _ => "Effective here",
        }
    }
    pub(super) fn managed_repositories(&self) -> Vec<&InstructionRepositoryRow> {
        self.snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .repositories
                    .iter()
                    .filter(|repository| {
                        repository.key != "external" && !repository.key.starts_with("external:")
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    pub(super) fn project_configured(&self) -> bool {
        self.managed_repositories()
            .iter()
            .any(|repository| repository.key != "global")
    }
    pub(super) fn choose_navigation(&mut self, index: usize) {
        self.repository_selected = index.min(SESSION);
        self.menu = None;
        self.pending = None;
        self.queued = None;
        self.text = None;
        self.wrapped.clear();
        self.wrap_key = None;
        self.target = None;
        self.detail_row = None;
        self.history_visible = false;
        self.revision_open = false;
        self.history.clear();
        self.scroll = 0;
        self.selected = 0;
        if index == REPOSITORIES {
            self.destination = Destination::Repositories;
            self.pane = Pane::Resources;
            self.preview_selection();
            self.status="Storage administration only. Selecting a repository here never redirects an instruction edit.".into();
        } else if index == SESSION {
            self.destination = Destination::Session;
            self.target = Some(InstructionInspectionTarget::Session);
            self.pane = Pane::Detail;
            self.detail(InstructionInspectionView::Metadata, None);
            self.status =
                "Read the exact active session snapshot. This is not a save destination.".into();
        } else {
            self.destination = Destination::Catalog;
            self.filter.kind = CATEGORIES[index].1.map(str::to_string);
            self.filter.origin = (index == 9).then_some(InstructionOrigin::Legacy);
            self.filter.main_catalog = index != 9;
            self.filter.grouped = true;
            self.filter.repository = None;
            self.filter.search.clear();
            self.search_cursor = 0;
            self.filter.effective = None;
            self.filter.valid = None;
            self.filter.redefinitions = None;
            self.filter_changed();
        }
        if self.snapshot.is_none() && self.queued.is_none() {
            self.queued = Some(InstructionInspectionRequest::Open {
                filter: self.filter.clone(),
            });
        }
    }
    pub(super) fn choose_scope(&mut self, scope: Option<String>) {
        if self.destination != Destination::Catalog {
            self.destination = Destination::Catalog;
            self.repository_selected = 0;
            self.filter.kind = Some("agent".into());
        }
        self.filter.scope = scope;
        self.filter.repository = None;
        self.filter.effective = None;
        self.filter.grouped = true;
        self.filter_changed();
    }
    pub(super) fn open_scope_choices(&mut self) {
        let items=[("Effective here",None,"One named item with all source versions. Applicability follows current files, not the running session snapshot."),("Global",Some("global"),"Browse global definitions. Edit writes their global source only."),("Project",Some("project"),"Browse definitions stored for this project. Missing definitions are not implicitly created.")].into_iter().map(|(label,scope,hint)|MenuItem{label:format!("{}{}",if self.filter.scope.as_deref()==scope{"* "}else{""},label),hint:hint.into(),key:"Enter".into(),action:MenuAction::Scope(scope.map(str::to_string)),disabled:None}).collect();
        self.navigation_menu("Choose scope", items);
    }
    fn navigation_menu(&mut self, title: &str, items: Vec<MenuItem>) {
        self.menu = Some(Menu {
            title: title.into(),
            items,
            query: String::new(),
            selected: 0,
            context: self.selected_target(),
            snapshot: self.snapshot_id(),
            revision: None,
            parent: None,
            explanation: false,
            explanation_scroll: 0,
        });
    }
    pub(super) fn active_row(&self) -> Option<&InstructionRow> {
        if self.destination != Destination::Catalog {
            return None;
        }
        if self.pane == Pane::Detail {
            self.detail_row.as_ref()
        } else if self.pane == Pane::Resources {
            self.rows.get(self.selected)
        } else {
            None
        }
    }
    pub(super) fn source_row(
        row: &InstructionRow,
        variant: &InstructionSourceVariant,
    ) -> InstructionRow {
        let mut result = row.clone();
        result.key = variant.key.clone();
        result.name = variant.name.clone();
        result.scope = variant.scope.clone();
        result.repository = variant.repository.clone();
        result.origin = variant.origin;
        result.effective = variant.effective;
        result.valid = variant.valid;
        result
    }
    pub(super) fn source_actions(&self) -> Vec<MenuItem> {
        let Some(row) = self.active_row() else {
            return Vec::new();
        };
        let mut items = Vec::new();
        for variant in &row.variants {
            let source = Self::source_row(row, variant);
            let source_name = format!(
                "{}{}",
                variant.scope,
                if variant.origin == InstructionOrigin::External {
                    " · external"
                } else {
                    ""
                }
            );
            let can_edit = variant.origin == InstructionOrigin::Managed || row.kind == "AGENTS.md";
            let disabled = self
                .rows_loading()
                .then(|| "Wait for source discovery to finish.".into());
            items.push(MenuItem{label:format!("View {source_name}"),hint:format!("{} · {}. This selects a version for inspection, not an edit destination for other items.",variant.path,if variant.effective{"Applies here"}else{"Shadowed here"}),key:"Enter".into(),action:MenuAction::Source(source.clone(),false),disabled:disabled.clone()});
            if can_edit {
                items.push(MenuItem {
                    label: format!("Edit {source_name}"),
                    hint: format!(
                        "Edit only {}. Current session instructions remain unchanged.",
                        variant.path
                    ),
                    key: "Enter".into(),
                    action: MenuAction::Source(source, true),
                    disabled,
                });
            }
        }
        if row.origin == InstructionOrigin::Managed
            && !matches!(
                row.kind.as_str(),
                "model-roster" | "store-settings" | "invalid-resource"
            )
        {
            let scope = if row.scope == "global" {
                "project"
            } else {
                "global"
            };
            let existing = row.variants.iter().any(|variant| {
                variant.scope == scope && variant.origin == InstructionOrigin::Managed
            });
            items.push(MenuItem{label:format!("Copy to {scope} and edit"),hint:"Create a private draft from this complete source. Save publishes only the destination; references must validate there. No existing version is overwritten.".into(),key:"choose".into(),action:MenuAction::CopyScope(if scope=="global"{InstructionEditScope::Global}else{InstructionEditScope::Project}),disabled:if existing{Some(format!("A {scope} version already exists. Use Edit {scope} or View {scope} to work with it."))}else{self.rows_loading().then(||"Wait for source discovery.".into())}});
        }
        if row.origin == InstructionOrigin::External && row.kind == "skill" {
            for (action, label) in [
                (EditAction::CopyGlobal, "Copy skill to global and edit"),
                (EditAction::CopyProject, "Copy skill to project and edit"),
            ] {
                items.push(MenuItem{label:label.into(),hint:"Copy the complete external package into a managed draft. Original source stays untouched.".into(),key:"choose".into(),action:MenuAction::Edit(action),disabled:self.rows_loading().then(||"Wait for source discovery.".into())});
            }
        }
        items
    }
    pub(super) fn open_sources(&mut self) {
        let items = self.source_actions();
        if items.is_empty() {
            self.status = "Select an instruction to inspect its source versions.".into();
            return;
        }
        self.navigation_menu("Source versions and copying", items);
    }
    pub(super) fn choose_source(&mut self, row: InstructionRow, edit: bool) {
        self.pane = Pane::Detail;
        self.target = Some(InstructionInspectionTarget::Resource(row.key.clone()));
        self.detail_row = Some(row);
        if edit {
            self.edit_action(EditAction::Open);
        } else {
            self.detail(InstructionInspectionView::Metadata, None);
        }
    }
    pub(super) fn copy_scope(&mut self, scope: InstructionEditScope) {
        self.begin_edit(InstructionEditAction::CopyToScope { scope });
    }
    pub(super) fn preview_selection(&mut self) {
        if self.snapshot.is_none()
            || self.search_editing
            || self.destination == Destination::Session
        {
            return;
        }
        let pane = self.pane;
        self.pane = Pane::Resources;
        if self.selected_target().is_some() {
            self.detail(InstructionInspectionView::Metadata, None);
        }
        self.pane = pane;
    }
    pub(super) fn new_instruction(&mut self) {
        if self.destination != Destination::Catalog
            || self.filter.origin == Some(InstructionOrigin::Legacy)
        {
            self.status = "Choose an instruction type before creating one.".into();
            return;
        }
        let items=[(InstructionEditScope::Global,"New global instruction"),(InstructionEditScope::Project,"New project instruction")].into_iter().map(|(scope,label)|MenuItem{label:label.into(),hint:format!("Create a {} draft in the explicitly selected scope. Nothing is saved until review.",self.category_label()),key:"Enter".into(),action:MenuAction::New(scope),disabled:(scope==InstructionEditScope::Project && !self.project_configured()).then(||"Set up project instructions first. Project setup is available from the header and Repositories & sync.".into())}).collect();
        self.navigation_menu("Create in which scope?", items);
    }
    pub(super) fn start_creation(&mut self, scope: InstructionEditScope) {
        if self.filter.kind.as_deref() == Some("model-roster") {
            self.status="Select an alias and Edit global to add or change aliases in the typed roster editor.".into();
            return;
        }
        self.edit_action(if scope == InstructionEditScope::Global {
            EditAction::CreateGlobal
        } else {
            EditAction::CreateProject
        });
        self.editing.set_creation_kind(self.filter.kind.as_deref());
    }
}
