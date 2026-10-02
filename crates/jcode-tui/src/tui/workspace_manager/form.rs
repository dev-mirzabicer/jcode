//! Typed form state for the workspace manager. Values are drafts until a
//! domain request is built from them; nothing here performs an effect.
use crossterm::event::{KeyCode, KeyModifiers};
#[cfg(test)]
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Choice {
    pub value: String,
    pub label: String,
}

impl Choice {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Text,
    Choice(Vec<Choice>),
    Toggle,
}

#[derive(Clone, Debug)]
pub(crate) struct Field {
    pub key: &'static str,
    pub label: String,
    pub kind: FieldKind,
    pub value: String,
    pub cursor: usize,
    pub required: bool,
    pub hint: String,
    /// Shown only while another field holds one of these values.
    pub when: Option<(&'static str, Vec<&'static str>)>,
}

impl Field {
    pub fn text(key: &'static str, label: &str, value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            key,
            label: label.into(),
            kind: FieldKind::Text,
            cursor: value.len(),
            value,
            required: false,
            hint: String::new(),
            when: None,
        }
    }
    pub fn choice(key: &'static str, label: &str, choices: Vec<Choice>, value: &str) -> Self {
        let value = if choices.iter().any(|choice| choice.value == value) {
            value.to_string()
        } else {
            choices
                .first()
                .map(|choice| choice.value.clone())
                .unwrap_or_default()
        };
        Self {
            key,
            label: label.into(),
            kind: FieldKind::Choice(choices),
            cursor: 0,
            value,
            required: true,
            hint: String::new(),
            when: None,
        }
    }
    pub fn toggle(key: &'static str, label: &str, on: bool) -> Self {
        Self {
            key,
            label: label.into(),
            kind: FieldKind::Toggle,
            cursor: 0,
            value: if on { "true" } else { "false" }.into(),
            required: false,
            hint: String::new(),
            when: None,
        }
    }
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
    pub fn hint(mut self, hint: &str) -> Self {
        self.hint = hint.into();
        self
    }
    pub fn when(mut self, key: &'static str, values: &[&'static str]) -> Self {
        self.when = Some((key, values.to_vec()));
        self
    }
    /// The text shown for this field's current value.
    pub fn display(&self) -> String {
        match &self.kind {
            FieldKind::Text => self.value.clone(),
            FieldKind::Toggle => if self.value == "true" { "[x]" } else { "[ ]" }.into(),
            FieldKind::Choice(choices) => choices
                .iter()
                .find(|choice| choice.value == self.value)
                .map(|choice| choice.label.clone())
                .unwrap_or_else(|| "(none available)".into()),
        }
    }
    fn cycle(&mut self, forward: bool) {
        match &self.kind {
            FieldKind::Toggle => {
                self.value = if self.value == "true" {
                    "false"
                } else {
                    "true"
                }
                .into();
            }
            FieldKind::Choice(choices) if !choices.is_empty() => {
                let index = choices
                    .iter()
                    .position(|choice| choice.value == self.value)
                    .unwrap_or(0);
                let next = if forward {
                    (index + 1) % choices.len()
                } else {
                    (index + choices.len() - 1) % choices.len()
                };
                self.value = choices[next].value.clone();
            }
            _ => {}
        }
    }
    fn insert(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        self.value.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FormEvent {
    None,
    Submit,
    Cancel,
}

#[derive(Clone, Debug)]
pub(crate) struct Form {
    pub title: String,
    pub intro: Vec<String>,
    pub fields: Vec<Field>,
    pub submit: String,
    /// Index into `visible()` fields, then Submit, then Cancel.
    pub focus: usize,
    pub error: Option<String>,
    pub dirty: bool,
    pub confirm_discard: bool,
    /// Index of the first visible row when the form is taller than the screen.
    pub offset: usize,
}

impl Form {
    pub fn new(title: impl Into<String>, submit: &str, fields: Vec<Field>) -> Self {
        Self {
            title: title.into(),
            intro: Vec::new(),
            fields,
            submit: submit.into(),
            focus: 0,
            error: None,
            dirty: false,
            confirm_discard: false,
            offset: 0,
        }
    }
    pub fn intro(mut self, lines: &[&str]) -> Self {
        self.intro = lines.iter().map(|line| line.to_string()).collect();
        self
    }
    pub fn value(&self, key: &str) -> &str {
        self.fields
            .iter()
            .find(|field| field.key == key)
            .map(|field| field.value.as_str())
            .unwrap_or("")
    }
    pub fn flag(&self, key: &str) -> bool {
        self.value(key) == "true"
    }
    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        if let Some(field) = self.fields.iter_mut().find(|field| field.key == key) {
            field.value = value.into();
            field.cursor = field.value.len();
        }
    }
    fn shown(&self, field: &Field) -> bool {
        field
            .when
            .as_ref()
            .is_none_or(|(key, values)| values.contains(&self.value(key)))
    }
    /// Indices into `fields` of the currently applicable fields.
    pub fn visible(&self) -> Vec<usize> {
        (0..self.fields.len())
            .filter(|index| self.shown(&self.fields[*index]))
            .collect()
    }
    #[cfg(test)]
    pub fn values(&self) -> HashMap<&'static str, String> {
        self.visible()
            .into_iter()
            .map(|index| {
                (
                    self.fields[index].key,
                    self.fields[index].value.trim().to_string(),
                )
            })
            .collect()
    }
    pub fn focused_field(&self) -> Option<usize> {
        self.visible().get(self.focus).copied()
    }
    pub fn on_submit(&self) -> bool {
        self.focus == self.visible().len()
    }
    pub fn on_cancel(&self) -> bool {
        self.focus == self.visible().len() + 1
    }
    /// Required visible fields must be non-empty before a request is built.
    pub fn missing(&self) -> Option<String> {
        self.visible()
            .into_iter()
            .map(|index| &self.fields[index])
            .find(|field| field.required && field.value.trim().is_empty())
            .map(|field| format!("{} is required", field.label))
    }
    fn move_focus(&mut self, delta: isize) {
        let count = self.visible().len() + 2;
        self.focus = (self.focus as isize + delta).rem_euclid(count as isize) as usize;
    }
    pub fn paste(&mut self, text: &str) {
        if let Some(index) = self.focused_field()
            && self.fields[index].kind == FieldKind::Text
        {
            let single = text.replace(['\r', '\n'], " ");
            self.fields[index].insert(single.trim_end());
            self.dirty = true;
            self.confirm_discard = false;
        }
    }
    pub fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> FormEvent {
        let control = modifiers.contains(KeyModifiers::CONTROL);
        if control && code == KeyCode::Char('s') {
            return self.try_submit();
        }
        if code == KeyCode::Esc {
            if self.dirty && !self.confirm_discard {
                self.confirm_discard = true;
                self.error = Some("Unsaved changes. Press Esc again to discard them.".into());
                return FormEvent::None;
            }
            return FormEvent::Cancel;
        }
        self.confirm_discard = false;
        match code {
            KeyCode::Tab | KeyCode::Down => {
                self.move_focus(1);
                return FormEvent::None;
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.move_focus(-1);
                return FormEvent::None;
            }
            KeyCode::Enter => {
                if self.on_submit() {
                    return self.try_submit();
                }
                if self.on_cancel() {
                    return FormEvent::Cancel;
                }
                self.move_focus(1);
                return FormEvent::None;
            }
            _ => {}
        }
        let Some(index) = self.focused_field() else {
            if matches!(code, KeyCode::Left | KeyCode::Right) {
                self.move_focus(if code == KeyCode::Right { 1 } else { -1 });
            }
            return FormEvent::None;
        };
        let field = &mut self.fields[index];
        let before = field.value.clone();
        match (&field.kind, code) {
            (FieldKind::Text, KeyCode::Char('u')) if control => {
                field.value.clear();
                field.cursor = 0;
            }
            (FieldKind::Text, KeyCode::Char(c)) if !control => {
                let mut buffer = [0; 4];
                field.insert(c.encode_utf8(&mut buffer));
            }
            (FieldKind::Text, KeyCode::Backspace) => {
                if let Some(previous) = field.value[..field.cursor].chars().next_back() {
                    field.cursor -= previous.len_utf8();
                    field.value.remove(field.cursor);
                }
            }
            (FieldKind::Text, KeyCode::Delete) => {
                if field.cursor < field.value.len() {
                    field.value.remove(field.cursor);
                }
            }
            (FieldKind::Text, KeyCode::Left) => {
                if let Some(previous) = field.value[..field.cursor].chars().next_back() {
                    field.cursor -= previous.len_utf8();
                }
            }
            (FieldKind::Text, KeyCode::Right) => {
                if let Some(next) = field.value[field.cursor..].chars().next() {
                    field.cursor += next.len_utf8();
                }
            }
            (FieldKind::Text, KeyCode::Home) => field.cursor = 0,
            (FieldKind::Text, KeyCode::End) => field.cursor = field.value.len(),
            (_, KeyCode::Char(' ') | KeyCode::Right) => field.cycle(true),
            (_, KeyCode::Left) => field.cycle(false),
            _ => {}
        }
        if field.value != before {
            self.dirty = true;
            self.error = None;
            // A dependent field may have appeared or disappeared; keep focus on
            // the same field rather than the same row number.
            if let Some(position) = self.visible().iter().position(|i| *i == index) {
                self.focus = position;
            }
        }
        FormEvent::None
    }
    fn try_submit(&mut self) -> FormEvent {
        if let Some(missing) = self.missing() {
            self.error = Some(missing);
            if let Some(position) = self.visible().iter().position(|index| {
                let field = &self.fields[*index];
                field.required && field.value.trim().is_empty()
            }) {
                self.focus = position;
            }
            return FormEvent::None;
        }
        FormEvent::Submit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> Form {
        Form::new(
            "Synthetic",
            "Review",
            vec![
                Field::choice(
                    "source",
                    "Source",
                    vec![
                        Choice::new("remote", "Remote"),
                        Choice::new("local", "Local"),
                    ],
                    "remote",
                ),
                Field::text("url", "URL", "")
                    .required()
                    .when("source", &["remote"]),
                Field::text("path", "Path", "")
                    .required()
                    .when("source", &["local"]),
                Field::toggle("lfs", "LFS", true),
            ],
        )
    }

    #[test]
    fn conditional_fields_follow_choice_and_required_blocks_submit() {
        let mut form = form();
        assert_eq!(form.visible(), vec![0, 1, 3]);
        assert_eq!(
            form.key(KeyCode::Char('s'), KeyModifiers::CONTROL),
            FormEvent::None
        );
        assert_eq!(form.focus, 1, "focus jumps to the missing field");
        assert!(form.error.as_deref().unwrap().contains("URL"));
        form.key(KeyCode::Up, KeyModifiers::NONE);
        form.key(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(form.value("source"), "local");
        assert_eq!(form.visible(), vec![0, 2, 3]);
        form.key(KeyCode::Down, KeyModifiers::NONE);
        form.paste("/synthetic/path\n");
        assert_eq!(form.value("path"), "/synthetic/path");
        assert!(
            !form.values().contains_key("url"),
            "hidden values are not submitted"
        );
        assert_eq!(
            form.key(KeyCode::Char('s'), KeyModifiers::CONTROL),
            FormEvent::Submit
        );
    }

    #[test]
    fn dirty_escape_needs_confirmation_and_unicode_editing_is_boundary_safe() {
        let mut form = form();
        form.key(KeyCode::Down, KeyModifiers::NONE);
        for c in "aé😀".chars() {
            form.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        form.key(KeyCode::Left, KeyModifiers::NONE);
        form.key(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(form.value("url"), "a😀");
        assert_eq!(form.key(KeyCode::Esc, KeyModifiers::NONE), FormEvent::None);
        assert!(form.confirm_discard);
        assert_eq!(
            form.key(KeyCode::Esc, KeyModifiers::NONE),
            FormEvent::Cancel
        );
    }

    #[test]
    fn enter_moves_through_fields_and_activates_buttons() {
        let mut form = form();
        form.set("url", "https://example.invalid/repo.git");
        for _ in 0..3 {
            assert_eq!(
                form.key(KeyCode::Enter, KeyModifiers::NONE),
                FormEvent::None
            );
        }
        assert!(form.on_submit());
        assert_eq!(
            form.key(KeyCode::Enter, KeyModifiers::NONE),
            FormEvent::Submit
        );
        form.key(KeyCode::Down, KeyModifiers::NONE);
        assert!(form.on_cancel());
        assert_eq!(
            form.key(KeyCode::Enter, KeyModifiers::NONE),
            FormEvent::Cancel
        );
    }
}
