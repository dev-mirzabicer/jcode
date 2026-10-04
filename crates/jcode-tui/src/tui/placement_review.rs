//! First-send placement review for an unplaced primary session.
//!
//! Once managed launch is rolled out, a primary needs a workspace placement
//! before it runs. The server proposes placements that keep the session's
//! recorded working directory; this dialog shows them, sends the human's
//! choice as one `place` request and reports the outcome. It holds no policy:
//! proposals, registration and adoption belong to the workspace owners.
//!
//! The message that triggered the review stays in the composer. When the
//! placement completes the app sends it again; Esc leaves it there.
use crate::protocol::{Request, ServerEvent};
use crate::workspace::*;
use crossterm::event::{KeyCode, KeyModifiers};
use jcode_tui_style::theme;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    /// A proposal request is queued or in flight.
    Loading,
    Choosing,
    /// The chosen placement was sent; its outcome is pending.
    Placing,
    /// The proposal or placement failed. The message explains what to do.
    Failed(String),
}

/// What the app should do after a key or reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    None,
    /// Close the dialog. The composer keeps its text.
    Close,
    /// Placement completed. Close and, if a message is held, send it.
    Placed {
        summary: String,
    },
    /// Close and open workspace management at the Sessions section.
    OpenWorkspace,
}

#[derive(Clone, Debug)]
pub struct PlacementReview {
    pub session: String,
    /// A message is waiting in the composer for this placement.
    pub resend: bool,
    pub stage: Stage,
    pub proposal: Option<PlacementProposal>,
    pub selected: usize,
    /// A broad root needs one explicit extra confirmation.
    broad_armed: bool,
    queued: Option<PrimaryLocationCommand>,
    in_flight: Option<(u64, PrimaryLocationCommand)>,
    /// One automatic re-review after a stale placement.
    retried: bool,
}

impl PlacementReview {
    pub fn open(session: String, resend: bool) -> Self {
        Self {
            queued: Some(PrimaryLocationCommand::ProposePlacement {
                session: session.clone(),
            }),
            session,
            resend,
            stage: Stage::Loading,
            proposal: None,
            selected: 0,
            broad_armed: false,
            in_flight: None,
            retried: false,
        }
    }

    /// Take the next request to send, binding it to `id`.
    pub fn reserve(&mut self, id: u64) -> Option<Request> {
        if self.in_flight.is_some() {
            return None;
        }
        let command = self.queued.take()?;
        self.in_flight = Some((id, command.clone()));
        Some(Request::PrimaryLocation {
            id,
            command: Box::new(command),
        })
    }

    pub fn owns(&self, id: u64) -> bool {
        self.in_flight
            .as_ref()
            .is_some_and(|(owned, _)| *owned == id)
    }

    /// The request could not be written. A placement's outcome is then
    /// unknown, so the dialog asks for a fresh review rather than resending.
    pub fn transport_failed(&mut self, error: &str) {
        let Some((_, command)) = self.in_flight.take() else {
            return;
        };
        self.stage = Stage::Failed(match command {
            PrimaryLocationCommand::Place { .. } => format!(
                "Connection failed while placing ({error}). Press r to review again; an applied placement shows as already placed."
            ),
            _ => format!("Could not request a placement review: {error}"),
        });
    }

    /// Apply a reply to the in-flight request.
    pub fn accept(&mut self, event: ServerEvent) -> Outcome {
        let Some((id, command)) = self.in_flight.take() else {
            return Outcome::None;
        };
        let response = match event {
            ServerEvent::PrimaryLocationResponse { response, .. } => *response,
            ServerEvent::Error { message, .. } => {
                self.stage = Stage::Failed(message);
                return Outcome::None;
            }
            _ => {
                self.in_flight = Some((id, command));
                return Outcome::None;
            }
        };
        match (command, response) {
            (
                PrimaryLocationCommand::ProposePlacement { .. },
                PrimaryLocationResponse::Proposal { proposal },
            ) => {
                self.selected = proposal.default.unwrap_or(0);
                self.broad_armed = false;
                self.stage = if proposal.candidates.is_empty() {
                    Stage::Failed(
                        "No placement can keep this working directory. Choose one in /workspace → Sessions."
                            .into(),
                    )
                } else {
                    Stage::Choosing
                };
                self.proposal = Some(*proposal);
                Outcome::None
            }
            (
                PrimaryLocationCommand::ProposePlacement { .. },
                PrimaryLocationResponse::Rejected { issue },
            ) => {
                // A proposal conflicts only with an existing placement: it was
                // placed elsewhere or by an earlier uncertain attempt.
                if issue.code == IssueCode::Conflict {
                    return Outcome::Placed {
                        summary: "This session is already placed.".into(),
                    };
                }
                self.stage = Stage::Failed(explain(&issue));
                Outcome::None
            }
            (PrimaryLocationCommand::Place { .. }, PrimaryLocationResponse::State { record }) => {
                match record.state {
                    LocationChangeState::Complete => Outcome::Placed {
                        summary: self.chosen_summary(),
                    },
                    LocationChangeState::Pending => {
                        self.stage = Stage::Failed(
                            "Placement is pending until the session is idle. Send again once it applies.".into(),
                        );
                        Outcome::None
                    }
                    _ => {
                        let detail = record
                            .issue
                            .as_ref()
                            .map(explain)
                            .unwrap_or_else(|| format!("Placement ended {:?}.", record.state));
                        self.stage = Stage::Failed(detail);
                        Outcome::None
                    }
                }
            }
            (PrimaryLocationCommand::Place { .. }, PrimaryLocationResponse::Rejected { issue }) => {
                if issue.code == IssueCode::Conflict && !self.retried {
                    // The catalog or session changed since the review. Review
                    // once more instead of guessing.
                    self.retried = true;
                    self.review_again();
                    return Outcome::None;
                }
                self.stage = Stage::Failed(explain(&issue));
                Outcome::None
            }
            (_, other) => {
                self.stage = Stage::Failed(format!("Unexpected reply: {other:?}"));
                Outcome::None
            }
        }
    }

    pub fn key(&mut self, code: KeyCode, _modifiers: KeyModifiers) -> Outcome {
        match code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Char('w') | KeyCode::Char('W') => return Outcome::OpenWorkspace,
            _ => {}
        }
        match self.stage {
            Stage::Choosing => {}
            Stage::Failed(_) => {
                if matches!(code, KeyCode::Char('r') | KeyCode::Char('R')) {
                    self.review_again();
                }
                return Outcome::None;
            }
            Stage::Loading | Stage::Placing => return Outcome::None,
        }
        let count = self
            .proposal
            .as_ref()
            .map(|proposal| proposal.candidates.len())
            .unwrap_or(0);
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                self.broad_armed = false;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < count {
                    self.selected += 1;
                }
                self.broad_armed = false;
            }
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => self.confirm(),
            _ => {}
        }
        Outcome::None
    }

    fn confirm(&mut self) {
        let Some(proposal) = &self.proposal else {
            return;
        };
        let Some(candidate) = proposal.candidates.get(self.selected) else {
            return;
        };
        if candidate.broad && !self.broad_armed {
            self.broad_armed = true;
            return;
        }
        self.queued = Some(PrimaryLocationCommand::Place {
            request: SessionPlacementRequest {
                request: RequestId::new(),
                session: proposal.session.clone(),
                working_dir: proposal.working_dir.clone(),
                expected_catalog_revision: proposal.catalog_revision,
                placement: candidate.placement.clone(),
            },
        });
        self.stage = Stage::Placing;
    }

    fn review_again(&mut self) {
        self.queued = Some(PrimaryLocationCommand::ProposePlacement {
            session: self.session.clone(),
        });
        self.stage = Stage::Loading;
    }

    fn chosen_summary(&self) -> String {
        self.proposal
            .as_ref()
            .and_then(|proposal| proposal.candidates.get(self.selected))
            .map(|candidate| format!("Placed: {}", candidate_title(candidate)))
            .unwrap_or_else(|| "Placed.".into())
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        // Below 24 columns the dialog cannot show its choices; the composer
        // keeps the message and /workspace remains available.
        if area.width < 24 || area.height < 6 {
            return;
        }
        let width = area.width.saturating_sub(4).min(76);
        let lines = self.lines(width.saturating_sub(4) as usize);
        let height = (lines.len() as u16 + 2).min(area.height);
        let rect = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        };
        frame.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::accent_color()))
            .title(Span::styled(
                " Place this session ",
                Style::default()
                    .fg(theme::accent_color())
                    .add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let inner = Rect {
            x: inner.x + 1,
            width: inner.width.saturating_sub(2),
            ..inner
        };
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let muted = Style::default().fg(theme::dim_color());
        let accent = Style::default().fg(theme::accent_color());
        let warning = Style::default().fg(theme::warning_color());
        let error = Style::default().fg(theme::error_color());
        let mut lines = Vec::new();
        let cwd = self
            .proposal
            .as_ref()
            .map(|proposal| display_path(&proposal.working_dir))
            .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled("Working directory  ", muted),
            Span::raw(truncate_start(&cwd, width.saturating_sub(19))),
        ]));
        lines.push(Line::raw(""));
        match &self.stage {
            Stage::Loading => lines.push(Line::styled("Reviewing placements…", muted)),
            Stage::Failed(message) => {
                for row in wrap_words(message, width) {
                    lines.push(Line::styled(row, error));
                }
                lines.push(Line::raw(""));
                for row in hints(
                    &[
                        "r review again",
                        "w /workspace",
                        "Esc close (message stays)",
                    ],
                    width,
                ) {
                    lines.push(Line::styled(row, muted));
                }
                return lines;
            }
            Stage::Choosing | Stage::Placing => {
                let proposal = self.proposal.as_ref().expect("choosing has a proposal");
                for (index, candidate) in proposal.candidates.iter().enumerate() {
                    let selected = index == self.selected;
                    let marker = if selected { "› " } else { "  " };
                    let style = if selected {
                        accent.add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    lines.push(Line::from(vec![
                        Span::styled(marker, accent),
                        Span::styled(
                            truncate(&candidate_title(candidate), width.saturating_sub(2)),
                            style,
                        ),
                    ]));
                    let suffix = if candidate.broad { " · broad" } else { "" };
                    let detail = format!(
                        "    {}{suffix}",
                        candidate_scope(candidate, width.saturating_sub(4 + suffix.len()))
                    );
                    lines.push(Line::styled(
                        truncate(&detail, width),
                        if candidate.broad { warning } else { muted },
                    ));
                }
                lines.push(Line::raw(""));
                if self.stage == Stage::Placing {
                    lines.push(Line::styled("Placing…", muted));
                } else if self.broad_armed {
                    for row in wrap_words(
                        "This gives the session write access to everything under that root. Press Enter again to confirm.",
                        width,
                    ) {
                        lines.push(Line::styled(row, warning));
                    }
                } else {
                    let action = if self.resend {
                        "Enter place and send"
                    } else {
                        "Enter place"
                    };
                    for row in hints(&[action, "↑↓ choose", "w /workspace", "Esc cancel"], width)
                    {
                        lines.push(Line::styled(row, muted));
                    }
                }
            }
        }
        lines
    }
}

fn candidate_title(candidate: &PlacementCandidate) -> String {
    let kind = match &candidate.placement {
        PrimaryPlacement::Standalone { .. } => "New standalone location",
        PrimaryPlacement::Existing { placement } => match placement {
            Placement::Project(_) => "Project",
            Placement::WorkArea(_) => "Work area",
            Placement::Checkout(_) => "Checkout",
            Placement::Directory(_) => "Directory",
            Placement::Standalone(_) => "Standalone location",
        },
    };
    match (&candidate.project, &candidate.placement) {
        (
            Some(project),
            PrimaryPlacement::Existing {
                placement: Placement::Project(_),
            },
        ) => {
            format!("{kind} {project}")
        }
        (Some(project), _) => format!("{kind} {} · project {project}", candidate.name),
        (None, _) => format!("{kind} {}", candidate.name),
    }
}

fn candidate_scope(candidate: &PlacementCandidate, width: usize) -> String {
    match &candidate.placement {
        PrimaryPlacement::Existing {
            placement: Placement::Project(_) | Placement::WorkArea(_),
        } => "writes: every root it contains".into(),
        _ => format!(
            "writes: {}",
            truncate_start(&display_path(&candidate.root), width.saturating_sub(8))
        ),
    }
}

/// Join hints with " · ", starting a new row when the next would not fit.
fn hints(parts: &[&str], width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    let mut rows: Vec<String> = Vec::new();
    for part in parts {
        match rows.last_mut() {
            Some(row) if row.width() + 3 + part.width() <= width => {
                row.push_str(" · ");
                row.push_str(part);
            }
            _ => rows.push(truncate(part, width)),
        }
    }
    rows
}

/// Wrap at spaces into rows of at most `width` cells.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    let mut rows: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match rows.last_mut() {
            Some(row) if row.width() + 1 + word.width() <= width => {
                row.push(' ');
                row.push_str(word);
            }
            _ => rows.push(truncate(word, width)),
        }
    }
    rows
}

/// Fit `text` into `width` cells keeping its end, which is the informative
/// part of a path.
fn truncate_start(text: &str, width: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if text.width() <= width {
        return text.to_string();
    }
    let mut used = 1;
    let mut tail = Vec::new();
    for c in text.chars().rev() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        used += w;
        tail.push(c);
    }
    std::iter::once('…').chain(tail.into_iter().rev()).collect()
}

fn explain(issue: &Issue) -> String {
    if issue.is_not_initialized() {
        return "Workspace is not set up yet. Initialize it in /workspace → Organization first."
            .into();
    }
    issue.detail.clone()
}

fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    match dirs::home_dir() {
        Some(home) => {
            let home = home.display().to_string();
            match text.strip_prefix(&home) {
                Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
                _ => text,
            }
        }
        None => text,
    }
}

/// Fit `text` into `width` terminal cells, ending with `…` when cut.
fn truncate(text: &str, width: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if text.width() <= width {
        return text.to_string();
    }
    let mut used = 0;
    let mut out = String::new();
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests;
