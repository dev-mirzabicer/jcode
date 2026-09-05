//! Gate tests.
//!
//! The central property: a blind retry must not satisfy the gate. That is what
//! separates this from a "run it twice" speed bump an agent defeats reflexively.

use super::*;
use crate::{RiskContext, assess};
use std::path::PathBuf;

fn ctx() -> RiskContext {
    RiskContext {
        working_dir: Some(PathBuf::from("/home/u/proj")),
        home_dir: Some(PathBuf::from("/home/u")),
    }
}

fn none() -> Justification {
    Justification::default()
}

fn saying(text: &str) -> Justification {
    Justification {
        text: Some(text.to_string()),
    }
}

#[test]
fn safe_commands_pass_straight_through() {
    let assessment = assess("ls -la", &ctx());
    assert_eq!(gate(&assessment, &none()), GateOutcome::Allow);
}

#[test]
fn in_project_cleanup_is_not_interrupted() {
    let assessment = assess("rm -rf target", &ctx());
    assert_eq!(gate(&assessment, &none()), GateOutcome::Allow);
}

#[test]
fn catastrophic_is_denied_even_with_a_perfect_justification() {
    let assessment = assess("rm -rf ~", &ctx());
    let elaborate = saying(
        "The user explicitly asked me to wipe their entire home directory \
         because they are decommissioning this machine today.",
    );
    assert_eq!(gate(&assessment, &elaborate), GateOutcome::Deny);
}

#[test]
fn first_attempt_at_a_risky_command_is_refused_with_a_reflection_prompt() {
    let assessment = assess("rm -rf $TARGET", &ctx());
    assert_eq!(gate(&assessment, &none()), GateOutcome::Reflect);
}

#[test]
fn a_blind_retry_does_not_satisfy_the_gate() {
    // This is the property that makes it more than a speed bump: repeating the
    // identical call, with no added reasoning, fails exactly as before.
    let assessment = assess("rm -rf $TARGET", &ctx());
    let first = gate(&assessment, &none());
    let second = gate(&assessment, &none());
    assert_eq!(first, second);
    assert_eq!(second, GateOutcome::Reflect);
}

#[test]
fn empty_affirmations_are_rejected() {
    // "yes" carries no information about what the user wanted.
    let assessment = assess("rm -rf $TARGET", &ctx());
    for token in ["yes", "ok", "confirmed", "proceed.", "Go ahead!", "y", ""] {
        assert!(
            matches!(gate(&assessment, &saying(token)), GateOutcome::Reflect),
            "{token:?} must not unlock the gate"
        );
    }
}

#[test]
fn a_substantive_justification_allows_the_command() {
    let assessment = assess("rm -rf $TARGET", &ctx());
    let reason = saying(
        "The user asked in their last message to delete the old other-project \
         checkout since they have re-cloned it elsewhere.",
    );
    assert_eq!(gate(&assessment, &reason), GateOutcome::Allow);
}

#[test]
fn justification_substance_check_is_a_low_but_real_bar() {
    assert!(!Justification::default().is_substantive());
    assert!(!saying("   ").is_substantive());
    assert!(!saying("sure").is_substantive());
    assert!(!saying("too short").is_substantive());
    assert!(saying("user asked to remove the stale build output in ~/old-builds").is_substantive());
}

#[test]
fn reflection_assessment_retains_the_specific_path() {
    let assessment = assess("rm -rf $TARGET", &ctx());
    let GateOutcome::Reflect = gate(&assessment, &none()) else {
        panic!("expected reflection");
    };
    assert!(
        assessment.explanation().contains("$TARGET"),
        "the renderer must receive the assessed target as runtime data"
    );
}
