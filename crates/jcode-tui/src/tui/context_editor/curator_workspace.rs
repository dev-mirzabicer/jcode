use super::*;
use crate::protocol::{
    CONTEXT_CURATOR_INSTRUCTION_MAX_CHARS, CONTEXT_CURATOR_TOTAL_INSTRUCTION_MAX_CHARS,
    ContextCuratorRouteOption, ContextCuratorRoutePreview, ContextCuratorSourcePurpose,
    ContextCuratorSourceScope, ContextCuratorTaskPreview,
};
use jcode_session_types::{StoredContextCuratorRole, StoredReasoningSelection};

const WORKSPACE_NARROW_WIDTH: u16 = 92;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CuratorWorkspaceSection {
    Overview,
    Route,
    Instructions,
    ExactCalls,
}

impl CuratorWorkspaceSection {
    const ALL: [Self; 4] = [
        Self::Overview,
        Self::Route,
        Self::Instructions,
        Self::ExactCalls,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Route => "Route & effort",
            Self::Instructions => "Instructions",
            Self::ExactCalls => "Exact calls",
        }
    }

    fn offset(self, delta: isize) -> Self {
        let current = Self::ALL
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0);
        let next = (current as isize + delta).clamp(0, Self::ALL.len().saturating_sub(1) as isize)
            as usize;
        Self::ALL[next]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CuratorWorkspacePane {
    RunSheet,
    Detail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CuratorPlanDetail {
    Overview,
    Prompt,
    Contract,
    SourceScope,
    Integrity,
}

impl CuratorPlanDetail {
    const ALL: [Self; 5] = [
        Self::Overview,
        Self::Prompt,
        Self::Contract,
        Self::SourceScope,
        Self::Integrity,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Prompt => "Exact prompt",
            Self::Contract => "Response contract",
            Self::SourceScope => "Source scope",
            Self::Integrity => "Plan integrity",
        }
    }

    fn tab_label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Prompt => "Prompt",
            Self::Contract => "Contract",
            Self::SourceScope => "Scope",
            Self::Integrity => "Integrity",
        }
    }

    fn offset(self, delta: isize) -> Self {
        let current = Self::ALL
            .iter()
            .position(|detail| *detail == self)
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(Self::ALL.len() as isize) as usize;
        Self::ALL[next]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CuratorInstructionScope {
    Transaction,
    Range,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CuratorGenerationOutcome {
    Failed,
    Canceled,
    Expired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CuratorWorkspaceOverlay {
    SaveDefault,
    LeaveRunning,
    Apply,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CuratorWorkspaceAction {
    Back,
    CustomizeRoute,
    OpenInstructions,
    Validate,
    Generate,
    RestoreDefault,
    SaveDefault,
    BeginInstructionEdit,
    FinishInstructionEdit,
    CancelDraft,
    LeaveRunning,
    Retry,
    EditRun,
    ToggleProposal,
    Apply,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CuratorHitTarget {
    Section(CuratorWorkspaceSection),
    Route(usize),
    Range(usize),
    Task(usize),
    PlanDetail(CuratorPlanDetail),
    ReviewItem(usize),
    InstructionScope(CuratorInstructionScope),
    Action(CuratorWorkspaceAction),
}

#[derive(Clone, Debug, Default)]
pub(super) struct CuratorWorkspaceHitRegions {
    pub(super) run_sheet: Rect,
    pub(super) detail: Rect,
    pub(super) scrollable_detail: Rect,
    pub(super) targets: Vec<(Rect, CuratorHitTarget)>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct CuratorTextEditorState {
    cursor: usize,
    preferred_column: Option<usize>,
    scroll: usize,
}

impl CuratorTextEditorState {
    fn clamp(&mut self, text: &str) {
        self.cursor = self.cursor.min(text.len());
        while self.cursor > 0 && !text.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
    }

    fn move_to_end(&mut self, text: &str) {
        self.cursor = text.len();
        self.preferred_column = None;
    }
}

#[derive(Clone, Debug)]
pub(super) struct CuratorWorkspaceState {
    pub(super) active: bool,
    pub(super) section: CuratorWorkspaceSection,
    pub(super) pane: CuratorWorkspacePane,
    pub(super) narrow_detail_open: bool,
    pub(super) narrow_task_detail_open: bool,
    pub(super) route_filter: String,
    pub(super) route_search_active: bool,
    pub(super) route_cursor: usize,
    pub(super) route_scroll: usize,
    pub(super) range_cursor: usize,
    pub(super) task_cursor: usize,
    pub(super) review_cursor: usize,
    pub(super) plan_detail: CuratorPlanDetail,
    pub(super) detail_scroll: usize,
    pub(super) instruction_scope: CuratorInstructionScope,
    pub(super) instruction_editing: bool,
    pub(super) instruction_editor: CuratorTextEditorState,
    pub(super) plan_dirty_reason: Option<String>,
    pub(super) feedback: Option<String>,
    pub(super) generation_outcome: Option<CuratorGenerationOutcome>,
    overlay: Option<CuratorWorkspaceOverlay>,
    pub(super) hit_regions: CuratorWorkspaceHitRegions,
    last_narrow: bool,
}

impl Default for CuratorWorkspaceState {
    fn default() -> Self {
        Self {
            active: false,
            section: CuratorWorkspaceSection::Overview,
            pane: CuratorWorkspacePane::RunSheet,
            narrow_detail_open: false,
            narrow_task_detail_open: false,
            route_filter: String::new(),
            route_search_active: false,
            route_cursor: 0,
            route_scroll: 0,
            range_cursor: 0,
            task_cursor: 0,
            review_cursor: 0,
            plan_detail: CuratorPlanDetail::Overview,
            detail_scroll: 0,
            instruction_scope: CuratorInstructionScope::Transaction,
            instruction_editing: false,
            instruction_editor: CuratorTextEditorState::default(),
            plan_dirty_reason: None,
            feedback: None,
            generation_outcome: None,
            overlay: None,
            hit_regions: CuratorWorkspaceHitRegions::default(),
            last_narrow: false,
        }
    }
}

impl CuratorWorkspaceState {
    pub(super) fn reset_for_session(&mut self) {
        *self = Self::default();
    }

    pub(super) fn mark_plan_dirty(&mut self, reason: impl Into<String>) {
        self.plan_dirty_reason = Some(reason.into());
        self.feedback = None;
        self.task_cursor = 0;
        self.detail_scroll = 0;
        self.plan_detail = CuratorPlanDetail::Overview;
    }

    pub(super) fn mark_plan_current(&mut self, task_count: usize) {
        self.plan_dirty_reason = None;
        self.generation_outcome = None;
        self.task_cursor = self.task_cursor.min(task_count.saturating_sub(1));
        self.section = CuratorWorkspaceSection::ExactCalls;
        self.detail_scroll = 0;
    }

    pub(super) fn mark_generation_outcome(&mut self, outcome: CuratorGenerationOutcome) {
        self.generation_outcome = Some(outcome);
        self.active = true;
        self.overlay = None;
        self.detail_scroll = 0;
        self.narrow_detail_open = false;
        self.narrow_task_detail_open = false;
    }

    pub(super) fn clear_generation_outcome(&mut self) {
        self.generation_outcome = None;
    }
}

impl ContextEditor {
    pub(super) fn open_curator_workspace(&mut self, section: CuratorWorkspaceSection) {
        self.curator_workspace.active = true;
        self.curator_workspace.section = section;
        self.curator_workspace.pane = CuratorWorkspacePane::RunSheet;
        self.curator_workspace.narrow_detail_open = false;
        self.curator_workspace.narrow_task_detail_open = false;
        self.curator_workspace.overlay = None;
        self.curator_workspace.detail_scroll = 0;
        self.modal = None;
        self.error = None;
        if section == CuratorWorkspaceSection::Route {
            self.align_curator_route_cursor_to_effective_selection();
        }
    }

    pub(super) fn curator_workspace_active(&self) -> bool {
        self.curator_workspace.active
    }

    pub(super) fn curator_workspace_plan_accepted(&mut self, task_count: usize) {
        self.curator_workspace.mark_plan_current(task_count);
        self.curator_workspace.active = true;
        self.curator_workspace.feedback = Some(format!(
            "Verified {task_count} isolated curator call{} without invoking a model.",
            if task_count == 1 { "" } else { "s" }
        ));
    }

    pub(super) fn curator_workspace_plan_rejected(&mut self) {
        self.curator_workspace.active = true;
        self.curator_workspace.section = CuratorWorkspaceSection::ExactCalls;
        self.curator_workspace.plan_dirty_reason =
            Some("Exact validation failed. Adjust the run or retry.".to_string());
        self.curator_workspace.feedback = None;
        self.curator_workspace.overlay = None;
    }

    pub(super) fn curator_workspace_default_saved(&mut self) {
        self.curator_workspace.active = true;
        self.curator_workspace.section = CuratorWorkspaceSection::Route;
        self.align_curator_route_cursor_to_effective_selection();
        self.curator_workspace.feedback = Some(
            "Saved provider, route, model, and effort as the durable default. Instructions remain temporary."
                .to_string(),
        );
    }

    pub(super) fn curator_workspace_draft_ready(&mut self) {
        self.curator_workspace.active = true;
        self.curator_workspace.generation_outcome = None;
        self.curator_workspace.review_cursor = 0;
        self.curator_workspace.detail_scroll = 0;
        self.curator_workspace.narrow_detail_open = false;
        self.curator_workspace.narrow_task_detail_open = false;
        self.curator_workspace.overlay = None;
        self.curator_workspace.feedback = Some(
            "Generation complete. Review every operation, proposal, usage record, and provenance before one atomic apply."
                .to_string(),
        );
    }

    pub(super) fn handle_curator_workspace_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> (bool, Option<ContextEditorAction>) {
        if let Some(overlay) = self.curator_workspace.overlay {
            return self.handle_curator_workspace_overlay_key(overlay, code);
        }

        if self.phase == ContextEditorPhase::PreparingDraft {
            return self.handle_curator_progress_key(code);
        }
        if self.phase == ContextEditorPhase::ReviewDraft {
            return self.handle_curator_review_key(code);
        }
        if self.curator_workspace.route_search_active {
            return self.handle_curator_route_search_key(code, modifiers);
        }
        if self.curator_workspace.instruction_editing {
            return self.handle_curator_instruction_edit_key(code, modifiers);
        }
        if self.curator_workspace.generation_outcome.is_some() {
            match code {
                KeyCode::Char('e') => {
                    return (
                        false,
                        self.activate_curator_workspace_action(CuratorWorkspaceAction::EditRun),
                    );
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.curator_workspace.detail_scroll =
                        self.curator_workspace.detail_scroll.saturating_sub(1);
                    return (false, None);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.curator_workspace.detail_scroll =
                        self.curator_workspace.detail_scroll.saturating_add(1);
                    return (false, None);
                }
                KeyCode::PageUp => {
                    self.curator_workspace.detail_scroll =
                        self.curator_workspace.detail_scroll.saturating_sub(10);
                    return (false, None);
                }
                KeyCode::PageDown => {
                    self.curator_workspace.detail_scroll =
                        self.curator_workspace.detail_scroll.saturating_add(10);
                    return (false, None);
                }
                _ => {}
            }
        }

        if code == KeyCode::Char('?') {
            self.curator_workspace.overlay = Some(CuratorWorkspaceOverlay::Help);
            return (false, None);
        }
        if code == KeyCode::Char('g') {
            return self.curator_workspace_primary_action();
        }
        if code == KeyCode::Esc {
            if self.curator_workspace.last_narrow && self.curator_workspace.narrow_task_detail_open
            {
                self.curator_workspace.narrow_task_detail_open = false;
                self.curator_workspace.detail_scroll = 0;
                return (false, None);
            }
            if self.curator_workspace.last_narrow && self.curator_workspace.narrow_detail_open {
                self.curator_workspace.narrow_detail_open = false;
                self.curator_workspace.detail_scroll = 0;
                return (false, None);
            }
            self.curator_workspace.active = false;
            self.curator_workspace.feedback = None;
            return (false, None);
        }
        if code == KeyCode::Tab && !self.curator_workspace.last_narrow {
            self.curator_workspace.pane = match self.curator_workspace.pane {
                CuratorWorkspacePane::RunSheet => CuratorWorkspacePane::Detail,
                CuratorWorkspacePane::Detail => CuratorWorkspacePane::RunSheet,
            };
            return (false, None);
        }
        if code == KeyCode::BackTab && !self.curator_workspace.last_narrow {
            self.curator_workspace.pane = match self.curator_workspace.pane {
                CuratorWorkspacePane::RunSheet => CuratorWorkspacePane::Detail,
                CuratorWorkspacePane::Detail => CuratorWorkspacePane::RunSheet,
            };
            return (false, None);
        }

        if self.curator_workspace.last_narrow && !self.curator_workspace.narrow_detail_open {
            return self.handle_curator_run_sheet_key(code);
        }
        if !self.curator_workspace.last_narrow
            && self.curator_workspace.pane == CuratorWorkspacePane::RunSheet
        {
            return self.handle_curator_run_sheet_key(code);
        }
        self.handle_curator_section_key(code, modifiers)
    }

    fn handle_curator_workspace_overlay_key(
        &mut self,
        overlay: CuratorWorkspaceOverlay,
        code: KeyCode,
    ) -> (bool, Option<ContextEditorAction>) {
        if code == KeyCode::Esc || code == KeyCode::Char('n') {
            self.curator_workspace.overlay = None;
            return (false, None);
        }
        if !matches!(code, KeyCode::Enter | KeyCode::Char('y')) {
            return (false, None);
        }
        self.curator_workspace.overlay = None;
        match overlay {
            CuratorWorkspaceOverlay::SaveDefault => {
                let Some(selection) = self.effective_curator_selection() else {
                    self.error = Some(
                        "No resolved curator route is available to save as the durable default."
                            .to_string(),
                    );
                    return (false, None);
                };
                (
                    false,
                    Some(ContextEditorAction::SaveCuratorDefault(selection)),
                )
            }
            CuratorWorkspaceOverlay::LeaveRunning => (true, None),
            CuratorWorkspaceOverlay::Apply => {
                if let Some(reason) = self.apply_disabled_reason() {
                    self.error = Some(format!(
                        "Cannot apply the context transaction because {reason}."
                    ));
                    return (false, None);
                }
                let action = self
                    .draft
                    .as_ref()
                    .map(|draft| ContextEditorAction::ApplyDraft {
                        draft_id: draft.identity.draft_id.clone(),
                        selected_distillation_ids: self
                            .selected_distillation_ids
                            .iter()
                            .cloned()
                            .collect(),
                    });
                (false, action)
            }
            CuratorWorkspaceOverlay::Help => (false, None),
        }
    }

    fn handle_curator_progress_key(
        &mut self,
        code: KeyCode,
    ) -> (bool, Option<ContextEditorAction>) {
        match code {
            KeyCode::Char('c') => {
                let action = self
                    .draft_id
                    .clone()
                    .map(|draft_id| ContextEditorAction::CancelDraft { draft_id });
                (false, action)
            }
            KeyCode::Esc => {
                self.curator_workspace.overlay = Some(CuratorWorkspaceOverlay::LeaveRunning);
                (false, None)
            }
            KeyCode::PageUp | KeyCode::Up | KeyCode::Char('k') => {
                self.curator_workspace.detail_scroll =
                    self.curator_workspace.detail_scroll.saturating_sub(1);
                (false, None)
            }
            KeyCode::PageDown | KeyCode::Down | KeyCode::Char('j') => {
                self.curator_workspace.detail_scroll =
                    self.curator_workspace.detail_scroll.saturating_add(1);
                (false, None)
            }
            _ => (false, None),
        }
    }

    fn handle_curator_review_key(&mut self, code: KeyCode) -> (bool, Option<ContextEditorAction>) {
        let item_count = self.curator_review_item_count();
        if self.curator_workspace.last_narrow {
            if self.curator_workspace.narrow_detail_open {
                match code {
                    KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                        self.curator_workspace.narrow_detail_open = false;
                        self.curator_workspace.detail_scroll = 0;
                        return (false, None);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.curator_workspace.detail_scroll =
                            self.curator_workspace.detail_scroll.saturating_sub(1);
                        return (false, None);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.curator_workspace.detail_scroll =
                            self.curator_workspace.detail_scroll.saturating_add(1);
                        return (false, None);
                    }
                    KeyCode::Enter => return (false, None),
                    _ => {}
                }
            } else {
                match code {
                    KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                        self.curator_workspace.narrow_detail_open = true;
                        self.curator_workspace.detail_scroll = 0;
                        return (false, None);
                    }
                    KeyCode::Esc => {
                        self.curator_workspace.active = false;
                        return (false, None);
                    }
                    _ => {}
                }
            }
        }
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.curator_workspace.review_cursor =
                    self.curator_workspace.review_cursor.saturating_sub(1);
                self.curator_workspace.detail_scroll = 0;
                (false, None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.curator_workspace.review_cursor = self
                    .curator_workspace
                    .review_cursor
                    .saturating_add(1)
                    .min(item_count.saturating_sub(1));
                self.curator_workspace.detail_scroll = 0;
                (false, None)
            }
            KeyCode::PageUp => {
                self.curator_workspace.detail_scroll =
                    self.curator_workspace.detail_scroll.saturating_sub(10);
                (false, None)
            }
            KeyCode::PageDown => {
                self.curator_workspace.detail_scroll =
                    self.curator_workspace.detail_scroll.saturating_add(10);
                (false, None)
            }
            KeyCode::Char(' ') => self.toggle_selected_review_proposal(),
            KeyCode::Char('a') | KeyCode::Enter => {
                if self.apply_disabled_reason().is_none() {
                    self.curator_workspace.overlay = Some(CuratorWorkspaceOverlay::Apply);
                }
                (false, None)
            }
            KeyCode::Char('e') => {
                self.phase = ContextEditorPhase::Editing;
                self.draft = None;
                self.selection_preview = None;
                self.selection_preview_pending = false;
                self.curator_workspace.section = CuratorWorkspaceSection::Overview;
                self.curator_workspace.review_cursor = 0;
                self.curator_workspace.feedback = Some(
                    "Returned to the run configuration. Generated artifacts are not applicable."
                        .to_string(),
                );
                (false, None)
            }
            _ => (false, None),
        }
    }

    fn handle_curator_run_sheet_key(
        &mut self,
        code: KeyCode,
    ) -> (bool, Option<ContextEditorAction>) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.curator_workspace.section = self.curator_workspace.section.offset(-1);
                self.curator_workspace.detail_scroll = 0;
                (false, None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.curator_workspace.section = self.curator_workspace.section.offset(1);
                self.curator_workspace.detail_scroll = 0;
                (false, None)
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if self.curator_workspace.section == CuratorWorkspaceSection::Route {
                    self.align_curator_route_cursor_to_effective_selection();
                }
                if self.curator_workspace.last_narrow {
                    self.curator_workspace.narrow_detail_open = true;
                } else {
                    self.curator_workspace.pane = CuratorWorkspacePane::Detail;
                }
                self.curator_workspace.detail_scroll = 0;
                (false, None)
            }
            _ => (false, None),
        }
    }

    fn handle_curator_section_key(
        &mut self,
        code: KeyCode,
        _modifiers: KeyModifiers,
    ) -> (bool, Option<ContextEditorAction>) {
        match self.curator_workspace.section {
            CuratorWorkspaceSection::Overview => match code {
                KeyCode::Char('r') => {
                    self.curator_workspace.section = CuratorWorkspaceSection::Route;
                    self.curator_workspace.detail_scroll = 0;
                    self.align_curator_route_cursor_to_effective_selection();
                    (false, None)
                }
                KeyCode::Char('i') => {
                    self.curator_workspace.section = CuratorWorkspaceSection::Instructions;
                    self.curator_workspace.detail_scroll = 0;
                    (false, None)
                }
                KeyCode::Char('p') => {
                    self.curator_workspace.section = CuratorWorkspaceSection::ExactCalls;
                    self.curator_workspace.detail_scroll = 0;
                    (false, None)
                }
                _ => (false, None),
            },
            CuratorWorkspaceSection::Route => self.handle_curator_route_key(code),
            CuratorWorkspaceSection::Instructions => self.handle_curator_instructions_key(code),
            CuratorWorkspaceSection::ExactCalls => self.handle_curator_exact_calls_key(code),
        }
    }

    fn handle_curator_route_key(&mut self, code: KeyCode) -> (bool, Option<ContextEditorAction>) {
        let filtered = self.filtered_curator_route_indices();
        match code {
            KeyCode::Char('/') => {
                self.curator_workspace.route_search_active = true;
                (false, None)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.curator_workspace.route_cursor =
                    self.curator_workspace.route_cursor.saturating_sub(1);
                self.ensure_route_cursor_visible(filtered.len());
                (false, None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.curator_workspace.route_cursor = self
                    .curator_workspace
                    .route_cursor
                    .saturating_add(1)
                    .min(filtered.len().saturating_sub(1));
                self.ensure_route_cursor_visible(filtered.len());
                (false, None)
            }
            KeyCode::Enter => {
                let Some(option_index) = filtered.get(self.curator_workspace.route_cursor).copied()
                else {
                    return (false, None);
                };
                self.select_curator_route_option(option_index);
                (false, None)
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.cycle_workspace_curator_effort(-1);
                (false, None)
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.cycle_workspace_curator_effort(1);
                (false, None)
            }
            KeyCode::Char('d') => {
                if self.curator_selection.is_some() {
                    self.restore_curator_default();
                    self.align_curator_route_cursor_to_effective_selection();
                }
                (false, None)
            }
            KeyCode::Char('s') => {
                if self.can_save_curator_default() {
                    self.curator_workspace.overlay = Some(CuratorWorkspaceOverlay::SaveDefault);
                }
                (false, None)
            }
            KeyCode::PageUp => {
                self.curator_workspace.route_scroll =
                    self.curator_workspace.route_scroll.saturating_sub(10);
                (false, None)
            }
            KeyCode::PageDown => {
                self.curator_workspace.route_scroll =
                    self.curator_workspace.route_scroll.saturating_add(10);
                (false, None)
            }
            _ => (false, None),
        }
    }

    fn handle_curator_route_search_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> (bool, Option<ContextEditorAction>) {
        let control = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Esc | KeyCode::Enter => {
                self.curator_workspace.route_search_active = false;
            }
            KeyCode::Backspace => {
                self.curator_workspace.route_filter.pop();
                self.curator_workspace.route_cursor = 0;
                self.curator_workspace.route_scroll = 0;
            }
            KeyCode::Char('u') if control => {
                self.curator_workspace.route_filter.clear();
                self.curator_workspace.route_cursor = 0;
                self.curator_workspace.route_scroll = 0;
            }
            KeyCode::Char(character) if !control => {
                self.curator_workspace.route_filter.push(character);
                self.curator_workspace.route_cursor = 0;
                self.curator_workspace.route_scroll = 0;
            }
            KeyCode::Up => {
                self.curator_workspace.route_cursor =
                    self.curator_workspace.route_cursor.saturating_sub(1);
            }
            KeyCode::Down => {
                let count = self.filtered_curator_route_indices().len();
                self.curator_workspace.route_cursor = self
                    .curator_workspace
                    .route_cursor
                    .saturating_add(1)
                    .min(count.saturating_sub(1));
            }
            _ => {}
        }
        (false, None)
    }

    fn handle_curator_instructions_key(
        &mut self,
        code: KeyCode,
    ) -> (bool, Option<ContextEditorAction>) {
        match code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.curator_workspace.instruction_scope = CuratorInstructionScope::Transaction;
                self.prepare_instruction_editor_cursor();
                (false, None)
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.curator_workspace.instruction_scope = CuratorInstructionScope::Range;
                self.prepare_instruction_editor_cursor();
                (false, None)
            }
            KeyCode::Char('[') if !self.staged_ranges.is_empty() => {
                self.curator_workspace.range_cursor =
                    self.curator_workspace.range_cursor.saturating_sub(1);
                self.prepare_instruction_editor_cursor();
                (false, None)
            }
            KeyCode::Char(']') if !self.staged_ranges.is_empty() => {
                self.curator_workspace.range_cursor = self
                    .curator_workspace
                    .range_cursor
                    .saturating_add(1)
                    .min(self.staged_ranges.len().saturating_sub(1));
                self.prepare_instruction_editor_cursor();
                (false, None)
            }
            KeyCode::Enter | KeyCode::Char('e') => {
                if self.curator_workspace.instruction_scope == CuratorInstructionScope::Range
                    && self.staged_ranges.is_empty()
                {
                    self.error = Some(
                        "Stage at least one summary range before adding range-specific instructions."
                            .to_string(),
                    );
                } else {
                    self.curator_workspace.instruction_editing = true;
                    self.prepare_instruction_editor_cursor();
                }
                (false, None)
            }
            KeyCode::PageUp => {
                self.curator_workspace.instruction_editor.scroll = self
                    .curator_workspace
                    .instruction_editor
                    .scroll
                    .saturating_sub(5);
                (false, None)
            }
            KeyCode::PageDown => {
                self.curator_workspace.instruction_editor.scroll = self
                    .curator_workspace
                    .instruction_editor
                    .scroll
                    .saturating_add(5);
                (false, None)
            }
            _ => (false, None),
        }
    }

    fn handle_curator_instruction_edit_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> (bool, Option<ContextEditorAction>) {
        if code == KeyCode::Esc {
            self.curator_workspace.instruction_editing = false;
            self.curator_workspace.instruction_editor.preferred_column = None;
            return (false, None);
        }
        let current_chars = match self.curator_workspace.instruction_scope {
            CuratorInstructionScope::Transaction => {
                self.curator_transaction_instructions.chars().count()
            }
            CuratorInstructionScope::Range => self
                .staged_ranges
                .get(self.curator_workspace.range_cursor)
                .map(|range| canonical_editor_range_key(&range.requested))
                .and_then(|range| self.curator_range_instructions.get(&range))
                .map_or(0, |text| text.chars().count()),
        };
        let other_chars = self
            .total_curator_instruction_chars()
            .saturating_sub(current_chars);
        let effective_limit = CONTEXT_CURATOR_INSTRUCTION_MAX_CHARS
            .min(CONTEXT_CURATOR_TOTAL_INSTRUCTION_MAX_CHARS.saturating_sub(other_chars));
        let changed = match self.curator_workspace.instruction_scope {
            CuratorInstructionScope::Transaction => edit_curator_text(
                &mut self.curator_transaction_instructions,
                &mut self.curator_workspace.instruction_editor,
                code,
                modifiers,
                effective_limit,
            ),
            CuratorInstructionScope::Range => {
                let Some(range) = self
                    .staged_ranges
                    .get(self.curator_workspace.range_cursor)
                    .map(|range| canonical_editor_range_key(&range.requested))
                else {
                    self.curator_workspace.instruction_editing = false;
                    return (false, None);
                };
                let text = self.curator_range_instructions.entry(range).or_default();
                edit_curator_text(
                    text,
                    &mut self.curator_workspace.instruction_editor,
                    code,
                    modifiers,
                    effective_limit,
                )
            }
        };
        if changed {
            self.invalidate_curator_plan_with_reason("Instructions changed");
        }
        (false, None)
    }

    fn handle_curator_exact_calls_key(
        &mut self,
        code: KeyCode,
    ) -> (bool, Option<ContextEditorAction>) {
        let task_count = self
            .curator_plan
            .as_ref()
            .map(|plan| plan.tasks.len())
            .unwrap_or(0);
        if self.curator_workspace.last_narrow
            && !self.curator_workspace.narrow_task_detail_open
            && code == KeyCode::Enter
            && task_count > 0
        {
            self.curator_workspace.narrow_task_detail_open = true;
            self.curator_workspace.detail_scroll = 0;
            return (false, None);
        }
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.curator_workspace.task_cursor =
                    self.curator_workspace.task_cursor.saturating_sub(1);
                self.curator_workspace.detail_scroll = 0;
                (false, None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.curator_workspace.task_cursor = self
                    .curator_workspace
                    .task_cursor
                    .saturating_add(1)
                    .min(task_count.saturating_sub(1));
                self.curator_workspace.detail_scroll = 0;
                (false, None)
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.curator_workspace.plan_detail = self.curator_workspace.plan_detail.offset(-1);
                self.curator_workspace.detail_scroll = 0;
                (false, None)
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.curator_workspace.plan_detail = self.curator_workspace.plan_detail.offset(1);
                self.curator_workspace.detail_scroll = 0;
                (false, None)
            }
            KeyCode::PageUp => {
                self.curator_workspace.detail_scroll =
                    self.curator_workspace.detail_scroll.saturating_sub(10);
                (false, None)
            }
            KeyCode::PageDown => {
                self.curator_workspace.detail_scroll =
                    self.curator_workspace.detail_scroll.saturating_add(10);
                (false, None)
            }
            KeyCode::Char('p') => (false, self.preview_curator_plan_action()),
            _ => (false, None),
        }
    }

    fn curator_workspace_primary_action(&mut self) -> (bool, Option<ContextEditorAction>) {
        if self.curator_workspace.generation_outcome.is_some() {
            self.curator_workspace.clear_generation_outcome();
            return self.prepare_action();
        }
        if self.curator_plan_pending {
            self.curator_workspace.feedback =
                Some("Exact no-model validation is already in progress.".to_string());
            return (false, None);
        }
        let request = self.current_draft_request();
        let requires_curator =
            !request.summary_ranges.is_empty() || !request.tool_results.is_empty();
        if requires_curator
            && (self.curator_plan.as_ref().is_none()
                || self.curator_plan_request.as_ref() != Some(&request))
        {
            return (false, self.preview_curator_plan_action());
        }
        self.prepare_action()
    }

    fn filtered_curator_route_indices(&self) -> Vec<usize> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Vec::new();
        };
        let query = self
            .curator_workspace
            .route_filter
            .trim()
            .to_ascii_lowercase();
        snapshot
            .curator_route_options
            .iter()
            .enumerate()
            .filter_map(|(index, option)| {
                let matches = query.is_empty()
                    || option.provider.to_ascii_lowercase().contains(&query)
                    || option.route.to_ascii_lowercase().contains(&query)
                    || option.model.to_ascii_lowercase().contains(&query)
                    || option.detail.to_ascii_lowercase().contains(&query);
                matches.then_some(index)
            })
            .collect()
    }

    fn ensure_route_cursor_visible(&mut self, count: usize) {
        self.curator_workspace.route_cursor = self
            .curator_workspace
            .route_cursor
            .min(count.saturating_sub(1));
        if self.curator_workspace.route_cursor < self.curator_workspace.route_scroll {
            self.curator_workspace.route_scroll = self.curator_workspace.route_cursor;
        }
    }

    fn select_curator_route_option(&mut self, option_index: usize) {
        let Some(option) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.curator_route_options.get(option_index))
            .cloned()
        else {
            return;
        };
        self.curator_selection = Some(ContextCuratorSelection {
            provider: Some(option.provider),
            route: Some(option.route),
            model: Some(option.model),
            effort: None,
        });
        self.invalidate_curator_plan_with_reason("Temporary route selection changed");
        self.curator_workspace.feedback = Some(
            "Selected a temporary route for this run. Nothing was saved as a default.".to_string(),
        );
        self.error = None;
    }

    fn selected_curator_route_option(&self) -> Option<&ContextCuratorRouteOption> {
        let selection = self.curator_selection.as_ref()?;
        let snapshot = self.snapshot.as_ref()?;
        snapshot.curator_route_options.iter().find(|option| {
            selection.provider.as_deref() == Some(option.provider.as_str())
                && selection.route.as_deref() == Some(option.route.as_str())
                && selection.model.as_deref() == Some(option.model.as_str())
        })
    }

    fn cycle_workspace_curator_effort(&mut self, direction: isize) {
        let Some(option) = self.selected_curator_route_option().cloned() else {
            self.error =
                Some("Choose a temporary route before overriding effort for this run.".to_string());
            return;
        };
        let Some(selection) = self.curator_selection.as_mut() else {
            return;
        };
        let mut values = Vec::with_capacity(option.efforts.len() + 1);
        values.push(None);
        values.extend(option.efforts.into_iter().map(Some));
        let current = values
            .iter()
            .position(|effort| effort == &selection.effort)
            .unwrap_or(0);
        let next = (current as isize + direction).rem_euclid(values.len() as isize) as usize;
        selection.effort = values[next].clone();
        self.invalidate_curator_plan_with_reason("Temporary effort selection changed");
        self.error = None;
    }

    fn restore_curator_default(&mut self) {
        if self.curator_selection.take().is_some() {
            self.invalidate_curator_plan_with_reason("Restored the durable curator default");
            self.curator_workspace.feedback = Some(
                "Restored the durable default for this run. Temporary instructions were preserved."
                    .to_string(),
            );
        } else {
            self.curator_workspace.feedback =
                Some("The durable curator default is already active.".to_string());
        }
        self.error = None;
    }

    pub(super) fn prepare_instruction_editor_cursor(&mut self) {
        let text = match self.curator_workspace.instruction_scope {
            CuratorInstructionScope::Transaction => self.curator_transaction_instructions.as_str(),
            CuratorInstructionScope::Range => self
                .staged_ranges
                .get(self.curator_workspace.range_cursor)
                .and_then(|range| {
                    self.curator_range_instructions
                        .get(&canonical_editor_range_key(&range.requested))
                })
                .map(String::as_str)
                .unwrap_or(""),
        };
        self.curator_workspace.instruction_editor.move_to_end(text);
    }

    fn invalidate_curator_plan_with_reason(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.curator_plan = None;
        self.curator_plan_request = None;
        self.curator_plan_pending = false;
        self.curator_workspace.detail_scroll = 0;
        self.curator_workspace.mark_plan_dirty(reason);
    }

    fn curator_review_item_count(&self) -> usize {
        self.draft.as_ref().map_or(0, |draft| {
            1 + draft.required_operations.len()
                + draft.distillation_proposals.len()
                + draft.ineligible_distillations.len()
                + draft.curator_usage.len()
        })
    }

    fn selected_review_proposal_index(&self) -> Option<usize> {
        let draft = self.draft.as_ref()?;
        let mut cursor = self.curator_workspace.review_cursor;
        if cursor == 0 {
            return None;
        }
        cursor -= 1;
        if cursor < draft.required_operations.len() {
            return None;
        }
        cursor -= draft.required_operations.len();
        (cursor < draft.distillation_proposals.len()).then_some(cursor)
    }

    fn toggle_selected_review_proposal(&mut self) -> (bool, Option<ContextEditorAction>) {
        let Some(index) = self.selected_review_proposal_index() else {
            return (false, None);
        };
        self.proposal_cursor = index;
        let Some(draft) = self.draft.as_ref() else {
            return (false, None);
        };
        let Some(proposal) = draft.distillation_proposals.get(index) else {
            return (false, None);
        };
        if !self.selected_distillation_ids.remove(&proposal.proposal_id) {
            self.selected_distillation_ids
                .insert(proposal.proposal_id.clone());
        }
        self.selection_preview_pending = true;
        self.selection_preview = None;
        (
            false,
            Some(ContextEditorAction::PreviewDraftSelection {
                draft_id: draft.identity.draft_id.clone(),
                selected_distillation_ids: self.selected_distillation_ids.iter().cloned().collect(),
            }),
        )
    }

    pub(super) fn handle_curator_workspace_mouse(
        &mut self,
        mouse: MouseEvent,
    ) -> Option<ContextEditorAction> {
        let position = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if self.curator_workspace.hit_regions.detail.contains(position) {
                    self.curator_workspace.detail_scroll =
                        self.curator_workspace.detail_scroll.saturating_sub(3);
                } else if self.curator_workspace.section == CuratorWorkspaceSection::Route {
                    self.curator_workspace.route_scroll =
                        self.curator_workspace.route_scroll.saturating_sub(3);
                }
            }
            MouseEventKind::ScrollDown => {
                if self.curator_workspace.hit_regions.detail.contains(position) {
                    self.curator_workspace.detail_scroll =
                        self.curator_workspace.detail_scroll.saturating_add(3);
                } else if self.curator_workspace.section == CuratorWorkspaceSection::Route {
                    self.curator_workspace.route_scroll =
                        self.curator_workspace.route_scroll.saturating_add(3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let target = self
                    .curator_workspace
                    .hit_regions
                    .targets
                    .iter()
                    .find_map(|(area, target)| area.contains(position).then(|| target.clone()));
                if let Some(target) = target {
                    return self.activate_curator_hit_target(target);
                }
                if self
                    .curator_workspace
                    .hit_regions
                    .run_sheet
                    .contains(position)
                {
                    self.curator_workspace.pane = CuratorWorkspacePane::RunSheet;
                } else if self.curator_workspace.hit_regions.detail.contains(position) {
                    self.curator_workspace.pane = CuratorWorkspacePane::Detail;
                }
            }
            _ => {}
        }
        None
    }

    fn activate_curator_hit_target(
        &mut self,
        target: CuratorHitTarget,
    ) -> Option<ContextEditorAction> {
        match target {
            CuratorHitTarget::Section(section) => {
                self.curator_workspace.section = section;
                self.curator_workspace.pane = CuratorWorkspacePane::Detail;
                self.curator_workspace.narrow_detail_open = self.curator_workspace.last_narrow;
                self.curator_workspace.detail_scroll = 0;
            }
            CuratorHitTarget::Route(index) => self.select_curator_route_option(index),
            CuratorHitTarget::Range(index) => {
                self.curator_workspace.range_cursor = index;
                self.curator_workspace.instruction_scope = CuratorInstructionScope::Range;
                self.prepare_instruction_editor_cursor();
            }
            CuratorHitTarget::Task(index) => {
                self.curator_workspace.task_cursor = index;
                self.curator_workspace.detail_scroll = 0;
                if self.curator_workspace.last_narrow {
                    self.curator_workspace.narrow_task_detail_open = true;
                }
            }
            CuratorHitTarget::PlanDetail(detail) => {
                self.curator_workspace.plan_detail = detail;
                self.curator_workspace.detail_scroll = 0;
            }
            CuratorHitTarget::ReviewItem(index) => {
                self.curator_workspace.review_cursor = index;
                self.curator_workspace.detail_scroll = 0;
                if self.curator_workspace.last_narrow {
                    self.curator_workspace.narrow_detail_open = true;
                }
            }
            CuratorHitTarget::InstructionScope(scope) => {
                self.curator_workspace.instruction_scope = scope;
                self.prepare_instruction_editor_cursor();
            }
            CuratorHitTarget::Action(action) => {
                return self.activate_curator_workspace_action(action);
            }
        }
        None
    }

    fn activate_curator_workspace_action(
        &mut self,
        action: CuratorWorkspaceAction,
    ) -> Option<ContextEditorAction> {
        match action {
            CuratorWorkspaceAction::Back => {
                self.curator_workspace.active = false;
                None
            }
            CuratorWorkspaceAction::CustomizeRoute => {
                self.curator_workspace.section = CuratorWorkspaceSection::Route;
                self.curator_workspace.pane = CuratorWorkspacePane::Detail;
                self.curator_workspace.narrow_detail_open = self.curator_workspace.last_narrow;
                self.align_curator_route_cursor_to_effective_selection();
                None
            }
            CuratorWorkspaceAction::OpenInstructions => {
                self.curator_workspace.section = CuratorWorkspaceSection::Instructions;
                self.curator_workspace.pane = CuratorWorkspacePane::Detail;
                self.curator_workspace.narrow_detail_open = self.curator_workspace.last_narrow;
                None
            }
            CuratorWorkspaceAction::Validate => self.preview_curator_plan_action(),
            CuratorWorkspaceAction::Generate | CuratorWorkspaceAction::Retry => {
                self.curator_workspace.clear_generation_outcome();
                self.prepare_action().1
            }
            CuratorWorkspaceAction::RestoreDefault => {
                self.restore_curator_default();
                self.align_curator_route_cursor_to_effective_selection();
                None
            }
            CuratorWorkspaceAction::SaveDefault => {
                if self.can_save_curator_default() {
                    self.curator_workspace.overlay = Some(CuratorWorkspaceOverlay::SaveDefault);
                }
                None
            }
            CuratorWorkspaceAction::BeginInstructionEdit => {
                self.curator_workspace.instruction_editing = true;
                self.prepare_instruction_editor_cursor();
                None
            }
            CuratorWorkspaceAction::FinishInstructionEdit => {
                self.curator_workspace.instruction_editing = false;
                None
            }
            CuratorWorkspaceAction::CancelDraft => self
                .draft_id
                .clone()
                .map(|draft_id| ContextEditorAction::CancelDraft { draft_id }),
            CuratorWorkspaceAction::LeaveRunning => {
                self.curator_workspace.overlay = Some(CuratorWorkspaceOverlay::LeaveRunning);
                None
            }
            CuratorWorkspaceAction::EditRun => {
                self.phase = ContextEditorPhase::Editing;
                self.draft = None;
                self.selection_preview = None;
                self.selection_preview_pending = false;
                self.curator_workspace.generation_outcome = None;
                self.curator_workspace.section = CuratorWorkspaceSection::Overview;
                None
            }
            CuratorWorkspaceAction::ToggleProposal => self.toggle_selected_review_proposal().1,
            CuratorWorkspaceAction::Apply => {
                if self.apply_disabled_reason().is_none() {
                    self.curator_workspace.overlay = Some(CuratorWorkspaceOverlay::Apply);
                }
                None
            }
        }
    }

    pub(super) fn render_curator_workspace(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let narrow = area.width < WORKSPACE_NARROW_WIDTH;
        self.curator_workspace.last_narrow = narrow;
        self.narrow_layout = narrow;
        self.curator_workspace.hit_regions = CuratorWorkspaceHitRegions::default();
        frame.render_widget(Clear, area);

        let state_label = self.curator_workspace_state_label();
        let title = self.curator_workspace_title(narrow, state_label);
        let outer = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(title)
            .border_style(Style::default().fg(Color::Rgb(186, 139, 255)));
        let inner = outer.inner(area);
        frame.render_widget(outer, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let footer_height = self
            .curator_workspace_footer_height(inner.width)
            .min(inner.height.saturating_sub(4))
            .max(1);
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(footer_height),
        ])
        .split(inner);
        self.render_curator_workspace_header(frame, chunks[0], narrow);

        if self.phase == ContextEditorPhase::PreparingDraft {
            self.render_curator_progress(frame, chunks[1], narrow);
        } else if self.phase == ContextEditorPhase::ReviewDraft {
            self.render_curator_review(frame, chunks[1], narrow);
        } else if self.curator_workspace.generation_outcome.is_some() {
            self.render_curator_outcome(frame, chunks[1], narrow);
        } else {
            self.render_curator_configuration(frame, chunks[1], narrow);
        }
        self.render_curator_workspace_footer(frame, chunks[2], narrow);
        self.render_curator_workspace_overlay(frame, area);
    }

    fn curator_workspace_title(&self, narrow: bool, state_label: &str) -> String {
        if !narrow {
            return format!(" Prepare context review · {state_label} ");
        }
        let path = if self.phase == ContextEditorPhase::PreparingDraft {
            "Generation".to_string()
        } else if self.phase == ContextEditorPhase::ReviewDraft {
            if self.curator_workspace.narrow_detail_open {
                "Atomic review > Detail".to_string()
            } else {
                "Atomic review".to_string()
            }
        } else if self.curator_workspace.generation_outcome.is_some() {
            "Preparation stopped".to_string()
        } else if !self.curator_workspace.narrow_detail_open {
            "Run sheet".to_string()
        } else if self.curator_workspace.section == CuratorWorkspaceSection::ExactCalls
            && self.curator_workspace.narrow_task_detail_open
        {
            let task_count = self
                .curator_plan
                .as_ref()
                .map_or(0, |plan| plan.tasks.len());
            format!(
                "Exact calls > Call {}/{} > {}",
                self.curator_workspace.task_cursor.saturating_add(1),
                task_count,
                self.curator_workspace.plan_detail.label()
            )
        } else if self.curator_workspace.section == CuratorWorkspaceSection::Instructions {
            format!(
                "Instructions > {}",
                match self.curator_workspace.instruction_scope {
                    CuratorInstructionScope::Transaction => "Transaction-wide",
                    CuratorInstructionScope::Range => "Range-specific",
                }
            )
        } else {
            self.curator_workspace.section.label().to_string()
        };
        format!(" Prepare context review > {path} · {state_label} ")
    }

    fn render_curator_workspace_header(&self, frame: &mut Frame, area: Rect, narrow: bool) {
        let (work, activity) = if self.phase == ContextEditorPhase::ReviewDraft {
            self.draft.as_ref().map_or_else(
                || {
                    (
                        "Atomic transaction review".to_string(),
                        "review before apply",
                    )
                },
                |draft| {
                    (
                        format!(
                            "{} required · {} eligible · {} ineligible · {} curator call{}",
                            draft.required_operations.len(),
                            draft.distillation_proposals.len(),
                            draft.ineligible_distillations.len(),
                            draft.curator_usage.len(),
                            if draft.curator_usage.len() == 1 {
                                ""
                            } else {
                                "s"
                            },
                        ),
                        "generation complete · review before apply",
                    )
                },
            )
        } else {
            let request = self.current_draft_request();
            let range_count = request.summary_ranges.len();
            let tool_count = request.tool_results.len();
            let task_count = range_count + tool_count;
            let reasoning = usize::from(request.reasoning.is_some());
            (
                format!(
                    "{range_count} range{} · {tool_count} output{} · {reasoning} reasoning · {task_count} {}call{}",
                    if range_count == 1 { "" } else { "s" },
                    if tool_count == 1 { "" } else { "s" },
                    if narrow { "" } else { "isolated " },
                    if task_count == 1 { "" } else { "s" },
                ),
                if self.phase == ContextEditorPhase::PreparingDraft {
                    "model generation active"
                } else if self.curator_plan_pending {
                    "checking exact calls · no model invoked"
                } else {
                    "no model invoked yet"
                },
            )
        };
        let mut lines = vec![Line::from(vec![Span::styled(
            one_line(&format!("{work} · {activity}"), area.width as usize),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )])];
        if narrow {
            let badge = self.curator_route_narrow_badge();
            let route_width = (area.width as usize)
                .saturating_sub(badge.chars().count())
                .saturating_sub(1);
            lines.push(Line::from(vec![
                Span::styled(badge, curator_badge_style()),
                Span::raw(" "),
                Span::styled(
                    one_line(&self.curator_route_compact_summary(), route_width),
                    Style::default().fg(Color::Gray),
                ),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled(self.curator_route_state_badge(), curator_badge_style()),
                Span::raw(" "),
                Span::styled(
                    self.curator_route_summary(),
                    Style::default().fg(Color::Gray),
                ),
            ]));
        }
        let feedback = self
            .error
            .as_deref()
            .map(|error| (format!("Error: {error}"), Color::Red))
            .or_else(|| {
                if self.phase == ContextEditorPhase::PreparingDraft
                    || self.curator_workspace.generation_outcome.is_some()
                {
                    self.status
                        .as_deref()
                        .map(|status| (status.to_string(), Color::DarkGray))
                } else {
                    None
                }
            })
            .or_else(|| {
                self.curator_workspace
                    .feedback
                    .as_deref()
                    .map(|feedback| (feedback.to_string(), Color::DarkGray))
            })
            .or_else(|| {
                self.curator_workspace
                    .plan_dirty_reason
                    .as_deref()
                    .map(|reason| (format!("Needs re-validation: {reason}"), Color::Yellow))
            })
            .or_else(|| {
                self.status
                    .as_deref()
                    .map(|status| (status.to_string(), Color::DarkGray))
            })
            .unwrap_or_else(|| {
                (
                    "Review the run, then explicitly generate.".to_string(),
                    Color::DarkGray,
                )
            });
        lines.push(
            Line::from(one_line(&feedback.0, area.width as usize))
                .style(Style::default().fg(feedback.1)),
        );
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn render_curator_configuration(&mut self, frame: &mut Frame, area: Rect, narrow: bool) {
        if narrow {
            if self.curator_workspace.narrow_detail_open {
                self.curator_workspace.hit_regions.detail = area;
                self.render_curator_section_detail(frame, area, true);
            } else {
                self.curator_workspace.hit_regions.run_sheet = area;
                self.render_curator_run_sheet(frame, area, true);
            }
            return;
        }
        let panes = Layout::horizontal([Constraint::Percentage(29), Constraint::Percentage(71)])
            .split(area);
        self.curator_workspace.hit_regions.run_sheet = panes[0];
        self.curator_workspace.hit_regions.detail = panes[1];
        self.render_curator_run_sheet(frame, panes[0], false);
        self.render_curator_section_detail(frame, panes[1], false);
    }

    fn render_curator_run_sheet(&mut self, frame: &mut Frame, area: Rect, narrow: bool) {
        let focused = narrow || self.curator_workspace.pane == CuratorWorkspacePane::RunSheet;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(if focused {
                " Run sheet · focused "
            } else {
                " Run sheet "
            })
            .border_style(workspace_focus_style(focused));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let mut lines = Vec::new();
        for (row, section) in CuratorWorkspaceSection::ALL.into_iter().enumerate() {
            if row >= inner.height as usize {
                break;
            }
            let selected = section == self.curator_workspace.section;
            let marker = if selected { "▸" } else { " " };
            let status = self.curator_section_status(section);
            let line = Line::from(vec![
                Span::styled(
                    format!("{marker} {:<16}", section.label()),
                    if selected {
                        Style::default()
                            .fg(Color::White)
                            .bg(Color::Rgb(50, 45, 70))
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    },
                ),
                Span::styled(status, curator_status_style(section, self)),
            ]);
            lines.push(line);
            self.curator_workspace.hit_regions.targets.push((
                Rect::new(inner.x, inner.y + row as u16, inner.width, 1),
                CuratorHitTarget::Section(section),
            ));
        }
        if lines.len() < inner.height as usize {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Enter open · g next action",
                Style::default().fg(Color::DarkGray),
            )));
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_curator_section_detail(&mut self, frame: &mut Frame, area: Rect, narrow: bool) {
        match self.curator_workspace.section {
            CuratorWorkspaceSection::Overview => self.render_curator_overview(frame, area, narrow),
            CuratorWorkspaceSection::Route => self.render_curator_routes(frame, area, narrow),
            CuratorWorkspaceSection::Instructions => {
                self.render_curator_instructions(frame, area, narrow)
            }
            CuratorWorkspaceSection::ExactCalls => {
                self.render_curator_exact_calls(frame, area, narrow)
            }
        }
    }

    fn render_curator_overview(&mut self, frame: &mut Frame, area: Rect, narrow: bool) {
        let focused = narrow || self.curator_workspace.pane == CuratorWorkspacePane::Detail;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(if focused {
                " Run overview · focused "
            } else {
                " Run overview "
            })
            .border_style(workspace_focus_style(focused));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let request = self.current_draft_request();
        let task_count = request.summary_ranges.len() + request.tool_results.len();
        let plan_state = self.curator_plan_state_label();
        let instructions = self.curator_instruction_summary();
        let lines = vec![
            Line::from(Span::styled(
                self.curator_route_state_badge(),
                curator_badge_style(),
            )),
            Line::from(Span::styled(
                self.curator_route_summary(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("Work  ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    "{} range summary · {} output evaluation · {} reasoning action",
                    request.summary_ranges.len(),
                    request.tool_results.len(),
                    usize::from(request.reasoning.is_some())
                )),
            ]),
            Line::from(vec![
                Span::styled("Plan  ", Style::default().fg(Color::DarkGray)),
                Span::styled(plan_state, Style::default().fg(Color::Yellow)),
                Span::raw(format!(
                    " · {task_count} isolated call{}",
                    if task_count == 1 { "" } else { "s" }
                )),
            ]),
            Line::from(vec![
                Span::styled("Instructions  ", Style::default().fg(Color::DarkGray)),
                Span::raw(instructions),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "Exact validation invokes no model. Generation remains explicit and may consume provider quota.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "Every task uses complete source. Atomic failure retains no applicable partial artifact.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                if self.curator_plan_current() {
                    "Ready to generate the reviewed isolated calls."
                } else {
                    "Validate the exact calls before generation."
                },
                Style::default()
                    .fg(Color::Rgb(140, 220, 170))
                    .add_modifier(Modifier::BOLD),
            )),
        ];
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((
                self.curator_workspace.detail_scroll.min(u16::MAX as usize) as u16,
                0,
            )),
            inner,
        );
    }

    fn render_curator_routes(&mut self, frame: &mut Frame, area: Rect, narrow: bool) {
        let focused = narrow || self.curator_workspace.pane == CuratorWorkspacePane::Detail;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(if focused {
                " Route & effort · focused "
            } else {
                " Route & effort "
            })
            .border_style(workspace_focus_style(focused));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let filtered = self.filtered_curator_route_indices();
        self.curator_workspace.route_cursor = self
            .curator_workspace
            .route_cursor
            .min(filtered.len().saturating_sub(1));
        let mut lines = vec![Line::from(vec![
            Span::styled("Search  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                if self.curator_workspace.route_filter.is_empty() {
                    if self.curator_workspace.route_search_active {
                        "▎".to_string()
                    } else {
                        "/ to filter".to_string()
                    }
                } else {
                    format!(
                        "{}{}",
                        self.curator_workspace.route_filter,
                        if self.curator_workspace.route_search_active {
                            "▎"
                        } else {
                            ""
                        }
                    )
                },
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "  {}/{} routes",
                    filtered.len(),
                    self.snapshot
                        .as_ref()
                        .map_or(0, |s| s.curator_route_options.len())
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ])];
        let effective_prefix = "Effective  ";
        let effective_badge = self.curator_route_state_badge();
        let effective_width = inner.width.saturating_sub(
            (effective_prefix.chars().count() + effective_badge.chars().count() + 1) as u16,
        );
        lines.push(Line::from(vec![
            Span::styled(effective_prefix, Style::default().fg(Color::DarkGray)),
            Span::styled(effective_badge, curator_badge_style()),
            Span::raw(" "),
            Span::raw(one_line(
                &self.curator_route_summary(),
                effective_width as usize,
            )),
        ]));
        let list_height = inner.height.saturating_sub(3) as usize;
        let max_start = filtered.len().saturating_sub(list_height);
        let start = self.curator_workspace.route_scroll.min(max_start);
        self.curator_workspace.route_scroll = start;
        for (visible_row, option_index) in filtered
            .iter()
            .copied()
            .skip(start)
            .take(list_height)
            .enumerate()
        {
            let Some(option) = self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.curator_route_options.get(option_index))
            else {
                continue;
            };
            let list_index = start + visible_row;
            let cursor = list_index == self.curator_workspace.route_cursor;
            let state = self.curator_option_state(option);
            let marker = match (cursor, state) {
                (true, Some(state)) => format!("▸ {state}"),
                (true, None) => "▸".to_string(),
                (false, Some(state)) => state.to_string(),
                (false, None) => String::new(),
            };
            let effort = self.curator_option_effort(option, state);
            let row = format!(
                "{marker:<9} {} · {} · {} · effort {effort}{}",
                option.model,
                option.provider,
                option.route,
                if option.detail.trim().is_empty() {
                    String::new()
                } else {
                    format!(" · {}", option.detail.trim())
                }
            );
            lines.push(Line::from(Span::styled(
                one_line(&row, inner.width as usize),
                if cursor {
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::Rgb(50, 45, 70))
                        .add_modifier(Modifier::BOLD)
                } else if state.is_some() {
                    Style::default().fg(Color::Rgb(140, 220, 170))
                } else {
                    Style::default().fg(Color::Gray)
                },
            )));
            self.curator_workspace.hit_regions.targets.push((
                Rect::new(inner.x, inner.y + 2 + visible_row as u16, inner.width, 1),
                CuratorHitTarget::Route(option_index),
            ));
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_curator_instructions(&mut self, frame: &mut Frame, area: Rect, narrow: bool) {
        let focused = narrow || self.curator_workspace.pane == CuratorWorkspacePane::Detail;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(if focused {
                " Instructions · focused "
            } else {
                " Instructions "
            })
            .border_style(workspace_focus_style(focused));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let tab_area = Rect::new(inner.x, inner.y, inner.width, 1);
        let transaction_selected =
            self.curator_workspace.instruction_scope == CuratorInstructionScope::Transaction;
        let tabs = Line::from(vec![
            Span::styled(
                " Transaction-wide ",
                instruction_tab_style(transaction_selected),
            ),
            Span::raw("  "),
            Span::styled(
                format!(" Range-specific ({}) ", self.staged_ranges.len()),
                instruction_tab_style(!transaction_selected),
            ),
        ]);
        frame.render_widget(Paragraph::new(tabs), tab_area);
        self.curator_workspace.hit_regions.targets.push((
            Rect::new(inner.x, inner.y, 20.min(inner.width), 1),
            CuratorHitTarget::InstructionScope(CuratorInstructionScope::Transaction),
        ));
        self.curator_workspace.hit_regions.targets.push((
            Rect::new(
                inner.x + 22.min(inner.width),
                inner.y,
                inner.width.saturating_sub(22),
                1,
            ),
            CuratorHitTarget::InstructionScope(CuratorInstructionScope::Range),
        ));

        let scope_area = Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            2.min(inner.height.saturating_sub(1)),
        );
        match self.curator_workspace.instruction_scope {
            CuratorInstructionScope::Transaction => {
                frame.render_widget(
                    Paragraph::new(
                        "Applies additively to every isolated call. Mandatory preservation contracts remain enforced.",
                    )
                    .style(Style::default().fg(Color::DarkGray))
                    .wrap(Wrap { trim: false }),
                    scope_area,
                );
            }
            CuratorInstructionScope::Range => {
                let mut selector_spans = Vec::new();
                let mut selector_x = scope_area.x;
                let start = self.curator_workspace.range_cursor.saturating_sub(4);
                for index in start..self.staged_ranges.len() {
                    let label = format!(" {} ", index + 1);
                    let label_width = label.chars().count().min(u16::MAX as usize) as u16;
                    if selector_x.saturating_add(label_width) > scope_area.right() {
                        break;
                    }
                    let selected = index == self.curator_workspace.range_cursor;
                    selector_spans.push(Span::styled(
                        label,
                        if selected {
                            Style::default()
                                .fg(Color::White)
                                .bg(Color::Rgb(50, 45, 70))
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        },
                    ));
                    self.curator_workspace.hit_regions.targets.push((
                        Rect::new(selector_x, scope_area.y, label_width, 1),
                        CuratorHitTarget::Range(index),
                    ));
                    selector_x = selector_x.saturating_add(label_width + 1);
                    selector_spans.push(Span::raw(" "));
                }
                let metadata = self
                    .staged_ranges
                    .get(self.curator_workspace.range_cursor)
                    .map(|range| {
                        format!(
                            "Range {} of {} · closed messages {}..{} · {} messages · {} tokens",
                            self.curator_workspace.range_cursor + 1,
                            self.staged_ranges.len(),
                            range.source_range.start_index_hint,
                            range.source_range.end_index_hint,
                            range.source_range.message_count,
                            format_tokens(range.source_tokens),
                        )
                    })
                    .unwrap_or_else(|| {
                        "Stage a summary range before adding range-specific instructions."
                            .to_string()
                    });
                frame.render_widget(
                    Paragraph::new(vec![
                        Line::from(selector_spans),
                        Line::from(one_line(&metadata, scope_area.width as usize)),
                    ])
                    .style(Style::default().fg(Color::DarkGray)),
                    scope_area,
                );
            }
        }
        let editor_y = inner.y + scope_area.height + 1;
        let editor_height = inner.bottom().saturating_sub(editor_y).saturating_sub(1);
        let editor_area = Rect::new(inner.x, editor_y, inner.width, editor_height);
        let editing = self.curator_workspace.instruction_editing;
        let editor_scroll = self.curator_workspace.instruction_editor.scroll;
        let (text, cursor) = self.current_instruction_text_and_cursor();
        let display = if editing {
            text_with_cursor(&text, cursor)
        } else if text.is_empty() {
            "(none)".to_string()
        } else {
            text.clone()
        };
        let editor_block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(if editing {
                " Editing · Esc done "
            } else {
                " Enter to edit "
            })
            .border_style(if editing {
                Style::default().fg(Color::Rgb(186, 139, 255))
            } else {
                Style::default().fg(Color::DarkGray)
            });
        let editor_inner = editor_block.inner(editor_area);
        frame.render_widget(editor_block, editor_area);
        frame.render_widget(
            Paragraph::new(display)
                .wrap(Wrap { trim: false })
                .scroll((editor_scroll.min(u16::MAX as usize) as u16, 0)),
            editor_inner,
        );
        let field_chars = text.chars().count();
        let total_chars = self.total_curator_instruction_chars();
        let counter_area = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("{field_chars}/{CONTEXT_CURATOR_INSTRUCTION_MAX_CHARS} field"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(" · "),
                Span::styled(
                    format!("{total_chars}/{CONTEXT_CURATOR_TOTAL_INSTRUCTION_MAX_CHARS} total"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(" · "),
                Span::styled(
                    if editing {
                        "changes require re-validation"
                    } else {
                        "instructions are temporary"
                    },
                    Style::default().fg(Color::Yellow),
                ),
            ])),
            counter_area,
        );
    }

    fn render_curator_exact_calls(&mut self, frame: &mut Frame, area: Rect, narrow: bool) {
        let focused = narrow || self.curator_workspace.pane == CuratorWorkspacePane::Detail;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(if focused {
                " Exact calls · focused "
            } else {
                " Exact calls "
            })
            .border_style(workspace_focus_style(focused));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if self.curator_plan_pending {
            let lines = vec![
                Line::from(Span::styled(
                    "Checking exact calls",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from("No model is invoked during this validation."),
                Line::from(
                    "Complete source, route identity, images, token budget, request bytes, response bounds, and source scope are being checked.",
                ),
                Line::from(""),
                Line::from(Span::styled(
                    "Staged operations and temporary instructions remain editable if validation fails.",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            return;
        }
        let Some(plan) = self.curator_plan.as_ref() else {
            let reason = self
                .curator_workspace
                .plan_dirty_reason
                .as_deref()
                .unwrap_or("Exact validation has not run.");
            let lines = vec![
                Line::from(Span::styled(
                    "NEEDS RE-VALIDATION",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(reason.to_string()),
                Line::from(""),
                Line::from(
                    "Staged operations and temporary transaction/range instructions are preserved.",
                ),
                Line::from("Validation invokes no model and must succeed before generation."),
                Line::from(Span::styled(
                    "Press g or choose Validate exact calls.",
                    Style::default()
                        .fg(Color::Rgb(140, 220, 170))
                        .add_modifier(Modifier::BOLD),
                )),
            ];
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            return;
        };

        self.curator_workspace.task_cursor = self
            .curator_workspace
            .task_cursor
            .min(plan.tasks.len().saturating_sub(1));
        if narrow {
            if self.curator_workspace.narrow_task_detail_open {
                self.render_curator_task_detail(frame, inner);
            } else {
                self.render_curator_task_list(frame, inner);
            }
        } else {
            let panes =
                Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(64)])
                    .split(inner);
            self.render_curator_task_list(frame, panes[0]);
            self.render_curator_task_detail(frame, panes[1]);
        }
    }

    fn render_curator_task_list(&mut self, frame: &mut Frame, area: Rect) {
        let Some(tasks) = self.curator_plan.as_ref().map(|plan| plan.tasks.clone()) else {
            return;
        };
        let block = Block::default()
            .borders(Borders::RIGHT)
            .title(format!(
                " {} isolated call{} ",
                tasks.len(),
                if tasks.len() == 1 { "" } else { "s" }
            ))
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let start = self
            .curator_workspace
            .task_cursor
            .saturating_sub(inner.height.saturating_sub(1) as usize);
        let mut lines = Vec::new();
        for (visible_row, (index, task)) in tasks
            .iter()
            .enumerate()
            .skip(start)
            .take(inner.height as usize)
            .enumerate()
        {
            let selected = index == self.curator_workspace.task_cursor;
            let marker = if selected { "▸" } else { " " };
            let label = format!(
                "{marker} {} · {} · FITS",
                curator_role_label(task.role),
                curator_task_target_label(task)
            );
            lines.push(Line::from(Span::styled(
                one_line(&label, inner.width as usize),
                if selected {
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::Rgb(50, 45, 70))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                },
            )));
            self.curator_workspace.hit_regions.targets.push((
                Rect::new(inner.x, inner.y + visible_row as u16, inner.width, 1),
                CuratorHitTarget::Task(index),
            ));
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_curator_task_detail(&mut self, frame: &mut Frame, area: Rect) {
        let Some(plan) = self.curator_plan.as_ref() else {
            return;
        };
        let Some(task) = plan.tasks.get(self.curator_workspace.task_cursor) else {
            frame.render_widget(Paragraph::new("No isolated call selected."), area);
            return;
        };
        let tabs_area = Rect::new(area.x, area.y, area.width, 1.min(area.height));
        let mut spans = Vec::new();
        let mut x = tabs_area.x;
        for detail in CuratorPlanDetail::ALL {
            let label = format!(" {} ", detail.tab_label());
            let width = label.chars().count().min(u16::MAX as usize) as u16;
            if x.saturating_add(width) > tabs_area.right() {
                break;
            }
            spans.push(Span::styled(
                label,
                if detail == self.curator_workspace.plan_detail {
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::Rgb(50, 45, 70))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ));
            self.curator_workspace.hit_regions.targets.push((
                Rect::new(x, tabs_area.y, width, 1),
                CuratorHitTarget::PlanDetail(detail),
            ));
            x = x.saturating_add(width + 1);
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), tabs_area);
        let detail_area = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(1),
        );
        self.curator_workspace.hit_regions.scrollable_detail = detail_area;
        let lines = curator_task_detail_lines(task, plan, self.curator_workspace.plan_detail);
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((
                self.curator_workspace.detail_scroll.min(u16::MAX as usize) as u16,
                0,
            )),
            detail_area,
        );
    }

    fn render_curator_progress(&mut self, frame: &mut Frame, area: Rect, narrow: bool) {
        let progress = self.draft_progress.as_ref();
        let completed = progress.map_or(0, |progress| progress.completed_items);
        let total = progress.map_or_else(
            || {
                self.curator_plan
                    .as_ref()
                    .map_or(0, |plan| plan.tasks.len())
            },
            |progress| progress.total_items,
        );
        let phase = progress.map_or("preparing", |progress| draft_phase_label(progress.phase));
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(format!(
                " Generating isolated curator calls · {completed}/{total} "
            ))
            .border_style(Style::default().fg(Color::Rgb(255, 193, 7)));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let tasks = self
            .curator_plan
            .as_ref()
            .map(|plan| plan.tasks.as_slice())
            .unwrap_or(&[]);
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Phase  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    phase,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(
                "Each row is one independent model call. Provider quota and time may be consumed.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "No partial artifact is applicable or retained if preparation fails or is canceled.",
                Style::default().fg(Color::Yellow),
            )),
            Line::from(""),
        ];
        for (index, task) in tasks.iter().enumerate() {
            let state = if index < completed {
                "DONE"
            } else if index == completed
                && progress
                    .is_some_and(|progress| progress.phase == ContextDraftPhase::PreparingArtifacts)
            {
                "RUNNING"
            } else {
                "WAITING"
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{state:<8}"),
                    match state {
                        "DONE" => Style::default().fg(Color::Rgb(140, 220, 170)),
                        "RUNNING" => Style::default()
                            .fg(Color::Rgb(255, 193, 7))
                            .add_modifier(Modifier::BOLD),
                        _ => Style::default().fg(Color::DarkGray),
                    },
                ),
                Span::raw(format!(
                    "{} · {}",
                    curator_role_label(task.role),
                    curator_task_target_label(task)
                )),
            ]));
        }
        if progress
            .is_some_and(|progress| progress.phase == ContextDraftPhase::ValidatingProjection)
        {
            lines.push(Line::from(vec![
                Span::styled(
                    "RUNNING ",
                    Style::default()
                        .fg(Color::Rgb(255, 193, 7))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("Validating the complete projected provider context"),
            ]));
        }
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((
                self.curator_workspace.detail_scroll.min(u16::MAX as usize) as u16,
                0,
            )),
            inner,
        );
        if narrow {
            self.curator_workspace.hit_regions.detail = area;
        }
    }

    fn render_curator_outcome(&mut self, frame: &mut Frame, area: Rect, _narrow: bool) {
        let outcome = self.curator_workspace.generation_outcome;
        let (title, color) = match outcome {
            Some(CuratorGenerationOutcome::Failed) => ("Preparation failed", Color::Red),
            Some(CuratorGenerationOutcome::Canceled) => ("Preparation canceled", Color::Yellow),
            Some(CuratorGenerationOutcome::Expired) => ("Prepared review expired", Color::Yellow),
            None => ("Preparation stopped", Color::Yellow),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(format!(" {title} "))
            .border_style(Style::default().fg(color));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let progress = self.draft_progress.as_ref();
        let completed = progress.map_or(0, |progress| progress.completed_items);
        let total = progress.map_or_else(
            || {
                self.curator_plan
                    .as_ref()
                    .map_or(0, |plan| plan.tasks.len())
            },
            |progress| progress.total_items,
        );
        let lines = vec![
            Line::from(Span::styled(
                title,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(format!(
                "Completed before stop: {completed}/{total} isolated calls."
            )),
            Line::from(Span::styled(
                "No curator artifacts were retained because preparation is atomic.",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("Retry reruns every isolated call from a freshly validated exact plan."),
            Line::from(
                "Staged operations and temporary transaction/range instructions remain available.",
            ),
            Line::from(""),
            Line::from(Span::styled(
                self.error
                    .as_deref()
                    .unwrap_or("No additional error details were reported."),
                Style::default().fg(if self.error.is_some() {
                    Color::Red
                } else {
                    Color::DarkGray
                }),
            )),
        ];
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((
                self.curator_workspace.detail_scroll.min(u16::MAX as usize) as u16,
                0,
            )),
            inner,
        );
    }

    fn render_curator_review(&mut self, frame: &mut Frame, area: Rect, narrow: bool) {
        if self.draft.is_none() {
            frame.render_widget(Paragraph::new("The generated review is unavailable."), area);
            return;
        }
        self.curator_workspace.review_cursor = self
            .curator_workspace
            .review_cursor
            .min(self.curator_review_item_count().saturating_sub(1));
        if narrow {
            if self.curator_workspace.narrow_detail_open {
                self.render_curator_review_detail(frame, area);
            } else {
                self.render_curator_review_list(frame, area);
            }
            return;
        }
        let panes = Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(66)])
            .split(area);
        self.render_curator_review_list(frame, panes[0]);
        self.render_curator_review_detail(frame, panes[1]);
    }

    fn render_curator_review_list(&mut self, frame: &mut Frame, area: Rect) {
        let Some(draft) = self.draft.as_ref() else {
            return;
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(" Atomic transaction review ")
            .border_style(workspace_focus_style(true));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let labels = curator_review_labels(draft, &self.selected_distillation_ids);
        let start = self
            .curator_workspace
            .review_cursor
            .saturating_sub(inner.height.saturating_sub(1) as usize);
        let mut lines = Vec::new();
        for (visible_row, (index, label)) in labels
            .iter()
            .enumerate()
            .skip(start)
            .take(inner.height as usize)
            .enumerate()
        {
            let selected = index == self.curator_workspace.review_cursor;
            let marker = if selected { "▸" } else { " " };
            lines.push(Line::from(Span::styled(
                one_line(&format!("{marker} {label}"), inner.width as usize),
                if selected {
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::Rgb(50, 45, 70))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                },
            )));
            self.curator_workspace.hit_regions.targets.push((
                Rect::new(inner.x, inner.y + visible_row as u16, inner.width, 1),
                CuratorHitTarget::ReviewItem(index),
            ));
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_curator_review_detail(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(" Selected review detail ")
            .border_style(workspace_focus_style(true));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let lines = self.curator_review_detail_lines();
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((
                self.curator_workspace.detail_scroll.min(u16::MAX as usize) as u16,
                0,
            )),
            inner,
        );
    }

    fn curator_workspace_footer_height(&self, width: u16) -> u16 {
        let width = width.max(1) as usize;
        let mut rows = 0usize;
        let mut row_width = 0usize;
        for (_, label, _) in self.curator_workspace_actions() {
            let rendered_width = label.chars().count() + 2;
            let needed = rendered_width + usize::from(row_width > 0);
            if row_width > 0 && row_width + needed > width {
                rows += 1;
                row_width = 0;
            }
            if row_width > 0 {
                row_width += 1;
            }
            row_width += rendered_width;
        }
        if row_width > 0 {
            rows += 1;
        }
        rows.saturating_add(1).min(u16::MAX as usize) as u16
    }

    fn render_curator_workspace_footer(&mut self, frame: &mut Frame, area: Rect, narrow: bool) {
        let actions = self.curator_workspace_actions();
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut row_width = 0usize;
        let width = area.width as usize;
        let mut y = area.y;
        let mut x = area.x;
        for (action, label, enabled) in actions {
            let rendered = if enabled {
                format!("[{label}]")
            } else {
                format!("({label})")
            };
            let needed = rendered.chars().count() + usize::from(!spans.is_empty());
            if row_width + needed > width && !spans.is_empty() {
                lines.push(Line::from(std::mem::take(&mut spans)));
                row_width = 0;
                y = y.saturating_add(1);
                x = area.x;
            }
            if row_width > 0 {
                spans.push(Span::raw(" "));
                row_width += 1;
                x = x.saturating_add(1);
            }
            let button_width = rendered.chars().count().min(u16::MAX as usize) as u16;
            spans.push(Span::styled(
                rendered,
                if enabled {
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::Rgb(45, 45, 60))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ));
            if enabled && y < area.bottom() {
                self.curator_workspace.hit_regions.targets.push((
                    Rect::new(x, y, button_width, 1),
                    CuratorHitTarget::Action(action),
                ));
            }
            row_width += button_width as usize;
            x = x.saturating_add(button_width);
        }
        if !spans.is_empty() && lines.len() < area.height as usize {
            lines.push(Line::from(spans));
        }
        let help = if self.phase == ContextEditorPhase::PreparingDraft {
            "c cancel · Esc leave confirmation · j/k scroll"
        } else if self.phase == ContextEditorPhase::ReviewDraft {
            if narrow && self.curator_workspace.narrow_detail_open {
                "j/k scroll · Space toggle proposal · a apply · Esc list"
            } else if narrow {
                "j/k item · Enter inspect · Space toggle · a apply · e edit run"
            } else {
                "j/k item · Space toggle proposal · a apply · e edit run · PgUp/PgDn detail"
            }
        } else if self.curator_workspace.generation_outcome.is_some() {
            "g retry all · e edit run · j/k scroll · Esc context · ? help"
        } else if narrow {
            "Enter open · Esc back · g next safe action · ? help"
        } else {
            "Tab pane · j/k navigate · Enter open · g next safe action · Esc context · ? help"
        };
        if lines.len() < area.height as usize {
            lines.push(Line::from(Span::styled(
                one_line(help, width),
                Style::default().fg(Color::DarkGray),
            )));
        }
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn render_curator_workspace_overlay(&self, frame: &mut Frame, area: Rect) {
        let Some(overlay) = self.curator_workspace.overlay else {
            return;
        };
        let width = area.width.saturating_sub(4).clamp(12, 78);
        let height = match overlay {
            CuratorWorkspaceOverlay::Help => area.height.saturating_sub(4).clamp(7, 16),
            _ => area.height.saturating_sub(4).clamp(6, 9),
        };
        let modal_area = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, modal_area);
        let (title, body, color) = match overlay {
            CuratorWorkspaceOverlay::SaveDefault => (
                " Save curator default ",
                vec![
                    Line::from(Span::styled(
                        self.curator_route_summary(),
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from("Only provider, route, model, and effort will be saved."),
                    Line::from("Transaction and range instructions remain temporary."),
                    Line::from(Span::styled(
                        "Enter/Y save · Esc/N cancel",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )),
                ],
                Color::Yellow,
            ),
            CuratorWorkspaceOverlay::LeaveRunning => (
                " Preparation is still running ",
                vec![
                    Line::from("Leave the Context Editor while preparation continues?"),
                    Line::from(
                        "No context change applies until the generated review is explicitly accepted.",
                    ),
                    Line::from(""),
                    Line::from(Span::styled(
                        "Enter/Y keep running in background · Esc/N stay",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )),
                ],
                Color::Yellow,
            ),
            CuratorWorkspaceOverlay::Apply => (
                " Apply atomic context transaction ",
                vec![
                    Line::from(
                        "Apply every required operation and the currently selected eligible distillations?",
                    ),
                    Line::from(
                        "This creates one context revision and one intentional cache transition.",
                    ),
                    Line::from(""),
                    Line::from(Span::styled(
                        "Enter/Y apply · Esc/N cancel",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )),
                ],
                Color::Yellow,
            ),
            CuratorWorkspaceOverlay::Help => (
                " Prepare context review help ",
                vec![
                    Line::from("Run sheet: Overview, Route & effort, Instructions, Exact calls."),
                    Line::from(
                        "Tab changes pane on wide terminals. Enter drills into the selected item.",
                    ),
                    Line::from(
                        "Esc unwinds search/detail first, then returns to Authoritative History.",
                    ),
                    Line::from(
                        "g validates exact calls, generates a current plan, or retries all calls.",
                    ),
                    Line::from(
                        "Route selection is temporary until Save as default is explicitly confirmed.",
                    ),
                    Line::from(
                        "Instruction edits remain temporary and require exact re-validation.",
                    ),
                    Line::from(
                        "Exact prompt, response contract, source scope, and fingerprint remain inspectable.",
                    ),
                    Line::from(
                        "Generation is atomic. Failure/cancellation retains no applicable partial artifact.",
                    ),
                    Line::from(""),
                    Line::from(Span::styled(
                        "Esc closes help",
                        Style::default().fg(Color::DarkGray),
                    )),
                ],
                Color::Rgb(186, 139, 255),
            ),
        };
        let paragraph = Paragraph::new(body)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .title(title)
                    .border_style(Style::default().fg(color)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, modal_area);
    }

    fn curator_workspace_actions(&self) -> Vec<(CuratorWorkspaceAction, String, bool)> {
        if self.phase == ContextEditorPhase::PreparingDraft {
            return vec![
                (
                    CuratorWorkspaceAction::CancelDraft,
                    "Cancel preparation".to_string(),
                    self.draft_id.is_some(),
                ),
                (
                    CuratorWorkspaceAction::LeaveRunning,
                    "Keep running in background".to_string(),
                    true,
                ),
            ];
        }
        if self.phase == ContextEditorPhase::ReviewDraft {
            return vec![
                (
                    CuratorWorkspaceAction::ToggleProposal,
                    "Toggle proposal".to_string(),
                    self.selected_review_proposal_index().is_some(),
                ),
                (
                    CuratorWorkspaceAction::Apply,
                    "Apply transaction".to_string(),
                    self.apply_disabled_reason().is_none(),
                ),
                (
                    CuratorWorkspaceAction::EditRun,
                    "Edit run".to_string(),
                    true,
                ),
            ];
        }
        if self.curator_workspace.generation_outcome.is_some() {
            return vec![
                (
                    CuratorWorkspaceAction::Retry,
                    "Retry all calls".to_string(),
                    true,
                ),
                (
                    CuratorWorkspaceAction::EditRun,
                    "Edit run".to_string(),
                    true,
                ),
                (
                    CuratorWorkspaceAction::Back,
                    "Return to context".to_string(),
                    true,
                ),
            ];
        }
        let mut actions = vec![(CuratorWorkspaceAction::Back, "Context".to_string(), true)];
        match self.curator_workspace.section {
            CuratorWorkspaceSection::Overview => {
                actions.push((
                    CuratorWorkspaceAction::CustomizeRoute,
                    "Customize run".to_string(),
                    true,
                ));
                actions.push((
                    CuratorWorkspaceAction::OpenInstructions,
                    "Instructions".to_string(),
                    true,
                ));
            }
            CuratorWorkspaceSection::Route => {
                let can_save = self.can_save_curator_default();
                actions.push((
                    CuratorWorkspaceAction::RestoreDefault,
                    "Restore default".to_string(),
                    self.curator_selection.is_some(),
                ));
                actions.push((
                    CuratorWorkspaceAction::SaveDefault,
                    if can_save {
                        "Save as default"
                    } else {
                        "Default saved"
                    }
                    .to_string(),
                    can_save,
                ));
            }
            CuratorWorkspaceSection::Instructions => {
                actions.push((
                    if self.curator_workspace.instruction_editing {
                        CuratorWorkspaceAction::FinishInstructionEdit
                    } else {
                        CuratorWorkspaceAction::BeginInstructionEdit
                    },
                    if self.curator_workspace.instruction_editing {
                        "Done editing"
                    } else {
                        "Edit instructions"
                    }
                    .to_string(),
                    self.curator_workspace.instruction_scope
                        == CuratorInstructionScope::Transaction
                        || !self.staged_ranges.is_empty(),
                ));
            }
            CuratorWorkspaceSection::ExactCalls => {}
        }
        let primary = if self.curator_plan_pending {
            (
                CuratorWorkspaceAction::Validate,
                "Validating exact calls".to_string(),
                false,
            )
        } else if self.curator_plan_current() {
            let count = self
                .curator_plan
                .as_ref()
                .map_or(0, |plan| plan.tasks.len());
            (
                CuratorWorkspaceAction::Generate,
                format!("Generate {count} call{}", if count == 1 { "" } else { "s" }),
                count > 0,
            )
        } else {
            (
                CuratorWorkspaceAction::Validate,
                "Validate exact calls".to_string(),
                !self.current_draft_request().is_empty(),
            )
        };
        actions.push(primary);
        actions
    }

    fn curator_workspace_state_label(&self) -> &'static str {
        if self.phase == ContextEditorPhase::PreparingDraft {
            "GENERATING"
        } else if self.phase == ContextEditorPhase::ReviewDraft {
            "REVIEW"
        } else if self.curator_workspace.generation_outcome.is_some() {
            "STOPPED"
        } else if self.curator_plan_pending {
            "CHECKING"
        } else if self.curator_plan_current() {
            "CURRENT"
        } else {
            "DRAFT"
        }
    }

    fn curator_plan_current(&self) -> bool {
        self.curator_plan.is_some()
            && self.curator_plan_request.as_ref() == Some(&self.current_draft_request())
            && !self.curator_plan_pending
    }

    fn curator_plan_state_label(&self) -> &'static str {
        if self.curator_plan_pending {
            "CHECKING"
        } else if self.curator_plan_current() {
            "CURRENT"
        } else if self.curator_workspace.plan_dirty_reason.is_some() {
            "STALE"
        } else {
            "NOT VALIDATED"
        }
    }

    fn curator_route_state_badge(&self) -> &'static str {
        if self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.curator_route.is_none())
            && self.curator_selection.is_none()
        {
            "UNAVAILABLE"
        } else if self.curator_selection.is_some() {
            "TEMPORARY FOR THIS RUN"
        } else if self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.curator_default == ContextCuratorSelection::default())
        {
            "CURRENT SESSION ROUTE"
        } else {
            "SAVED DEFAULT"
        }
    }

    fn curator_route_narrow_badge(&self) -> &'static str {
        match self.curator_route_state_badge() {
            "CURRENT SESSION ROUTE" => "SESSION ROUTE",
            "TEMPORARY FOR THIS RUN" => "TEMP RUN",
            badge => badge,
        }
    }

    fn curator_route_compact_summary(&self) -> String {
        if let Some(selection) = self.curator_selection.as_ref() {
            return format!(
                "{} · {} · {}",
                selection.model.as_deref().unwrap_or("active model"),
                selection.route.as_deref().unwrap_or("active route"),
                selection.effort.as_deref().unwrap_or("provider default")
            );
        }
        if let Some(route) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.curator_route.as_ref())
        {
            return format!(
                "{} · {} · {}",
                route.model,
                route.route,
                route.effort.as_deref().unwrap_or("provider default")
            );
        }
        "choose a capable route".to_string()
    }

    fn curator_option_state(&self, option: &ContextCuratorRouteOption) -> Option<&'static str> {
        if curator_option_matches_selection(option, self.curator_selection.as_ref()) {
            return Some("RUN");
        }
        let snapshot = self.snapshot.as_ref()?;
        if snapshot
            .curator_route
            .as_ref()
            .is_some_and(|route| curator_option_matches_resolved_route(option, route))
        {
            return Some(
                if snapshot.curator_default == ContextCuratorSelection::default() {
                    "SESSION"
                } else {
                    "DEFAULT"
                },
            );
        }
        None
    }

    fn can_save_curator_default(&self) -> bool {
        self.effective_curator_selection().is_some()
            && (self.curator_selection.is_some()
                || self.snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.curator_default == ContextCuratorSelection::default()
                }))
    }

    fn curator_option_effort<'a>(
        &'a self,
        option: &'a ContextCuratorRouteOption,
        state: Option<&str>,
    ) -> &'a str {
        if state == Some("RUN") {
            return self
                .curator_selection
                .as_ref()
                .and_then(|selection| selection.effort.as_deref())
                .unwrap_or("provider default");
        }
        if matches!(state, Some("DEFAULT" | "SESSION")) {
            return self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.curator_route.as_ref())
                .and_then(|route| route.effort.as_deref())
                .unwrap_or("provider default");
        }
        if option.efforts.is_empty() {
            "provider default"
        } else {
            "choose after selection"
        }
    }

    pub(super) fn align_curator_route_cursor_to_effective_selection(&mut self) {
        let effective_option = self.snapshot.as_ref().and_then(|snapshot| {
            snapshot.curator_route_options.iter().position(|option| {
                curator_option_matches_selection(option, self.curator_selection.as_ref())
                    || (self.curator_selection.is_none()
                        && snapshot.curator_route.as_ref().is_some_and(|route| {
                            curator_option_matches_resolved_route(option, route)
                        }))
            })
        });
        let Some(effective_option) = effective_option else {
            return;
        };
        let filtered = self.filtered_curator_route_indices();
        if let Some(cursor) = filtered
            .iter()
            .position(|option_index| *option_index == effective_option)
        {
            self.curator_workspace.route_cursor = cursor;
            self.curator_workspace.route_scroll = cursor.saturating_sub(2);
        }
    }

    fn curator_route_summary(&self) -> String {
        if let Some(selection) = self.curator_selection.as_ref() {
            return format!(
                "{} · {} · {} · effort {}",
                selection.provider.as_deref().unwrap_or("current provider"),
                selection.model.as_deref().unwrap_or("current model"),
                selection.route.as_deref().unwrap_or("current route"),
                selection.effort.as_deref().unwrap_or("provider default"),
            );
        }
        self.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.curator_route.as_ref())
            .map(|route| {
                format!(
                    "{} · {} · {} · effort {}",
                    route.provider_display_name,
                    route.model,
                    route.route,
                    route.effort.as_deref().unwrap_or("provider default")
                )
            })
            .or_else(|| {
                self.snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.curator_unavailable_reason.as_ref())
                    .map(|reason| format!("No independent route: {reason}"))
            })
            .unwrap_or_else(|| "Resolving independent curator route…".to_string())
    }

    fn curator_section_status(&self, section: CuratorWorkspaceSection) -> String {
        match section {
            CuratorWorkspaceSection::Overview => if self.curator_plan_current() {
                "READY"
            } else {
                "REVIEW"
            }
            .to_string(),
            CuratorWorkspaceSection::Route => if self.curator_selection.is_some() {
                "RUN"
            } else {
                "DEFAULT"
            }
            .to_string(),
            CuratorWorkspaceSection::Instructions => {
                let count = usize::from(!self.curator_transaction_instructions.is_empty())
                    + self
                        .curator_range_instructions
                        .values()
                        .filter(|instructions| !instructions.is_empty())
                        .count();
                if count == 0 {
                    "NONE".to_string()
                } else {
                    format!("{count} CUSTOM")
                }
            }
            CuratorWorkspaceSection::ExactCalls => self.curator_plan_state_label().to_string(),
        }
    }

    fn curator_instruction_summary(&self) -> String {
        let ranges = self
            .curator_range_instructions
            .values()
            .filter(|instructions| !instructions.is_empty())
            .count();
        format!(
            "transaction {} chars · {ranges} range override(s)",
            self.curator_transaction_instructions.chars().count()
        )
    }

    fn current_instruction_text_and_cursor(&mut self) -> (String, usize) {
        let text = match self.curator_workspace.instruction_scope {
            CuratorInstructionScope::Transaction => self.curator_transaction_instructions.as_str(),
            CuratorInstructionScope::Range => self
                .staged_ranges
                .get(self.curator_workspace.range_cursor)
                .and_then(|range| {
                    self.curator_range_instructions
                        .get(&canonical_editor_range_key(&range.requested))
                })
                .map(String::as_str)
                .unwrap_or(""),
        };
        self.curator_workspace.instruction_editor.clamp(text);
        (
            text.to_string(),
            self.curator_workspace.instruction_editor.cursor,
        )
    }

    pub(super) fn total_curator_instruction_chars(&self) -> usize {
        self.curator_transaction_instructions.chars().count()
            + self
                .curator_range_instructions
                .values()
                .map(|instructions| instructions.chars().count())
                .sum::<usize>()
    }

    pub(super) fn curator_review_detail_lines(&self) -> Vec<Line<'static>> {
        let Some(draft) = self.draft.as_ref() else {
            return vec![Line::from("The generated review is unavailable.")];
        };
        let mut cursor = self.curator_workspace.review_cursor;
        if cursor == 0 {
            let economics = &self
                .selection_preview
                .as_ref()
                .map(|preview| &preview.preview.economics)
                .unwrap_or(&draft.preview.economics);
            let mut lines = vec![
                Line::from(Span::styled(
                    "Atomic transaction overview",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(format!(
                    "Revision {} → {} · raw stored messages unchanged: {}",
                    draft.preview.current_context_revision,
                    draft.preview.proposed_context_revision,
                    draft.preview.raw_stored_message_count
                )),
                Line::from(Span::styled(
                    "Applying creates one context revision and one intentional cache transition.",
                    Style::default().fg(Color::Yellow),
                )),
            ];
            push_economics_lines(&mut lines, economics);
            push_validation_lines(
                &mut lines,
                &draft.preview.validation,
                draft.preview.formatter_placeholder_count,
            );
            if !draft.preview.notices.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Notices",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )));
                for notice in &draft.preview.notices {
                    push_workspace_multiline(&mut lines, notice, "(empty notice)");
                }
            }
            return lines;
        }
        cursor -= 1;
        if let Some(operation) = draft.required_operations.get(cursor) {
            return required_operation_detail_lines(operation);
        }
        cursor = cursor.saturating_sub(draft.required_operations.len());
        if let Some(proposal) = draft.distillation_proposals.get(cursor) {
            let selected = self
                .selected_distillation_ids
                .contains(&proposal.proposal_id);
            let operation = &proposal.operation;
            let mut lines = vec![
                Line::from(Span::styled(
                    format!(
                        "{} Eligible distillation",
                        if selected { "[x]" } else { "[ ]" }
                    ),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(format!(
                    "{} · call {}",
                    operation.tool_name, operation.tool_call_id
                )),
                Line::from(format!(
                    "{} → {} tokens · {:.2}% of original",
                    operation.original_token_estimate,
                    operation.replacement_token_estimate,
                    operation.replacement_ratio_millionths as f64 / 10_000.0
                )),
                Line::from(""),
            ];
            push_workspace_labeled_multiline(
                &mut lines,
                "Replacement",
                &operation.replacement_content,
            );
            lines.push(Line::from(""));
            push_workspace_labeled_multiline(
                &mut lines,
                "Preservation rationale",
                &operation.preservation_rationale,
            );
            for uncertainty in &operation.uncertainties {
                push_workspace_labeled_multiline(&mut lines, "Uncertainty", uncertainty);
            }
            lines.push(Line::from(""));
            push_generator_detail(&mut lines, &operation.generator);
            return lines;
        }
        cursor = cursor.saturating_sub(draft.distillation_proposals.len());
        if let Some(ineligible) = draft.ineligible_distillations.get(cursor) {
            let mut lines = vec![
                Line::from(Span::styled(
                    "Ineligible output",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(format!(
                    "{} · call {}",
                    ineligible.tool_name, ineligible.tool_call_id
                )),
                Line::from(""),
            ];
            push_workspace_labeled_multiline(&mut lines, "Reason", &ineligible.reason);
            for uncertainty in &ineligible.uncertainties {
                push_workspace_labeled_multiline(&mut lines, "Uncertainty", uncertainty);
            }
            return lines;
        }
        cursor = cursor.saturating_sub(draft.ineligible_distillations.len());
        if let Some(usage) = draft.curator_usage.get(cursor) {
            return vec![
                Line::from(Span::styled(
                    "Curator call usage and provenance",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(format!(
                    "{} · {} · {} · effort {}",
                    usage.provider,
                    usage.model,
                    usage.route,
                    usage.effort.as_deref().unwrap_or("provider default")
                )),
                Line::from(format!(
                    "Role {} · artifact {} · prompt {}",
                    usage.role.map(curator_role_label).unwrap_or("unknown"),
                    usage.artifact_id.as_deref().unwrap_or("unknown"),
                    usage.prompt_version.as_deref().unwrap_or("unknown")
                )),
                Line::from(format!(
                    "Input {} · output {} · cache read {} · cache creation {}",
                    usage.input_tokens,
                    usage.output_tokens,
                    usage
                        .cache_read_input_tokens
                        .map_or_else(|| "unknown".to_string(), |value| value.to_string()),
                    usage
                        .cache_creation_input_tokens
                        .map_or_else(|| "unknown".to_string(), |value| value.to_string())
                )),
                Line::from(format!(
                    "Exact cost: {}",
                    usage.cost_usd.map_or_else(
                        || "unknown or not metered".to_string(),
                        |cost| format!("${cost:.6}")
                    )
                )),
            ];
        }
        vec![Line::from("No review item selected.")]
    }
}

fn workspace_focus_style(focused: bool) -> Style {
    Style::default().fg(if focused {
        Color::Rgb(130, 130, 170)
    } else {
        Color::Rgb(65, 65, 80)
    })
}

fn curator_badge_style() -> Style {
    Style::default()
        .fg(Color::Rgb(186, 139, 255))
        .add_modifier(Modifier::BOLD)
}

fn curator_status_style(section: CuratorWorkspaceSection, editor: &ContextEditor) -> Style {
    match section {
        CuratorWorkspaceSection::ExactCalls if editor.curator_plan_pending => {
            Style::default().fg(Color::Rgb(255, 193, 7))
        }
        CuratorWorkspaceSection::ExactCalls if editor.curator_plan_current() => {
            Style::default().fg(Color::Rgb(140, 220, 170))
        }
        CuratorWorkspaceSection::Route if editor.curator_selection.is_some() => {
            Style::default().fg(Color::Rgb(255, 193, 7))
        }
        _ => Style::default().fg(Color::DarkGray),
    }
}

fn instruction_tab_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(Color::White)
            .bg(Color::Rgb(50, 45, 70))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn curator_option_matches_selection(
    option: &ContextCuratorRouteOption,
    selection: Option<&ContextCuratorSelection>,
) -> bool {
    selection.is_some_and(|selection| {
        selection.provider.as_deref() == Some(option.provider.as_str())
            && selection.route.as_deref() == Some(option.route.as_str())
            && selection.model.as_deref() == Some(option.model.as_str())
    })
}

fn curator_option_matches_resolved_route(
    option: &ContextCuratorRouteOption,
    route: &ContextCuratorRoutePreview,
) -> bool {
    option.provider.eq_ignore_ascii_case(&route.provider_name)
        && option.route == route.route
        && option.model == route.model
}

fn curator_role_label(role: StoredContextCuratorRole) -> &'static str {
    match role {
        StoredContextCuratorRole::RangeSummarizer => "Range summary",
        StoredContextCuratorRole::ToolResultDistiller => "Tool-result distillation",
    }
}

fn curator_task_target_label(task: &ContextCuratorTaskPreview) -> String {
    task.target_label.replace(" (1 messages)", " (1 message)")
}

fn source_purpose_label(purpose: ContextCuratorSourcePurpose) -> &'static str {
    match purpose {
        ContextCuratorSourcePurpose::PrimaryRange => "primary range",
        ContextCuratorSourcePurpose::PrimaryToolCall => "primary tool call",
        ContextCuratorSourcePurpose::PrimaryToolResult => "primary tool result",
        ContextCuratorSourcePurpose::SupportingConversation => "supporting conversation",
        ContextCuratorSourcePurpose::ActiveSummary => "active summary",
    }
}

fn push_scope_group<'a>(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    scopes: impl Iterator<Item = &'a ContextCuratorSourceScope>,
) {
    lines.push(Line::from(label.to_string()));
    let scopes = scopes.collect::<Vec<_>>();
    if scopes.is_empty() {
        lines.push(Line::from("  none"));
        return;
    }
    for scope in scopes {
        lines.push(Line::from(format!(
            "  {} · {} · message {} · stored {}",
            source_purpose_label(scope.purpose),
            if scope.includes_all_blocks {
                "all blocks".to_string()
            } else {
                format!("blocks {:?}", scope.block_ordinals)
            },
            scope.message_id.as_deref().unwrap_or("anonymous"),
            scope
                .stored_index
                .map_or_else(|| "n/a".to_string(), |value| value.to_string()),
        )));
    }
}

fn instruction_scope_label(chars: usize) -> String {
    if chars == 0 {
        "none".to_string()
    } else {
        format!("{chars} character(s) · exact text is visible under Prompt")
    }
}

pub(super) fn curator_task_detail_lines(
    task: &ContextCuratorTaskPreview,
    plan: &ContextCuratorPlanPreview,
    detail: CuratorPlanDetail,
) -> Vec<Line<'static>> {
    match detail {
        CuratorPlanDetail::Overview => vec![
            Line::from(Span::styled(
                format!("{} · FITS", curator_role_label(task.role)),
                Style::default()
                    .fg(Color::Rgb(140, 220, 170))
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(curator_task_target_label(task)),
            Line::from(""),
            Line::from(format!(
                "Input {} / {} safe tokens",
                format_tokens(task.estimated_input_tokens),
                format_tokens(task.safe_input_budget)
            )),
            Line::from(format!(
                "Request {} / {} bytes · {} image(s)",
                task.request_bytes, task.request_byte_limit, task.image_count
            )),
            Line::from(format!(
                "Route {} · {} · {} · effort {}",
                plan.route.provider_display_name,
                plan.route.model,
                plan.route.route,
                plan.route.effort.as_deref().unwrap_or("provider default")
            )),
            Line::from(format!("Source scope entries: {}", task.source_scope.len())),
            Line::from(""),
            Line::from(Span::styled(
                "Complete source passed preflight. This task remains unapplied until the complete atomic transaction is reviewed and committed.",
                Style::default().fg(Color::DarkGray),
            )),
        ],
        CuratorPlanDetail::Prompt => {
            let mut lines = vec![
                Line::from(Span::styled(
                    "Exact effective system prompt",
                    Style::default()
                        .fg(Color::Rgb(186, 139, 255))
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ];
            push_workspace_multiline(&mut lines, &task.effective_system_prompt, "(empty prompt)");
            lines
        }
        CuratorPlanDetail::Contract => {
            let mut lines = vec![
                Line::from(Span::styled(
                    "Exact response contract",
                    Style::default()
                        .fg(Color::Rgb(186, 139, 255))
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ];
            push_workspace_multiline(&mut lines, &task.response_contract, "(empty contract)");
            lines
        }
        CuratorPlanDetail::SourceScope => {
            let mut lines = vec![
                Line::from(Span::styled(
                    "Exact source scope",
                    Style::default()
                        .fg(Color::Rgb(186, 139, 255))
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ];
            push_scope_group(
                &mut lines,
                "Authoritative primary source",
                task.source_scope.iter().filter(|scope| {
                    matches!(
                        scope.purpose,
                        ContextCuratorSourcePurpose::PrimaryRange
                            | ContextCuratorSourcePurpose::PrimaryToolCall
                            | ContextCuratorSourcePurpose::PrimaryToolResult
                    )
                }),
            );
            push_scope_group(
                &mut lines,
                "Supporting conversation",
                task.source_scope.iter().filter(|scope| {
                    scope.purpose == ContextCuratorSourcePurpose::SupportingConversation
                }),
            );
            push_scope_group(
                &mut lines,
                "Active summaries",
                task.source_scope
                    .iter()
                    .filter(|scope| scope.purpose == ContextCuratorSourcePurpose::ActiveSummary),
            );
            lines.push(Line::from(""));
            if let Some(evidence) = task.file_evidence.as_ref() {
                super::push_file_evidence_lines(&mut lines, evidence);
            } else {
                lines.push(Line::from("Harness-generated file evidence"));
                lines.push(Line::from("  none for this atomic task"));
            }
            lines.push(Line::from(""));
            lines.push(Line::from("User instructions"));
            lines.push(Line::from(format!(
                "  Transaction-wide: {}",
                instruction_scope_label(task.user_instructions.transaction_wide_chars)
            )));
            lines.push(Line::from(format!(
                "  Task-specific: {}",
                instruction_scope_label(task.user_instructions.task_specific_chars)
            )));
            lines.push(Line::from(""));
            lines.push(Line::from("Complete-source preflight"));
            lines.push(Line::from(
                "  passed for the exact source, evidence, instructions, route, and limits shown here",
            ));
            lines
        }
        CuratorPlanDetail::Integrity => vec![
            Line::from(Span::styled(
                "Plan integrity",
                Style::default()
                    .fg(Color::Rgb(186, 139, 255))
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("Jcode rebuilds this exact plan immediately before any curator call."),
            Line::from(
                "A changed route, prompt, limit, source scope, revision, or transcript digest rejects generation.",
            ),
            Line::from(""),
            Line::from(format!("SHA-256 fingerprint: {}", plan.fingerprint)),
            Line::from(format!("Session: {}", plan.session_id)),
            Line::from(format!("Context revision: {}", plan.context_revision)),
            Line::from(format!("Transcript digest: {}", plan.transcript_digest)),
        ],
    }
}

fn curator_review_labels(
    draft: &ContextDraft,
    selected_distillation_ids: &BTreeSet<String>,
) -> Vec<String> {
    let mut labels = vec!["Overview & economics".to_string()];
    for operation in &draft.required_operations {
        labels.push(match operation {
            StoredContextOperation::RangeSummary(summary) => format!(
                "Required · Range {}..{}",
                summary.source_range.start_index_hint, summary.source_range.end_index_hint
            ),
            StoredContextOperation::ReasoningSuppression(reasoning) => {
                format!("Required · Reasoning · {} blocks", reasoning.targets.len())
            }
            StoredContextOperation::ToolResultDistillation(distillation) => {
                format!("Required · {} result", distillation.tool_name)
            }
        });
    }
    for proposal in &draft.distillation_proposals {
        labels.push(format!(
            "{} Eligible · {}",
            if selected_distillation_ids.contains(&proposal.proposal_id) {
                "[x]"
            } else {
                "[ ]"
            },
            proposal.operation.tool_name
        ));
    }
    for ineligible in &draft.ineligible_distillations {
        labels.push(format!("Ineligible · {}", ineligible.tool_name));
    }
    for usage in &draft.curator_usage {
        labels.push(format!(
            "Usage · {} · {}",
            usage.role.map(curator_role_label).unwrap_or("curator"),
            usage.artifact_id.as_deref().unwrap_or("artifact")
        ));
    }
    labels
}

fn push_workspace_multiline(lines: &mut Vec<Line<'static>>, text: &str, empty: &'static str) {
    if text.is_empty() {
        lines.push(Line::from(empty));
    } else {
        lines.extend(text.split('\n').map(|line| Line::from(line.to_string())));
    }
}

fn push_workspace_labeled_multiline(
    lines: &mut Vec<Line<'static>>,
    label: &'static str,
    text: &str,
) {
    lines.push(Line::from(Span::styled(
        label,
        Style::default()
            .fg(Color::Rgb(186, 139, 255))
            .add_modifier(Modifier::BOLD),
    )));
    push_workspace_multiline(lines, text, "(none)");
}

fn push_generator_detail(
    lines: &mut Vec<Line<'static>>,
    generator: &jcode_session_types::StoredContextArtifactGenerator,
) {
    lines.push(Line::from(Span::styled(
        "Generator provenance",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!(
        "{} · {} · {} · effort {}",
        generator.provider,
        generator.model,
        generator.route,
        generator.effort.as_deref().unwrap_or("provider default")
    )));
    lines.push(Line::from(format!(
        "Role {} · prompt {} · selection {}",
        generator.role.map(curator_role_label).unwrap_or("unknown"),
        generator.prompt_version,
        generator
            .selection_source
            .map(|source| format!("{source:?}"))
            .unwrap_or_else(|| "unknown".to_string())
    )));
    push_workspace_labeled_multiline(
        lines,
        "Transaction instructions",
        generator.transaction_instructions.as_deref().unwrap_or(""),
    );
    push_workspace_labeled_multiline(
        lines,
        "Task instructions",
        generator.task_instructions.as_deref().unwrap_or(""),
    );
}

fn required_operation_detail_lines(operation: &StoredContextOperation) -> Vec<Line<'static>> {
    match operation {
        StoredContextOperation::RangeSummary(summary) => {
            let mut lines = vec![
                Line::from(Span::styled(
                    "Required range summary",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(format!(
                    "Messages {}..{} · {} message(s)",
                    summary.source_range.start_index_hint,
                    summary.source_range.end_index_hint,
                    summary.source_range.message_count
                )),
                Line::from(format!(
                    "{} → {} tokens",
                    summary.source_token_estimate, summary.replacement_token_estimate
                )),
                Line::from(""),
            ];
            push_workspace_labeled_multiline(&mut lines, "Summary", &summary.summary_text);
            lines.push(Line::from(""));
            push_workspace_labeled_multiline(
                &mut lines,
                "File-change digest",
                &summary.file_change_digest,
            );
            super::push_file_evidence_lines(&mut lines, &summary.effective_file_evidence());
            for warning in &summary.warnings {
                push_workspace_labeled_multiline(&mut lines, "Warning", warning);
            }
            if let Some(generator) = summary.generator.as_ref() {
                lines.push(Line::from(""));
                push_generator_detail(&mut lines, generator);
            } else {
                lines.push(Line::from("Generator provenance unavailable."));
            }
            lines
        }
        StoredContextOperation::ReasoningSuppression(reasoning) => {
            let selection = match &reasoning.selection {
                StoredReasoningSelection::KeepLatestAssistantTurns {
                    protected_recent_assistant_turns,
                } => format!("Protect latest {protected_recent_assistant_turns} assistant turns"),
                StoredReasoningSelection::MessageRanges { ranges } => {
                    format!("{} manual message range(s)", ranges.len())
                }
            };
            let mut lines = vec![
                Line::from(Span::styled(
                    "Required reasoning suppression",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(selection),
                Line::from(format!(
                    "{} exact block target(s) across {} assistant turn(s)",
                    reasoning.targets.len(),
                    reasoning.assistant_turns_affected
                )),
                Line::from(format!(
                    "Replay block kinds: {:?}",
                    reasoning.replay_block_kinds
                )),
                Line::from(format!(
                    "Estimated tokens removed: {}",
                    reasoning.original_token_estimate
                )),
                Line::from(format!(
                    "Provider validation evidence version: {}",
                    reasoning.validation_evidence_version
                )),
            ];
            for evidence in &reasoning.validation {
                lines.push(Line::from(format!(
                    "{} · {} · {} · {:?}",
                    evidence.provider, evidence.model, evidence.request_builder, evidence.outcome
                )));
                for warning in &evidence.warnings {
                    push_workspace_labeled_multiline(&mut lines, "Provider warning", warning);
                }
            }
            lines
        }
        StoredContextOperation::ToolResultDistillation(distillation) => {
            let mut lines = vec![
                Line::from(Span::styled(
                    "Required tool-result distillation",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(format!(
                    "{} · call {}",
                    distillation.tool_name, distillation.tool_call_id
                )),
                Line::from(format!(
                    "{} → {} tokens",
                    distillation.original_token_estimate, distillation.replacement_token_estimate
                )),
                Line::from(""),
            ];
            push_workspace_labeled_multiline(
                &mut lines,
                "Replacement",
                &distillation.replacement_content,
            );
            lines.push(Line::from(""));
            push_workspace_labeled_multiline(
                &mut lines,
                "Preservation rationale",
                &distillation.preservation_rationale,
            );
            for uncertainty in &distillation.uncertainties {
                push_workspace_labeled_multiline(&mut lines, "Uncertainty", uncertainty);
            }
            lines.push(Line::from(""));
            push_generator_detail(&mut lines, &distillation.generator);
            lines
        }
    }
}

fn draft_phase_label(phase: ContextDraftPhase) -> &'static str {
    match phase {
        ContextDraftPhase::Capturing => "capturing exact authoritative state",
        ContextDraftPhase::ClosingRanges => "closing structural ranges",
        ContextDraftPhase::ExtractingChangeEvidence => "extracting change evidence",
        ContextDraftPhase::PreparingArtifacts => "running isolated curator calls",
        ContextDraftPhase::ValidatingProjection => "validating projected provider context",
        ContextDraftPhase::CalculatingEconomics => "calculating exact economics",
        ContextDraftPhase::Ready => "ready for explicit review",
    }
}

fn text_with_cursor(text: &str, cursor: usize) -> String {
    let mut rendered = String::with_capacity(text.len() + 3);
    let cursor = cursor.min(text.len());
    rendered.push_str(&text[..cursor]);
    rendered.push('▎');
    rendered.push_str(&text[cursor..]);
    rendered
}

fn edit_curator_text(
    text: &mut String,
    state: &mut CuratorTextEditorState,
    code: KeyCode,
    modifiers: KeyModifiers,
    max_chars: usize,
) -> bool {
    state.clamp(text);
    let control = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Left => {
            state.cursor = previous_char_boundary(text, state.cursor);
            state.preferred_column = None;
            false
        }
        KeyCode::Right => {
            state.cursor = next_char_boundary(text, state.cursor);
            state.preferred_column = None;
            false
        }
        KeyCode::Home => {
            state.cursor = line_start(text, state.cursor);
            state.preferred_column = None;
            false
        }
        KeyCode::End => {
            state.cursor = line_end(text, state.cursor);
            state.preferred_column = None;
            false
        }
        KeyCode::Up => {
            move_text_cursor_vertical(text, state, -1);
            false
        }
        KeyCode::Down => {
            move_text_cursor_vertical(text, state, 1);
            false
        }
        KeyCode::PageUp => {
            state.scroll = state.scroll.saturating_sub(5);
            false
        }
        KeyCode::PageDown => {
            state.scroll = state.scroll.saturating_add(5);
            false
        }
        KeyCode::Backspace if control => {
            let start = previous_word_boundary(text, state.cursor);
            if start == state.cursor {
                return false;
            }
            text.replace_range(start..state.cursor, "");
            state.cursor = start;
            state.preferred_column = None;
            true
        }
        KeyCode::Backspace => {
            let start = previous_char_boundary(text, state.cursor);
            if start == state.cursor {
                return false;
            }
            text.replace_range(start..state.cursor, "");
            state.cursor = start;
            state.preferred_column = None;
            true
        }
        KeyCode::Delete => {
            let end = next_char_boundary(text, state.cursor);
            if end == state.cursor {
                return false;
            }
            text.replace_range(state.cursor..end, "");
            state.preferred_column = None;
            true
        }
        KeyCode::Enter => insert_curator_text(text, state, "\n", max_chars),
        KeyCode::Char('u') if control => {
            let start = line_start(text, state.cursor);
            if start == state.cursor {
                return false;
            }
            text.replace_range(start..state.cursor, "");
            state.cursor = start;
            state.preferred_column = None;
            true
        }
        KeyCode::Char(character) if !control => {
            let mut buffer = [0u8; 4];
            insert_curator_text(text, state, character.encode_utf8(&mut buffer), max_chars)
        }
        _ => false,
    }
}

fn insert_curator_text(
    text: &mut String,
    state: &mut CuratorTextEditorState,
    inserted: &str,
    max_chars: usize,
) -> bool {
    if text
        .chars()
        .count()
        .saturating_add(inserted.chars().count())
        > max_chars
    {
        return false;
    }
    text.insert_str(state.cursor, inserted);
    state.cursor += inserted.len();
    state.preferred_column = None;
    true
}

fn previous_char_boundary(text: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }
    let mut index = cursor.saturating_sub(1);
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn next_char_boundary(text: &str, cursor: usize) -> usize {
    if cursor >= text.len() {
        return text.len();
    }
    let mut index = cursor + 1;
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn line_start(text: &str, cursor: usize) -> usize {
    text[..cursor.min(text.len())]
        .rfind('\n')
        .map_or(0, |index| index + 1)
}

fn line_end(text: &str, cursor: usize) -> usize {
    let cursor = cursor.min(text.len());
    text[cursor..]
        .find('\n')
        .map_or(text.len(), |offset| cursor + offset)
}

fn previous_word_boundary(text: &str, cursor: usize) -> usize {
    let mut index = cursor.min(text.len());
    while index > 0 {
        let previous = previous_char_boundary(text, index);
        let character = text[previous..index].chars().next().unwrap_or(' ');
        if !character.is_whitespace() {
            break;
        }
        index = previous;
    }
    while index > 0 {
        let previous = previous_char_boundary(text, index);
        let character = text[previous..index].chars().next().unwrap_or(' ');
        if character.is_whitespace() {
            break;
        }
        index = previous;
    }
    index
}

fn move_text_cursor_vertical(text: &str, state: &mut CuratorTextEditorState, direction: isize) {
    let current_start = line_start(text, state.cursor);
    let current_column = text[current_start..state.cursor].chars().count();
    let desired = state.preferred_column.unwrap_or(current_column);
    state.preferred_column = Some(desired);
    let target_start = if direction < 0 {
        if current_start == 0 {
            return;
        }
        line_start(text, current_start.saturating_sub(1))
    } else {
        let current_end = line_end(text, state.cursor);
        if current_end >= text.len() {
            return;
        }
        current_end + 1
    };
    let target_end = line_end(text, target_start);
    state.cursor = text[target_start..target_end]
        .char_indices()
        .nth(desired)
        .map_or(target_end, |(offset, _)| target_start + offset);
}
