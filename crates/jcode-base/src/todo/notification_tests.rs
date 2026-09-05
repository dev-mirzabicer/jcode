use crate::instruction::{InstructionRuntime, InstructionSources};

fn fixture_render(request: TodoNoticeRequest) -> RenderedTodoNotice {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("notifications")).unwrap();
    for registration in crate::instruction::notification::registrations().unwrap() {
        let id = registration.id.as_str();
        if !id.starts_with("todo-") { continue; }
        let body = if id == "todo-auto-poke" {
            "SYNTHETIC-INCOMPLETE {{count}} {{plural}}".to_string()
        } else if id.starts_with("todo-digest-") && !id.starts_with("todo-digest-intent-") {
            format!("SYNTHETIC:{id}{{{{label}}}}")
        } else { format!("SYNTHETIC:{id}") };
        std::fs::write(root.path().join(registration.default_relative_path), format!("---\nid: {id}\nkind: notification\ntemplate: handlebars\n---\n{body}")).unwrap();
    }
    render_todo_notice_in(&request, &InstructionRuntime::discover(InstructionSources::new(root.path()))).unwrap()
}

fn build_gate_digest(observations: &[GateObservation], plan: &TodoPlan, goals: &[TodoGoal]) -> Option<String> {
    if observations.is_empty() { return None; }
    Some(fixture_render(TodoNoticeRequest::Digest { observations: observations.to_vec(), plan: plan.clone(), goals: goals.to_vec() }).text)
}

fn build_todo_completion_continuation_message(todos: &[TodoItem]) -> String {
    fixture_render(TodoNoticeRequest::Completion { todos: todos.to_vec() }).text
}
fn build_todo_confidence_spike_continuation_message(todos: &[TodoItem]) -> String {
    fixture_render(TodoNoticeRequest::Confidence { todos: todos.to_vec() }).text
}
fn build_todo_ownership_continuation_message(todos: &[TodoItem], goals: &[TodoGoal]) -> String {
    fixture_render(TodoNoticeRequest::Ownership { todos: todos.to_vec(), goals: goals.to_vec() }).text
}

#[test]
fn every_digest_dimension_selects_the_registered_state_branch() {
    let dimensions = [(GateObservationKind::IntentUnderstanding,"intent"),(GateObservationKind::ClosedFeedbackLoop,"loop"),(GateObservationKind::FeedbackLoopRelevance,"relevance"),(GateObservationKind::FeedbackLoopCoverage,"coverage"),(GateObservationKind::FeedbackLoopTraceability,"traceability")];
    for (kind,id) in dimensions {
        for cleared in [false,true] {
            let plan=TodoPlan { understands_user_intent: Some(if cleared {IntentUnderstanding::Complete} else {IntentUnderstanding::Uncertain}), ..Default::default() };
            let goal=TodoGoal { group:Some("group".into()),closed_feedback_loop:Some(if cleared {FeedbackLoopState::Closed}else{FeedbackLoopState::Absent}),feedback_loop_relevance:Some(if cleared {FeedbackLoopRelevance::AcceptanceAligned}else{FeedbackLoopRelevance::Synthetic}),feedback_loop_coverage:Some(if cleared {FeedbackLoopCoverage::EdgeAndIntegrationPaths}else{FeedbackLoopCoverage::Narrow}),feedback_loop_traceability:Some(if cleared {FeedbackLoopTraceability::Complete}else{FeedbackLoopTraceability::Unmapped}),difficulty:Some(Difficulty::Involved),..Default::default() };
            let rendered=fixture_render(TodoNoticeRequest::Digest {observations:vec![GateObservation {kind,group:Some("group".into()),state:None}],plan,goals:vec![goal]});
            assert_eq!(rendered.kind,TodoNoticeKind::Digest);
            assert!(rendered.text.contains(&format!("SYNTHETIC:todo-digest-{id}-{}",if cleared {"cleared"}else{"open"})));
        }
    }
}
