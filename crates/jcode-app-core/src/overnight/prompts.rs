//! Managed overnight prose; the runner retains timing, permissions and state.
use super::{OvernightManifest, OvernightPreflight, format_minutes, preflight_summary};
use crate::instruction::workflow::Workflow;
use anyhow::Result;
use chrono::Utc;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OvernightPromptKind {
    Coordinator,
    Continuation,
    Handoff,
    Morning,
    PostWake,
    Final,
}

pub struct OvernightPrompt {
    pub kind: OvernightPromptKind,
    pub text: String,
}

pub fn prompt_event_summary(kind: OvernightPromptKind) -> String {
    match kind {
        OvernightPromptKind::Coordinator => "Sending initial overnight coordinator mission",
        OvernightPromptKind::Handoff => "Sending handoff-ready poke",
        OvernightPromptKind::Morning => "Sending morning report poke",
        OvernightPromptKind::PostWake => "Sending post-wake continuation poke",
        OvernightPromptKind::Final => "Sending final wrap-up poke",
        OvernightPromptKind::Continuation => "Sending continuation poke",
    }
    .to_string()
}

pub fn build_coordinator_prompt(
    manifest: &OvernightManifest,
    preflight: &OvernightPreflight,
) -> Result<OvernightPrompt> {
    let working_dir = manifest.working_dir.as_deref().map(Path::new);
    let mission = match manifest.mission.as_deref() {
        Some(mission) => mission.to_string(),
        None => Workflow::OvernightDefaultMission.render(working_dir)?,
    };
    let text = Workflow::OvernightCoordinator {
        run_id: &manifest.run_id,
        target_wake_at: &manifest.target_wake_at.to_rfc3339(),
        post_wake_grace_until: &manifest.post_wake_grace_until.to_rfc3339(),
        mission: &mission,
        issue_drafts: &manifest.issue_drafts_dir.display().to_string(),
        review_notes: &manifest.review_notes_path.display().to_string(),
        task_cards: &manifest.task_cards_dir.display().to_string(),
        task_card_schema: &manifest
            .task_cards_dir
            .join("task-card-schema.md")
            .display()
            .to_string(),
        validation: &manifest.validation_dir.display().to_string(),
        review_html: &manifest.review_path.display().to_string(),
        preflight_summary: &preflight_summary(preflight),
    }
    .render(working_dir)?;
    Ok(OvernightPrompt {
        kind: OvernightPromptKind::Coordinator,
        text,
    })
}

pub fn build_visible_current_session_prompt(manifest: &OvernightManifest) -> Result<String> {
    let working_dir = manifest.working_dir.as_deref().map(Path::new);
    let mission = match manifest.mission.as_deref() {
        Some(mission) => mission.to_string(),
        None => Workflow::OvernightDefaultMission.render(working_dir)?,
    };
    let text = Workflow::OvernightVisible {
        run_id: &manifest.run_id,
        target_wake_at: &manifest.target_wake_at.to_rfc3339(),
        post_wake_grace_until: &manifest.post_wake_grace_until.to_rfc3339(),
        mission: &mission,
        review_notes: &manifest.review_notes_path.display().to_string(),
        task_cards: &manifest.task_cards_dir.display().to_string(),
        task_card_schema: &manifest
            .task_cards_dir
            .join("task-card-schema.md")
            .display()
            .to_string(),
        validation: &manifest.validation_dir.display().to_string(),
        review_html: &manifest.review_path.display().to_string(),
        manifest_path: &manifest.run_dir.join("manifest.json").display().to_string(),
    }
    .render(working_dir)?;
    Ok(text)
}

pub fn build_continuation_prompt(manifest: &OvernightManifest) -> Result<OvernightPrompt> {
    let working_dir = manifest.working_dir.as_deref().map(Path::new);
    let remaining = manifest
        .target_wake_at
        .signed_duration_since(Utc::now())
        .num_minutes()
        .max(0) as u32;
    let text = Workflow::OvernightContinuation {
        remaining: &format_minutes(remaining),
        review_notes: &manifest.review_notes_path.display().to_string(),
    }
    .render(working_dir)?;
    Ok(OvernightPrompt {
        kind: OvernightPromptKind::Continuation,
        text,
    })
}

pub fn build_handoff_ready_prompt(manifest: &OvernightManifest) -> Result<OvernightPrompt> {
    let working_dir = manifest.working_dir.as_deref().map(Path::new);
    let text = Workflow::OvernightHandoff {
        review_notes: &manifest.review_notes_path.display().to_string(),
    }
    .render(working_dir)?;
    Ok(OvernightPrompt {
        kind: OvernightPromptKind::Handoff,
        text,
    })
}

pub fn build_morning_report_prompt(manifest: &OvernightManifest) -> Result<OvernightPrompt> {
    let working_dir = manifest.working_dir.as_deref().map(Path::new);
    let text = Workflow::OvernightMorning {
        review_notes: &manifest.review_notes_path.display().to_string(),
        review_html: &manifest.review_path.display().to_string(),
    }
    .render(working_dir)?;
    Ok(OvernightPrompt {
        kind: OvernightPromptKind::Morning,
        text,
    })
}

pub fn build_post_wake_continuation_prompt(
    manifest: &OvernightManifest,
) -> Result<OvernightPrompt> {
    let working_dir = manifest.working_dir.as_deref().map(Path::new);
    let text = Workflow::OvernightPostWake {
        review_notes: &manifest.review_notes_path.display().to_string(),
        post_wake_grace_until: &manifest.post_wake_grace_until.to_rfc3339(),
    }
    .render(working_dir)?;
    Ok(OvernightPrompt {
        kind: OvernightPromptKind::PostWake,
        text,
    })
}

pub fn build_final_wrapup_prompt(manifest: &OvernightManifest) -> Result<OvernightPrompt> {
    let working_dir = manifest.working_dir.as_deref().map(Path::new);
    let text = Workflow::OvernightFinal {
        review_notes: &manifest.review_notes_path.display().to_string(),
        review_html: &manifest.review_path.display().to_string(),
    }
    .render(working_dir)?;
    Ok(OvernightPrompt {
        kind: OvernightPromptKind::Final,
        text,
    })
}
