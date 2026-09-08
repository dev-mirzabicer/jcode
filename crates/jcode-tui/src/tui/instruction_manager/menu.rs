//! Discoverable action/filter menus. The same actions power labels and shortcuts.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FilterField {
    All,
    Kind,
    Scope,
    Validity,
    Effectiveness,
    Origin,
    Redefinitions,
    Repository,
}
#[derive(Clone)]
pub(super) enum MenuAction {
    Navigate(usize),
    Scope(Option<String>),
    Source(InstructionRow, bool),
    CopyScope(InstructionEditScope),
    New(InstructionEditScope),
    Recovery(super::editing::RecoveryChoice),
    Edit(super::editing::EditAction),
    Key(KeyCode),
    Filter(InstructionFilter),
    FilterMenu(FilterField),
}
#[derive(Clone)]
pub(super) struct MenuItem {
    pub label: String,
    pub hint: String,
    pub key: String,
    pub action: MenuAction,
    pub disabled: Option<String>,
}
pub(super) struct Menu {
    pub title: String,
    pub items: Vec<MenuItem>,
    pub query: String,
    pub selected: usize,
    pub context: Option<InstructionInspectionTarget>,
    pub snapshot: Option<String>,
    pub revision: Option<String>,
    pub parent: Option<FilterField>,
    pub explanation: bool,
    pub explanation_scroll: usize,
}
impl Menu {
    pub fn matches(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                format!("{} {}", item.label, item.hint)
                    .to_lowercase()
                    .contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }
}

pub(super) const VIEWS: [(InstructionInspectionView, char, &str, &str); 8] = [
    (
        InstructionInspectionView::Source,
        '1',
        "Source",
        "Read the complete original file, including frontmatter",
    ),
    (
        InstructionInspectionView::Metadata,
        '2',
        "Overview",
        "Identity, source, applicability, validation and consumer contracts",
    ),
    (
        InstructionInspectionView::Rendered,
        '3',
        "Rendered preview",
        "Render current instructions or preview a model alias; does not activate",
    ),
    (
        InstructionInspectionView::System,
        '4',
        "System prompt",
        "Agent: current-source composition. Session: exact stored prompt",
    ),
    (
        InstructionInspectionView::Dependencies,
        '5',
        "Dependencies",
        "Includes, validation-only references and reverse consumers",
    ),
    (
        InstructionInspectionView::History,
        '6',
        "Git history",
        "Choose a revision to read or two revisions to compare",
    ),
    (
        InstructionInspectionView::WorkingDiff,
        '7',
        "Working changes",
        "Compare current files with Git HEAD; nothing is written",
    ),
    (
        InstructionInspectionView::ScopeComparison,
        '8',
        "Compare scopes",
        "Read global and project definitions side by side in one document",
    ),
];

impl InstructionManager {
    pub(super) fn view_label(&self) -> &'static str {
        if self.revision_open {
            return if self
                .revision_selection
                .as_ref()
                .is_some_and(|selection| selection.to.is_some())
            {
                "Revision comparison"
            } else if self.view == InstructionInspectionView::Metadata {
                "Commit details"
            } else {
                "Revision content"
            };
        }
        if self.view == InstructionInspectionView::System
            && self.target == Some(InstructionInspectionTarget::Session)
        {
            return "Stored system prompt";
        }
        if self.view == InstructionInspectionView::System {
            return "System preview (not active)";
        }
        VIEWS
            .iter()
            .find(|(view, _, _, _)| *view == self.view)
            .map_or("Overview", |(_, _, label, _)| label)
    }

    pub(super) fn view_unavailable(&self, view: InstructionInspectionView) -> Option<String> {
        if self.snapshot.is_none() || self.rows_loading() {
            return Some("Wait for source discovery, or cancel and refresh.".into());
        }
        let target = self.selected_target()?;
        let git_only = matches!(
            view,
            InstructionInspectionView::History | InstructionInspectionView::WorkingDiff
        );
        match target {
            InstructionInspectionTarget::Session => (!matches!(
                view,
                InstructionInspectionView::Metadata | InstructionInspectionView::System
            ))
            .then(|| "Session offers Overview and its exact Stored system prompt.".into()),
            InstructionInspectionTarget::Repository(key) => {
                if key == "external" && git_only {
                    Some("These sources do not share a Git repository.".into())
                } else {
                    (!matches!(
                        view,
                        InstructionInspectionView::Metadata
                            | InstructionInspectionView::History
                            | InstructionInspectionView::WorkingDiff
                    ))
                    .then(|| "Repository offers Overview, Git history and Working changes.".into())
                }
            }
            InstructionInspectionTarget::Resource(key) => {
                let Some(row) = self
                    .rows
                    .iter()
                    .find(|row| row.key == key)
                    .or(self.detail_row.as_ref().filter(|row| row.key == key))
                else {
                    return Some("Select a resource from the current catalog.".into());
                };
                if git_only && row.repository == "external" {
                    return Some("This source has no owning Git repository.".into());
                }
                if view == InstructionInspectionView::System && row.kind != "agent" {
                    return Some(
                        "Choose an agent for a full system preview, or Session for stored text."
                            .into(),
                    );
                }
                if matches!(
                    view,
                    InstructionInspectionView::Dependencies
                        | InstructionInspectionView::ScopeComparison
                ) && (row.origin != InstructionOrigin::Managed
                    || matches!(
                        row.kind.as_str(),
                        "model-roster" | "store-settings" | "invalid-resource" | "configuration"
                    ))
                {
                    return Some(
                        "This source has no managed instruction dependency/scope relationship."
                            .into(),
                    );
                }
                if view == InstructionInspectionView::Rendered && !row.valid {
                    return Some("Source validation failed. Overview explains the problem; Source preserves the file.".into());
                }
                None
            }
        }
    }

    pub(super) fn rows_loading(&self) -> bool {
        self.queued
            .as_ref()
            .into_iter()
            .chain(self.pending.as_ref().map(|pending| &pending.request))
            .any(|request| {
                matches!(
                    request,
                    InstructionInspectionRequest::Open { .. }
                        | InstructionInspectionRequest::Resources { .. }
                )
            })
    }

    pub(super) fn open_actions(&mut self, views_only: bool) {
        if self.editing.visible {
            self.open_edit_menu();
            return;
        }
        let mut items = if views_only {
            Vec::new()
        } else {
            self.source_actions()
        };
        if !views_only {
            items.extend([
            MenuItem{label:"Choose instruction type".into(),hint:"Type-first browsing. This does not select a save destination.".into(),key:"F1".into(),action:MenuAction::Key(KeyCode::F(1)),disabled:None},
            MenuItem{label:format!("Scope: {}",self.scope_label()),hint:"Choose Effective here, Global or Project.".into(),key:"S".into(),action:MenuAction::Key(KeyCode::Char('s')),disabled:None},
            MenuItem{label:"Repositories & sync".into(),hint:"Global/project setup, branches and explicit synchronization. Not an editing destination selector.".into(),key:"choose".into(),action:MenuAction::Navigate(navigation::REPOSITORIES),disabled:None},
            MenuItem{label:"Current session instructions".into(),hint:"Inspect the exact active snapshot without changing it.".into(),key:"choose".into(),action:MenuAction::Navigate(navigation::SESSION),disabled:None},
        ]);
        }

        for (view, key, label, hint) in VIEWS {
            items.push(MenuItem {
                label: if view == InstructionInspectionView::System
                    && self.selected_target() == Some(InstructionInspectionTarget::Session)
                {
                    "Stored system prompt".into()
                } else {
                    label.into()
                },
                hint: hint.into(),
                key: key.to_string(),
                action: MenuAction::Key(KeyCode::Char(key)),
                disabled: self
                    .selected_target()
                    .is_none()
                    .then(|| "Select a resource first.".into())
                    .or_else(|| self.view_unavailable(view)),
            });
        }
        if !views_only
            && let Some(InstructionInspectionTarget::Repository(key)) = self.selected_target()
        {
            let mut filter = self.filter.clone();
            filter.repository = Some(key);
            items.push(MenuItem {
                label: "Browse this repository".into(),
                hint: "Filter the resource list to this repository; no source changes".into(),
                key: "choose".into(),
                action: MenuAction::Filter(filter),
                disabled: None,
            });
        }
        if !views_only {
            let mut add = |key, label: &str, hint: &str, disabled| {
                items.push(MenuItem {
                    label: label.into(),
                    hint: hint.into(),
                    key: format_key(key),
                    action: MenuAction::Key(key),
                    disabled,
                })
            };
            if self.history_visible && self.pane == Pane::Detail {
                add(
                    KeyCode::Char('i'),
                    "Commit details",
                    "Read the complete author, date, subject and changed-path list",
                    self.history.is_empty().then(|| "History is empty.".into()),
                );
                add(
                    KeyCode::Enter,
                    "Read revision",
                    "Open the selected commit; Escape returns to this list",
                    self.history.is_empty().then(|| "History is empty.".into()),
                );
                add(
                    KeyCode::Char('a'),
                    "Mark comparison base",
                    "Then choose a second commit and use Compare revisions",
                    self.history.is_empty().then(|| "History is empty.".into()),
                );
                add(
                    KeyCode::Char('b'),
                    "Compare revisions",
                    "Read the diff from the marked base to the selected commit",
                    self.history_base
                        .is_none()
                        .then(|| "Mark a base revision first.".into()),
                );
            }
            add(
                KeyCode::Char('/'),
                "Search resources",
                "Search ID, name, type or scope; Ctrl-U clears search",
                None,
            );
            add(
                KeyCode::Char('f'),
                "Choose filters",
                "See every active filter and choose explicit values",
                None,
            );
            add(
                KeyCode::Char('c'),
                "Clear all filters",
                "Includes search and repository restrictions",
                None,
            );
            add(
                KeyCode::Char('n'),
                "Next page",
                "Continue through complete resources, history or content",
                (!self.can_page(true)).then(|| "Already on the last page.".into()),
            );
            add(
                KeyCode::Char('p'),
                "Previous page",
                "Return to the preceding page",
                (!self.can_page(false)).then(|| "Already on the first page.".into()),
            );
            add(
                KeyCode::Char('z'),
                "Expand / collapse pane",
                "Give the focused pane full width",
                None,
            );
            add(
                KeyCode::Char('r'),
                "Refresh sources",
                "Recapture source state; session instructions stay unchanged",
                None,
            );
            add(
                KeyCode::Char('x'),
                "Cancel loading",
                "Stop inspection work without changing source",
                self.pending
                    .is_none()
                    .then(|| "No request is loading.".into()),
            );
            add(
                KeyCode::F(1),
                "Repositories / session",
                "Inspect source stores or the active session",
                None,
            );
            add(
                KeyCode::F(2),
                "Resource list",
                "Return to the browsable instruction catalog",
                None,
            );
            add(
                KeyCode::F(3),
                "Reading pane",
                "Return to the current captured detail",
                None,
            );
            add(
                KeyCode::Tab,
                "Next pane",
                "Move focus without changing the selected resource",
                None,
            );
            add(
                KeyCode::BackTab,
                "Previous pane",
                "Move focus in reverse order",
                None,
            );
            add(
                KeyCode::Home,
                "Start of page",
                "Move to the first visible row or line",
                None,
            );
            add(
                KeyCode::End,
                "End of page",
                "Move to the last row or line of this page",
                None,
            );
            add(
                KeyCode::Char('g'),
                "Toggle redefinitions",
                "Group project overrides of global definitions",
                None,
            );
            add(
                KeyCode::Esc,
                "Back",
                "Return to history, list or repositories without losing selection",
                None,
            );
            add(
                KeyCode::Char('?'),
                "Keyboard help",
                "All controls, reading semantics and current filters",
                None,
            );
            add(
                KeyCode::Char('q'),
                "Close manager",
                "Return to chat; no source changes",
                None,
            );
        }
        if !views_only {
            items.extend(self.edit_menu_items());
        }
        self.menu = Some(Menu {
            title: if views_only {
                "Choose a view"
            } else {
                "Actions"
            }
            .into(),
            items,
            query: String::new(),
            selected: 0,
            context: self.selected_target(),
            snapshot: self.snapshot_id(),
            revision: self
                .history_visible
                .then(|| {
                    self.history
                        .get(self.history_selected)
                        .map(|entry| entry.commit.clone())
                })
                .flatten(),
            parent: None,
            explanation: false,
            explanation_scroll: 0,
        });
    }

    pub(super) fn filter_summary(&self) -> String {
        let f = &self.filter;
        let mut parts = Vec::new();
        if !f.search.is_empty() {
            parts.push(format!("Search: {}", f.search));
        }
        if let Some(valid) = f.valid {
            parts.push(if valid { "Valid only" } else { "Invalid only" }.into());
        }
        if let Some(effective) = f.effective {
            parts.push(
                if effective {
                    "Effective only"
                } else {
                    "Shadowed only"
                }
                .into(),
            );
        }
        if let Some(origin) = f.origin {
            parts.push(format!("Origin: {origin:?}"));
        }
        if let Some(redefined) = f.redefinitions {
            parts.push(
                if redefined {
                    "Project redefinitions"
                } else {
                    "Not redefinitions"
                }
                .into(),
            );
        }
        if let Some(key) = &f.repository {
            parts.push(format!(
                "Repository: {}",
                self.snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot
                        .repositories
                        .iter()
                        .find(|store| &store.key == key))
                    .map_or(key.as_str(), |store| store.kind.as_str())
            ));
        }
        if parts.is_empty() {
            "All resources · no filters".into()
        } else {
            parts.join(" | ")
        }
    }

    pub(super) fn open_filters(&mut self, field: FilterField) {
        use FilterField::*;
        let mut items = Vec::new();
        let mut choice = |label: &str, filter: InstructionFilter| {
            let current = filter == self.filter;
            items.push(MenuItem {
                label: label.into(),
                hint: if current {
                    "Currently selected".into()
                } else {
                    "Apply this filter; all other filters remain unchanged".into()
                },
                key: if current {
                    "selected".into()
                } else {
                    String::new()
                },
                action: MenuAction::Filter(filter),
                disabled: None,
            });
        };
        match field {
            All => {}
            Kind => {
                for kind in [
                    None,
                    Some("system"),
                    Some("agent"),
                    Some("agent-addendum"),
                    Some("module"),
                    Some("notification"),
                    Some("tool-guidance"),
                    Some("skill"),
                    Some("model-roster"),
                    Some("AGENTS.md"),
                    Some("legacy-prompt"),
                    Some("store-settings"),
                    Some("invalid-resource"),
                    Some("configuration"),
                ] {
                    let mut f = self.filter.clone();
                    f.kind = kind.map(str::to_string);
                    choice(kind.unwrap_or("All types"), f);
                }
            }
            Scope => {
                for scope in [None, Some("global"), Some("project")] {
                    let mut f = self.filter.clone();
                    f.scope = scope.map(str::to_string);
                    choice(scope.unwrap_or("All scopes"), f);
                }
            }
            Validity | Effectiveness | Redefinitions => {
                for value in [None, Some(true), Some(false)] {
                    let mut f = self.filter.clone();
                    let labels = match field {
                        Validity => {
                            f.valid = value;
                            ["Valid and invalid", "Valid only", "Invalid only"]
                        }
                        Effectiveness => {
                            f.effective = value;
                            ["Effective and shadowed", "Effective only", "Shadowed only"]
                        }
                        _ => {
                            f.redefinitions = value;
                            [
                                "All definitions",
                                "Project redefinitions",
                                "Not redefinitions",
                            ]
                        }
                    };
                    choice(
                        labels[match value {
                            None => 0,
                            Some(true) => 1,
                            Some(false) => 2,
                        }],
                        f,
                    );
                }
            }
            Origin => {
                for origin in [
                    None,
                    Some(InstructionOrigin::Managed),
                    Some(InstructionOrigin::Legacy),
                    Some(InstructionOrigin::External),
                ] {
                    let mut f = self.filter.clone();
                    f.origin = origin;
                    choice(
                        match origin {
                            None => "All origins",
                            Some(InstructionOrigin::Managed) => "Managed",
                            Some(InstructionOrigin::Legacy) => "Legacy",
                            Some(InstructionOrigin::External) => "External",
                        },
                        f,
                    );
                }
            }
            Repository => {
                let mut f = self.filter.clone();
                f.repository = None;
                choice("All repositories", f);
                if let Some(snapshot) = &self.snapshot {
                    for repository in &snapshot.repositories {
                        let mut f = self.filter.clone();
                        f.repository = Some(repository.key.clone());
                        choice(&format!("{}: {}", repository.kind, repository.root), f);
                    }
                }
            }
        }
        if field == All {
            for (field, label) in [
                (Kind, "Resource type"),
                (Scope, "Global / project scope"),
                (Validity, "Validation"),
                (Effectiveness, "Effective / shadowed"),
                (Origin, "Managed / legacy / external"),
                (Redefinitions, "Project redefinitions"),
                (Repository, "Repository"),
            ] {
                items.push(MenuItem {
                    label: label.into(),
                    hint: match field {
                        Kind => format!(
                            "Current type: {}",
                            self.filter.kind.as_deref().unwrap_or("All")
                        ),
                        Scope => format!(
                            "Current scope: {}",
                            self.filter.scope.as_deref().unwrap_or("All")
                        ),
                        Validity => format!(
                            "Current validation: {}",
                            match self.filter.valid {
                                None => "All",
                                Some(true) => "Valid only",
                                Some(false) => "Invalid only",
                            }
                        ),
                        Effectiveness => format!(
                            "Current lookup: {}",
                            match self.filter.effective {
                                None => "All",
                                Some(true) => "Effective only",
                                Some(false) => "Shadowed only",
                            }
                        ),
                        Origin => format!(
                            "Current origin: {}",
                            match self.filter.origin {
                                None => "All",
                                Some(InstructionOrigin::Managed) => "Managed",
                                Some(InstructionOrigin::Legacy) => "Legacy",
                                Some(InstructionOrigin::External) => "External",
                            }
                        ),
                        Redefinitions => format!(
                            "Current definitions: {}",
                            match self.filter.redefinitions {
                                None => "All",
                                Some(true) => "Project overrides",
                                Some(false) => "Not overrides",
                            }
                        ),
                        Repository => format!(
                            "Current repository: {}",
                            self.filter.repository.as_deref().unwrap_or("All")
                        ),
                        All => String::new(),
                    },
                    key: "choose".into(),
                    action: MenuAction::FilterMenu(field),
                    disabled: None,
                });
            }
            items.push(MenuItem {
                label: "Clear all filters".into(),
                hint: "Show all resources, including invalid and shadowed".into(),
                key: "C".into(),
                action: MenuAction::Filter(InstructionFilter::default()),
                disabled: None,
            });
        }
        let selected = items
            .iter()
            .position(|item| item.key == "selected")
            .unwrap_or(0);
        self.menu = Some(Menu {
            title: match field {
                All => "Filters",
                Kind => "Resource type",
                Scope => "Source scope",
                Validity => "Validation",
                Effectiveness => "Effective source",
                Origin => "Source origin",
                Redefinitions => "Redefinitions",
                Repository => "Repository",
            }
            .into(),
            items,
            query: String::new(),
            selected,
            context: None,
            snapshot: self.snapshot_id(),
            revision: self
                .history_visible
                .then(|| {
                    self.history
                        .get(self.history_selected)
                        .map(|entry| entry.commit.clone())
                })
                .flatten(),
            parent: (field != All).then_some(All),
            explanation: false,
            explanation_scroll: 0,
        });
    }

    pub(super) fn menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        if menu.explanation {
            match code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Left => {
                    menu.explanation = false;
                    menu.explanation_scroll = 0;
                }
                KeyCode::Down | KeyCode::PageDown => {
                    menu.explanation_scroll = menu.explanation_scroll.saturating_add(1)
                }
                KeyCode::Up | KeyCode::PageUp => {
                    menu.explanation_scroll = menu.explanation_scroll.saturating_sub(1)
                }
                KeyCode::Home => menu.explanation_scroll = 0,
                KeyCode::End => menu.explanation_scroll = usize::MAX,
                _ => {}
            }
            return;
        }
        let count = menu.matches().len();
        match code {
            KeyCode::Char('?') => {
                menu.explanation = true;
                menu.explanation_scroll = 0;
            }
            KeyCode::Esc | KeyCode::Left => {
                let parent = menu.parent;
                self.menu = None;
                if let Some(parent) = parent {
                    self.open_filters(parent);
                }
            }
            KeyCode::Up => menu.selected = menu.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => {
                menu.selected = menu.selected.saturating_add(1).min(count.saturating_sub(1))
            }
            KeyCode::PageDown => {
                menu.selected = menu.selected.saturating_add(8).min(count.saturating_sub(1))
            }
            KeyCode::PageUp | KeyCode::BackTab => menu.selected = menu.selected.saturating_sub(8),
            KeyCode::Home => menu.selected = 0,
            KeyCode::End => menu.selected = count.saturating_sub(1),
            KeyCode::Backspace => {
                menu.query.pop();
                menu.selected = 0;
            }
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                menu.query.clear();
                menu.selected = 0;
            }
            KeyCode::Enter | KeyCode::Right => self.choose_menu(),
            KeyCode::Char(ch)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                menu.query.push(ch);
                menu.selected = 0;
            }
            _ => {}
        }
    }

    pub(super) fn choose_menu(&mut self) {
        let Some(menu) = &self.menu else {
            return;
        };
        let Some(index) = menu.matches().get(menu.selected).copied() else {
            return;
        };
        let item = menu.items[index].clone();
        if let Some(reason) = item.disabled {
            self.status = reason;
            if let Some(menu) = self.menu.as_mut() {
                menu.explanation = true;
                menu.explanation_scroll = 0;
            }
            return;
        }
        if matches!(
            item.action,
            MenuAction::Key(KeyCode::Char('1'..='8' | 'a' | 'b' | 'i'))
                | MenuAction::Edit(_)
                | MenuAction::Source(_, _)
                | MenuAction::CopyScope(_)
                | MenuAction::New(_)
                | MenuAction::Recovery(super::editing::RecoveryChoice::Historical { .. })
        ) && (menu.context != self.selected_target() || menu.snapshot != self.snapshot_id())
        {
            self.menu = None;
            self.status =
                "Source selection changed. Open Actions again for the current selection.".into();
            return;
        }
        if matches!(
            item.action,
            MenuAction::Key(KeyCode::Enter | KeyCode::Char('a' | 'b' | 'i'))
        ) && menu.revision
            != self
                .history_visible
                .then(|| {
                    self.history
                        .get(self.history_selected)
                        .map(|entry| entry.commit.clone())
                })
                .flatten()
        {
            self.menu = None;
            self.status =
                "History selection changed. Reopen Actions for the current revision.".into();
            return;
        }
        self.menu = None;
        match item.action {
            MenuAction::Navigate(index) => self.choose_navigation(index),
            MenuAction::Scope(scope) => self.choose_scope(scope),
            MenuAction::Source(row, edit) => self.choose_source(row, edit),
            MenuAction::CopyScope(scope) => self.copy_scope(scope),
            MenuAction::New(scope) => self.start_creation(scope),
            MenuAction::Recovery(choice) => self.choose_recovery(choice),
            MenuAction::Edit(action) => self.edit_action(action),
            MenuAction::Key(key) => {
                self.key(key, KeyModifiers::NONE);
            }
            MenuAction::Filter(filter) => {
                self.filter = filter;
                self.filter_changed();
            }
            MenuAction::FilterMenu(field) => self.open_filters(field),
        }
    }
}

pub(super) fn format_key(key: KeyCode) -> String {
    match key {
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Char(' ') => "Space".into(),
        KeyCode::Char(ch) => ch.to_ascii_uppercase().to_string(),
        KeyCode::F(n) => format!("F{n}"),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => "Shift-Tab".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        _ => String::new(),
    }
}
