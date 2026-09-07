use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FieldKey {
    Id,
    Name,
    Description,
    Kind,
    Template,
    Availability,
    Target,
    DefaultAgent,
    Include,
    Alias,
    Candidate,
    Effort,
    Notes,
    RepositoryMode,
    RepositoryPath,
    RepositoryUrl,
    Branch,
    Remote,
    Start,
    LocalBranch,
}
#[derive(Clone)]
pub(super) struct Field {
    pub key: FieldKey,
    pub label: String,
    pub value: String,
    pub cursor: usize,
    pub options: Vec<String>,
}
impl Field {
    fn closed(&self) -> bool {
        matches!(
            self.key,
            FieldKey::Kind
                | FieldKey::Template
                | FieldKey::Availability
                | FieldKey::Effort
                | FieldKey::RepositoryMode
        )
    }
    fn text(key: FieldKey, label: &str, value: String) -> Self {
        let cursor = value.len();
        Self {
            key,
            label: label.into(),
            value,
            cursor,
            options: Vec::new(),
        }
    }
    fn choice(key: FieldKey, label: &str, value: String, mut options: Vec<String>) -> Self {
        if !options.contains(&value) {
            options.insert(0, value.clone());
        }
        let mut field = Self::text(key, label, value);
        field.options = options;
        field
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FormButton {
    Submit,
    AddReference,
    AddCandidate,
    RemoveItem,
    Earlier,
    Later,
    AddAlias,
    DeleteAlias,
}
#[derive(Clone)]
enum Purpose {
    Action(EditAction),
    Resource(InstructionResourceFields),
    Settings,
    Roster(Vec<InstructionRosterFields>, usize),
    Repository(Box<InstructionRepositoryChoices>, RepositoryMode),
}
pub(super) struct Picker {
    pub values: Vec<String>,
    pub query: String,
    pub selected: usize,
}
impl Picker {
    pub fn matching(&self) -> Vec<&String> {
        let query = self.query.to_lowercase();
        self.values
            .iter()
            .filter(|value| value.to_lowercase().contains(&query))
            .collect()
    }
}
pub(super) struct EditForm {
    pub title: String,
    pub fields: Vec<Field>,
    pub selected: usize,
    pub picker: Option<Picker>,
    pub error: String,
    pub buttons: Vec<FormButton>,
    pub offset: usize,
    purpose: Purpose,
    choices: InstructionEditChoices,
    last_field: usize,
}
impl EditForm {
    pub fn is_repository(&self) -> bool {
        matches!(self.purpose, Purpose::Repository(_, _))
    }
    pub fn focus(&mut self, index: usize) {
        self.selected = index;
        if index < self.fields.len() {
            self.last_field = index;
        }
    }
    pub fn start(action: EditAction, draft: Option<&InstructionEditDraft>) -> Self {
        let choices = draft.map(|draft| draft.choices.clone()).unwrap_or_default();
        let mut form = Self {
            title: match action {
                EditAction::Rename => "Rename resource",
                EditAction::Addendum => "Create project addendum",
                EditAction::CreateProject => "Create project resource",
                _ => "Create global resource",
            }
            .into(),
            fields: vec![Field::text(
                FieldKey::Id,
                "Stable ID (required)",
                String::new(),
            )],
            selected: 0,
            picker: None,
            error: String::new(),
            buttons: vec![FormButton::Submit],
            offset: 0,
            purpose: Purpose::Action(action),
            choices,
            last_field: 0,
        };
        if matches!(action, EditAction::CreateGlobal | EditAction::CreateProject) {
            form.fields.push(Field::choice(
                FieldKey::Kind,
                "Resource type",
                "module".into(),
                [
                    "module",
                    "agent",
                    "agent-addendum",
                    "system",
                    "notification",
                    "tool-guidance",
                    "skill",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ));
            form.fields
                .push(Field::text(FieldKey::Name, "Display name", String::new()));
            form.fields.push(Field::text(
                FieldKey::Description,
                "Description (agents/skills)",
                String::new(),
            ));
            form.fields.push(Field::choice(
                FieldKey::Template,
                "Template",
                "plain".into(),
                vec!["plain".into(), "handlebars".into()],
            ));
            form.fields.push(Field::choice(
                FieldKey::Availability,
                "Availability (agents)",
                "both".into(),
                vec!["primary".into(), "isolated".into(), "both".into()],
            ));
            form.fields.push(Field::choice(
                FieldKey::Target,
                "Target (addenda)",
                String::new(),
                form.choices.agents.clone(),
            ));
        }
        form
    }
    pub fn metadata(file: &InstructionEditFile, choices: &InstructionEditChoices) -> Self {
        let mut form = Self {
            title: format!("Metadata: {}", file.path),
            fields: Vec::new(),
            selected: 0,
            picker: None,
            error: String::new(),
            buttons: vec![FormButton::Submit],
            offset: 0,
            purpose: Purpose::Settings,
            choices: choices.clone(),
            last_field: 0,
        };
        match &file.metadata {
            InstructionEditMetadata::Resource(metadata) => {
                form.purpose = Purpose::Resource(metadata.clone());
                form.fields.push(Field::text(
                    FieldKey::Name,
                    "Display name",
                    metadata.name.clone().unwrap_or_default(),
                ));
                form.fields.push(Field::text(
                    FieldKey::Description,
                    "Description",
                    metadata.description.clone().unwrap_or_default(),
                ));
                form.fields.push(Field::choice(
                    FieldKey::Template,
                    "Template",
                    if metadata.template == InstructionEditTemplate::Plain {
                        "plain"
                    } else {
                        "handlebars"
                    }
                    .into(),
                    vec!["plain".into(), "handlebars".into()],
                ));
                if let Some(value) = metadata.availability {
                    form.fields.push(Field::choice(
                        FieldKey::Availability,
                        "Agent availability",
                        availability(value).into(),
                        vec!["primary".into(), "isolated".into(), "both".into()],
                    ));
                }
                if let Some(value) = &metadata.target {
                    form.fields.push(Field::choice(
                        FieldKey::Target,
                        "Addendum target",
                        value.clone(),
                        choices.agents.clone(),
                    ));
                }
                for value in &metadata.includes {
                    form.fields.push(Field::choice(
                        FieldKey::Include,
                        "Included module",
                        value.clone(),
                        choices.modules.clone(),
                    ));
                }
                form.buttons.extend([
                    FormButton::AddReference,
                    FormButton::RemoveItem,
                    FormButton::Earlier,
                    FormButton::Later,
                ]);
            }
            InstructionEditMetadata::StoreSettings { default_agent } => {
                let mut options = vec![String::new()];
                options.extend(choices.agents.clone());
                form.fields.push(Field::choice(
                    FieldKey::DefaultAgent,
                    "Default agent (empty: inherit)",
                    default_agent.clone().unwrap_or_default(),
                    options,
                ));
            }
            InstructionEditMetadata::Roster(entries) => {
                form.purpose = Purpose::Roster(entries.clone(), 0);
                form.load_alias(0);
            }
            _ => {
                form.error = "This source needs explicit external-editor repair rather than ordinary metadata editing.".into();
            }
        }
        form
    }
    fn load_alias(&mut self, index: usize) {
        let Purpose::Roster(entries, selected) = &mut self.purpose else {
            return;
        };
        *selected = index.min(entries.len().saturating_sub(1));
        self.fields.clear();
        self.buttons = vec![
            FormButton::Submit,
            FormButton::AddCandidate,
            FormButton::RemoveItem,
            FormButton::Earlier,
            FormButton::Later,
            FormButton::AddAlias,
            FormButton::DeleteAlias,
        ];
        if let Some(entry) = entries.get(*selected) {
            self.fields.push(Field::choice(
                FieldKey::Alias,
                "Alias (choose to inspect another)",
                entry.alias.clone(),
                entries.iter().map(|entry| entry.alias.clone()).collect(),
            ));
            self.fields.push(Field::text(
                FieldKey::Description,
                "Task description (required)",
                entry.description.clone(),
            ));
            self.fields.push(Field::choice(
                FieldKey::Effort,
                "Default effort (empty: provider)",
                entry.effort.clone().unwrap_or_default(),
                [
                    "", "none", "minimal", "low", "medium", "high", "xhigh", "max",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ));
            self.fields.push(Field::text(
                FieldKey::Notes,
                "Human-only notes",
                entry.notes.clone().unwrap_or_default(),
            ));
            for model in &entry.candidates {
                self.fields.push(Field::choice(
                    FieldKey::Candidate,
                    "Candidate route/model (ordered)",
                    model.clone(),
                    self.choices.models.clone(),
                ));
            }
        }
        self.selected = 0;
        self.last_field = 0;
    }
    fn stash_alias(&mut self) {
        let alias = self.value(FieldKey::Alias);
        let description = self.value(FieldKey::Description);
        let effort = optional(self.value(FieldKey::Effort));
        let notes = optional(self.value(FieldKey::Notes));
        let candidates = self
            .fields
            .iter()
            .filter(|field| field.key == FieldKey::Candidate)
            .map(|field| field.value.clone())
            .collect();
        if let Purpose::Roster(entries, selected) = &mut self.purpose
            && let Some(entry) = entries.get_mut(*selected)
        {
            *entry = InstructionRosterFields {
                alias,
                description,
                effort,
                notes,
                candidates,
            };
        }
    }
    pub fn paste(&mut self, text: &str) {
        if let Some(picker) = &mut self.picker {
            picker.query.push_str(text);
            picker.selected = 0;
            return;
        }
        if let Some(field) = self.fields.get_mut(self.selected) {
            if field.closed() {
                self.picker = Some(Picker {
                    values: field.options.clone(),
                    query: text.into(),
                    selected: 0,
                });
                return;
            }
            field.value.insert_str(field.cursor, text);
            field.cursor += text.len();
            self.error.clear();
        }
    }
    fn value(&self, key: FieldKey) -> String {
        self.fields
            .iter()
            .find(|field| field.key == key)
            .map(|field| field.value.clone())
            .unwrap_or_default()
    }
    fn resource_fields(
        &self,
        original: Option<&InstructionResourceFields>,
    ) -> InstructionResourceFields {
        let kind = original.map(|value| value.kind).unwrap_or_else(|| {
            match self.value(FieldKey::Kind).as_str() {
                "agent" => InstructionEditKind::Agent,
                "agent-addendum" => InstructionEditKind::AgentAddendum,
                "system" => InstructionEditKind::System,
                "notification" => InstructionEditKind::Notification,
                "tool-guidance" => InstructionEditKind::ToolGuidance,
                "skill" => InstructionEditKind::Skill,
                _ => InstructionEditKind::Module,
            }
        });
        InstructionResourceFields {
            id: original
                .map(|value| value.id.clone())
                .unwrap_or_else(|| self.value(FieldKey::Id)),
            kind,
            name: optional(self.value(FieldKey::Name)),
            description: optional(self.value(FieldKey::Description)),
            template: if self.value(FieldKey::Template) == "handlebars" {
                InstructionEditTemplate::Handlebars
            } else {
                InstructionEditTemplate::Plain
            },
            availability: (kind == InstructionEditKind::Agent).then(|| {
                match self.value(FieldKey::Availability).as_str() {
                    "primary" => InstructionEditAvailability::Primary,
                    "isolated" => InstructionEditAvailability::Isolated,
                    _ => InstructionEditAvailability::Both,
                }
            }),
            target: (kind == InstructionEditKind::AgentAddendum)
                .then(|| self.value(FieldKey::Target)),
            includes: self
                .fields
                .iter()
                .filter(|field| field.key == FieldKey::Include)
                .map(|field| field.value.clone())
                .collect(),
            allowed_tools: original.and_then(|value| value.allowed_tools.clone()),
        }
    }
    fn submit(&mut self) -> Result<FormResult, String> {
        for field in &self.fields {
            if field.closed() && !field.options.contains(&field.value) {
                return Err(format!("Choose a supported value for {}.", field.label));
            }
        }
        self.stash_alias();
        match &self.purpose {
            Purpose::Action(EditAction::CreateGlobal | EditAction::CreateProject) => {
                let fields = self.resource_fields(None);
                if fields.id.is_empty() {
                    return Err("Enter a stable resource ID.".into());
                }
                if matches!(
                    fields.kind,
                    InstructionEditKind::Agent | InstructionEditKind::Skill
                ) && (fields.name.is_none() || fields.description.is_none())
                {
                    return Err("Agents and skills need a name and description.".into());
                }
                Ok(FormResult::Begin(InstructionEditAction::Create {
                    scope: if matches!(self.purpose, Purpose::Action(EditAction::CreateProject)) {
                        InstructionEditScope::Project
                    } else {
                        InstructionEditScope::Global
                    },
                    fields,
                }))
            }
            Purpose::Action(EditAction::Rename) => {
                Ok(FormResult::Begin(InstructionEditAction::Rename {
                    id: self.value(FieldKey::Id),
                }))
            }
            Purpose::Action(EditAction::Addendum) => {
                Ok(FormResult::Begin(InstructionEditAction::Addendum {
                    id: self.value(FieldKey::Id),
                }))
            }
            Purpose::Resource(original) => Ok(FormResult::Metadata(
                InstructionEditMetadata::Resource(self.resource_fields(Some(original))),
            )),
            Purpose::Settings => Ok(FormResult::Metadata(
                InstructionEditMetadata::StoreSettings {
                    default_agent: optional(self.value(FieldKey::DefaultAgent)),
                },
            )),
            Purpose::Repository(choices, mode) => Ok(FormResult::Repository(
                choices.scope,
                self.repository_action(*mode),
            )),
            Purpose::Roster(entries, _) => Ok(FormResult::Metadata(
                InstructionEditMetadata::Roster(entries.clone()),
            )),
            _ => Err("Form action is unavailable.".into()),
        }
    }
    fn button(&mut self, button: FormButton) -> Option<Result<FormResult, String>> {
        match button {
            FormButton::Submit => return Some(self.submit()),
            FormButton::AddReference => {
                self.fields.push(Field::choice(
                    FieldKey::Include,
                    "Included module (required)",
                    String::new(),
                    self.choices.modules.clone(),
                ));
                self.selected = self.fields.len() - 1;
            }
            FormButton::AddCandidate => {
                self.fields.push(Field::choice(
                    FieldKey::Candidate,
                    "Candidate route/model (required)",
                    String::new(),
                    self.choices.models.clone(),
                ));
                self.selected = self.fields.len() - 1;
            }
            FormButton::RemoveItem => {
                if self.fields.get(self.last_field).is_some_and(|field| {
                    matches!(field.key, FieldKey::Include | FieldKey::Candidate)
                }) {
                    self.fields.remove(self.last_field);
                    self.selected = self.last_field.min(self.fields.len());
                } else {
                    self.error = "Select an included module or candidate first.".into();
                }
            }
            FormButton::Earlier | FormButton::Later => {
                let i = self.last_field;
                let j = if button == FormButton::Earlier {
                    i.saturating_sub(1)
                } else {
                    i.saturating_add(1)
                };
                if i < self.fields.len()
                    && j < self.fields.len()
                    && self.fields[i].key == self.fields[j].key
                    && matches!(self.fields[i].key, FieldKey::Candidate | FieldKey::Include)
                {
                    self.fields.swap(i, j);
                    self.selected = j;
                    self.last_field = j;
                }
            }
            FormButton::AddAlias => {
                self.stash_alias();
                if let Purpose::Roster(entries, _) = &mut self.purpose {
                    let index = entries.len();
                    entries.push(InstructionRosterFields {
                        alias: String::new(),
                        description: String::new(),
                        candidates: Vec::new(),
                        effort: None,
                        notes: None,
                    });
                    self.load_alias(index);
                    if let Some(field) = self.fields.first_mut() {
                        field.options.clear();
                        field.label = "New alias ID (required)".into();
                    }
                }
            }
            FormButton::DeleteAlias => {
                if let Purpose::Roster(entries, index) = &mut self.purpose {
                    if !entries.is_empty() {
                        entries.remove(*index);
                    }
                    self.load_alias(0);
                    self.error =
                        "Alias removed from this draft. Review the diff before committing.".into();
                }
            }
        }
        None
    }
    fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> FormEvent {
        if let Some(picker) = &mut self.picker {
            let count = picker.matching().len();
            match code {
                KeyCode::Esc => self.picker = None,
                KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
                KeyCode::Down => {
                    picker.selected = (picker.selected + 1).min(count.saturating_sub(1))
                }
                KeyCode::Backspace => {
                    picker.query.pop();
                    picker.selected = 0;
                }
                KeyCode::Char(character) => {
                    picker.query.push(character);
                    picker.selected = 0;
                }
                KeyCode::Enter => {
                    let selected = picker
                        .matching()
                        .get(picker.selected)
                        .map(|value| (*value).clone());
                    self.picker = None;
                    if let Some(value) = selected {
                        if self
                            .fields
                            .get(self.selected)
                            .is_some_and(|field| field.key == FieldKey::RepositoryMode)
                        {
                            if let Some(mode) = RepositoryMode::all()
                                .into_iter()
                                .find(|mode| mode.label() == value)
                            {
                                self.load_repository_mode(mode);
                            }
                            return FormEvent::Continue;
                        }
                        if self
                            .fields
                            .get(self.selected)
                            .is_some_and(|field| field.key == FieldKey::Alias)
                        {
                            self.stash_alias();
                            if let Purpose::Roster(entries, _) = &self.purpose
                                && let Some(index) =
                                    entries.iter().position(|entry| entry.alias == value)
                            {
                                self.load_alias(index);
                            }
                        } else if let Some(field) = self.fields.get_mut(self.selected) {
                            field.value = value;
                            field.cursor = field.value.len();
                        }
                    }
                }
                _ => {}
            }
            return FormEvent::Continue;
        }
        if code == KeyCode::Esc {
            return FormEvent::Cancel;
        }
        let count = self.fields.len() + self.buttons.len();
        match code {
            KeyCode::Tab | KeyCode::Down => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::BackTab | KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Enter if self.selected >= self.fields.len() => {
                if let Some(button) = self.buttons.get(self.selected - self.fields.len()).copied()
                    && let Some(result) = self.button(button)
                {
                    return FormEvent::Submit(result);
                }
            }
            KeyCode::Enter => {
                let field = &self.fields[self.selected];
                if !field.options.is_empty() {
                    self.picker = Some(Picker {
                        values: field.options.clone(),
                        query: String::new(),
                        selected: field
                            .options
                            .iter()
                            .position(|value| value == &field.value)
                            .unwrap_or(0),
                    });
                } else {
                    self.selected = (self.selected + 1).min(count.saturating_sub(1));
                }
            }
            _ => {
                if let Some(field) = self.fields.get_mut(self.selected) {
                    if field.closed() {
                        if let KeyCode::Char(character) = code {
                            self.picker = Some(Picker {
                                values: field.options.clone(),
                                query: character.to_string(),
                                selected: 0,
                            });
                        }
                        return FormEvent::Continue;
                    }
                    match code {
                        KeyCode::Left => {
                            field.cursor = field.value[..field.cursor]
                                .char_indices()
                                .last()
                                .map_or(0, |(index, _)| index)
                        }
                        KeyCode::Right => {
                            field.cursor = field.value[field.cursor..]
                                .chars()
                                .next()
                                .map_or(field.cursor, |character| {
                                    field.cursor + character.len_utf8()
                                })
                        }
                        KeyCode::Home => field.cursor = 0,
                        KeyCode::End => field.cursor = field.value.len(),
                        KeyCode::Backspace => {
                            if let Some((index, _)) =
                                field.value[..field.cursor].char_indices().last()
                            {
                                field.value.replace_range(index..field.cursor, "");
                                field.cursor = index;
                            }
                        }
                        KeyCode::Delete => {
                            if let Some(character) = field.value[field.cursor..].chars().next() {
                                field.value.replace_range(
                                    field.cursor..field.cursor + character.len_utf8(),
                                    "",
                                );
                            }
                        }
                        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                            field.value.clear();
                            field.cursor = 0;
                        }
                        KeyCode::Char(character)
                            if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                        {
                            field.value.insert(field.cursor, character);
                            field.cursor += character.len_utf8();
                        }
                        _ => {}
                    }
                    self.error.clear();
                }
            }
        }
        if self.selected < self.fields.len() {
            self.last_field = self.selected;
        }
        FormEvent::Continue
    }
}
fn optional(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}
fn availability(value: InstructionEditAvailability) -> &'static str {
    match value {
        InstructionEditAvailability::Primary => "primary",
        InstructionEditAvailability::Isolated => "isolated",
        InstructionEditAvailability::Both => "both",
    }
}
enum FormResult {
    Repository(InstructionEditScope, InstructionRepositoryAction),
    Begin(InstructionEditAction),
    Metadata(InstructionEditMetadata),
}
enum FormEvent {
    Continue,
    Cancel,
    Submit(Result<FormResult, String>),
}
impl InstructionManager {
    pub(super) fn edit_form_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        if self.editing.busy() {
            return;
        }
        let event = self
            .editing
            .form
            .as_mut()
            .map(|form| form.key(code, modifiers));
        match event {
            Some(FormEvent::Cancel) => {
                self.editing.form = None;
                if self.editing.draft.is_none() {
                    self.editing.visible = false;
                }
            }
            Some(FormEvent::Submit(Ok(result))) => {
                self.editing.submitted_form = self.editing.form.take();
                match result {
                    FormResult::Repository(scope, action) => {
                        self.editing.queued =
                            Some(InstructionManagementRequest::PlanRepository { scope, action });
                    }
                    FormResult::Begin(action) => self.begin_edit(action),
                    FormResult::Metadata(metadata) => {
                        if let Some(draft) = &self.editing.draft
                            && let Some(file) = draft.files.get(self.editing.file_index)
                        {
                            self.editing.queued = Some(InstructionManagementRequest::Update {
                                draft: draft.id.clone(),
                                generation: draft.generation,
                                change: InstructionDraftChange::Metadata {
                                    file: file.key.clone(),
                                    metadata,
                                },
                            });
                        }
                    }
                }
            }
            Some(FormEvent::Submit(Err(error))) => {
                if let Some(form) = &mut self.editing.form {
                    form.error = error;
                }
            }
            _ => {}
        }
    }
}

include!("repository_forms.rs");
