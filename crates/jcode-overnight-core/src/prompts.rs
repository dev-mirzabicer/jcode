use chrono::{DateTime, Utc};

use super::{OvernightManifest, OvernightRunStatus, format_minutes};

pub(crate) fn overnight_phase(manifest: &OvernightManifest, now: DateTime<Utc>) -> &'static str {
    match manifest.status {
        OvernightRunStatus::Completed => "completed",
        OvernightRunStatus::Failed => "failed",
        OvernightRunStatus::CancelRequested => "cancelling",
        OvernightRunStatus::Running => {
            if now < manifest.handoff_ready_at {
                "running"
            } else if now < manifest.target_wake_at {
                "wind-down"
            } else if manifest.morning_report_posted_at.is_none() {
                "morning report"
            } else if now < manifest.post_wake_grace_until {
                "post-wake"
            } else {
                "finalizing"
            }
        }
    }
}

pub(crate) fn time_relation_to_target(manifest: &OvernightManifest, now: DateTime<Utc>) -> String {
    let minutes = manifest
        .target_wake_at
        .signed_duration_since(now)
        .num_minutes();
    if minutes >= 0 {
        format!("target in {}", format_minutes(minutes as u32))
    } else {
        format!("target passed {} ago", format_minutes((-minutes) as u32))
    }
}

pub(crate) fn relative_time(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let minutes = now.signed_duration_since(then).num_minutes();
    if minutes >= 0 {
        format!("{} ago", format_minutes(minutes as u32))
    } else {
        format!("in {}", format_minutes((-minutes) as u32))
    }
}

pub(crate) fn next_prompt_label(manifest: &OvernightManifest, now: DateTime<Utc>) -> String {
    if !matches!(manifest.status, OvernightRunStatus::Running) {
        return "none".to_string();
    }
    if now < manifest.handoff_ready_at {
        return format!(
            "handoff mode in {} or after current turn",
            format_minutes(
                manifest
                    .handoff_ready_at
                    .signed_duration_since(now)
                    .num_minutes()
                    .max(0) as u32
            )
        );
    }
    if now < manifest.target_wake_at {
        return format!(
            "morning report in {} or after current turn",
            format_minutes(
                manifest
                    .target_wake_at
                    .signed_duration_since(now)
                    .num_minutes()
                    .max(0) as u32
            )
        );
    }
    if manifest.morning_report_posted_at.is_none() {
        return "morning report after current turn".to_string();
    }
    if now < manifest.post_wake_grace_until {
        return format!(
            "final wrap by {} or after current turn",
            manifest.post_wake_grace_until.format("%H:%M UTC")
        );
    }
    "final wrap after current turn".to_string()
}
