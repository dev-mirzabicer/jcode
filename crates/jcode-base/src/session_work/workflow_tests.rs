use super::*;

fn errors(text: &str) -> Vec<(Option<usize>, String)> {
    parse_workflow(text)
        .expect_err("text should be rejected")
        .0
        .into_iter()
        .map(|issue| (issue.line, issue.message))
        .collect()
}

fn first_line(text: &str) -> Option<usize> {
    errors(text)[0].0
}

const EXAMPLE: &str = "- [x] ground: Understand the sidebar and its constraints {research}
- [>] proto: Prototype three sidebars {prototype/frontend}
  - [x] proto-a: Minimal rail
  - [>] proto-b: Dense panel
- [?] after: Decide the rest after Mirza picks one
- [-] perf: Performance pass
  > skipped: CSS-only change
";

#[test]
fn the_documented_example_parses_with_types_notes_and_nesting() {
    let workflow = parse_workflow(EXAMPLE).unwrap();
    assert_eq!(workflow.modules.len(), 4);
    let proto = workflow.module("proto").unwrap();
    assert_eq!(proto.status, ModuleStatus::Active);
    assert_eq!(
        proto.module_type.as_ref().unwrap().to_string(),
        "prototype/frontend"
    );
    assert_eq!(proto.children.len(), 2);
    assert_eq!(workflow.active_leaf().unwrap().id.as_str(), "proto-b");
    let perf = workflow.module("perf").unwrap();
    assert_eq!(perf.skip_reason(), Some("CSS-only change"));
    assert_eq!(
        workflow.module("ground").unwrap().title,
        "Understand the sidebar and its constraints"
    );
    assert_eq!(render_workflow(&workflow), EXAMPLE);
}

#[test]
fn blank_lines_and_crlf_are_accepted_but_the_text_is_otherwise_strict() {
    let workflow = parse_workflow("\n- [ ] a: A\r\n\n  \n- [ ] b: B title: with colon\n").unwrap();
    assert_eq!(workflow.modules.len(), 2);
    assert_eq!(workflow.modules[1].title, "B title: with colon");
}

#[test]
fn every_marker_is_recognized_and_unknown_markers_are_line_errors() {
    for (marker, status) in [
        (' ', ModuleStatus::Pending),
        ('>', ModuleStatus::Active),
        ('x', ModuleStatus::Done),
        ('?', ModuleStatus::Undetermined),
    ] {
        let workflow = parse_workflow(&format!("- [{marker}] a: A\n")).unwrap();
        assert_eq!(workflow.modules[0].status, status);
    }
    let found = errors("- [ ] a: A\n- [X] b: B\n");
    assert_eq!(found[0].0, Some(2));
    assert!(found[0].1.contains("[X]"));
}

#[test]
fn malformed_lines_report_their_own_line() {
    for (text, line) in [
        ("# Plan\n- [ ] a: A\n", 1),
        ("- [ ] a: A\n- [ ]a: A2\n", 2),
        ("- [ ] a: A\n- [ ] b B\n", 2),
        ("- [ ] a: A\n- [ ] B: upper\n", 2),
        ("- [ ] a: A\n- [ ] b:no-space\n", 2),
        ("- [ ] a: A\n- [ ] b : spaced\n", 2),
        ("- [ ] a: A\n- [ ] b: \n", 2),
        ("- [ ] a: A\n- [ ] : no id\n", 2),
        ("- [ ] a: A\n   - [ ] b: odd\n", 2),
        ("- [ ] a: A\n\t- [ ] b: tab\n", 2),
        ("  - [ ] a: too deep first\n", 1),
        ("- [ ] a: A\n    - [ ] b: jumps two levels\n", 2),
        ("> orphan note\n- [ ] a: A\n", 1),
        ("- [ ] a: A\n> unindented note\n", 2),
        ("- [ ] a: A {Bad}\n", 1),
        ("- [ ] a: A {x/y/z}\n", 1),
        ("* [ ] a: bullet\n", 1),
    ] {
        assert_eq!(first_line(text), Some(line), "{text:?}");
    }
}

#[test]
fn braces_inside_a_title_are_not_a_module_type() {
    let workflow =
        parse_workflow("- [ ] a: Use {curly} braces in text\n- [ ] b: Glued{type}\n").unwrap();
    assert_eq!(workflow.modules[0].module_type, None);
    assert_eq!(workflow.modules[0].title, "Use {curly} braces in text");
    assert_eq!(workflow.modules[1].module_type, None);
}

#[test]
fn an_empty_workflow_is_refused_without_a_line() {
    for text in ["", "\n\n", "   \n"] {
        let found = errors(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, None);
    }
}

#[test]
fn only_one_leaf_may_be_active() {
    let found = errors("- [>] a: A\n- [ ] b: B\n- [>] c: C\n");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, Some(3));
    assert!(found[0].1.contains("`a` (line 1)") && found[0].1.contains("`c` (line 3)"));
}

#[test]
fn parent_markers_must_follow_their_children() {
    for (text, ok) in [
        ("- [>] p: P\n  - [>] a: A\n  - [ ] b: B\n", true),
        (
            "- [x] p: P\n  - [x] a: A\n  - [-] b: B\n    > skipped: why\n",
            true,
        ),
        ("- [-] p: P\n  - [-] a: A\n    > skipped: why\n", true),
        ("- [?] p: P\n  - [?] a: A\n", true),
        ("- [ ] p: P\n  - [x] a: A\n  - [ ] b: B\n", true),
        ("- [ ] p: P\n  - [x] a: A\n  - [?] b: B\n", true),
        ("- [x] p: P\n  - [>] a: A\n", false),
        ("- [ ] p: P\n  - [>] a: A\n", false),
        ("- [>] p: P\n  - [x] a: A\n", false),
        ("- [x] p: P\n  - [-] a: A\n    > skipped: why\n", false),
        ("- [?] p: P\n  - [ ] a: A\n", false),
    ] {
        assert_eq!(parse_workflow(text).is_ok(), ok, "{text}");
    }
    // Each parent is checked against its children's markers as written, so
    // only the parent that disagrees with its own children is reported.
    let found = errors("- [ ] p: P\n  - [ ] q: Q\n    - [>] a: A\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, Some(2));
}

#[test]
fn skipped_leaves_need_a_reason_note() {
    assert_eq!(first_line("- [ ] a: A\n- [-] b: B\n"), Some(2));
    assert_eq!(first_line("- [-] b: B\n  > skipped:\n"), Some(1));
    assert!(parse_workflow("- [-] b: B\n  > other note\n  > skipped: done elsewhere\n").is_ok());
}

#[test]
fn module_ids_must_be_unique_across_the_tree() {
    let found = errors("- [ ] a: A\n  - [ ] b: B\n- [ ] b: Again\n");
    assert_eq!(found[0].0, Some(3));
    assert!(found[0].1.contains("line 2"));
}

#[test]
fn all_problems_are_reported_together_in_line_order() {
    let found = errors("- [ ] a: A\n- [Q] b: B\n- [ ] a: dup\n- [-] c: C\n");
    let lines: Vec<_> = found.iter().map(|(line, _)| *line).collect();
    assert_eq!(lines, vec![Some(2)]);
    let found = errors("- [ ] a: A\n- [ ] a: dup\n- [-] c: C\n");
    let lines: Vec<_> = found.iter().map(|(line, _)| *line).collect();
    assert_eq!(lines, vec![Some(2), Some(3)]);
    let message = parse_workflow("- [ ] a: A\n- [ ] a: dup\n")
        .unwrap_err()
        .to_string();
    assert!(message.starts_with("workflow.md was not changed:\n- line 2: "));
}

/// Deterministic xorshift generator: the property tests need no extra crate.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn random_tree(
    rng: &mut Rng,
    depth: usize,
    next_id: &mut usize,
    active_left: &mut bool,
) -> Vec<WorkflowModule> {
    let count = 1 + rng.below(3) as usize;
    (0..count)
        .map(|_| {
            *next_id += 1;
            let id = ModuleId::parse(&format!("m{next_id}")).unwrap();
            let children = if depth < 3 && rng.below(3) == 0 {
                random_tree(rng, depth + 1, next_id, active_left)
            } else {
                Vec::new()
            };
            let mut notes = Vec::new();
            if rng.below(4) == 0 {
                notes.push(format!("note {next_id}: keep {{braces}} and > marks"));
            }
            let status = if children.is_empty() {
                let mut status = match rng.below(5) {
                    0 => ModuleStatus::Pending,
                    1 => ModuleStatus::Active,
                    2 => ModuleStatus::Done,
                    3 => ModuleStatus::Skipped,
                    _ => ModuleStatus::Undetermined,
                };
                if status == ModuleStatus::Active {
                    if *active_left {
                        *active_left = false;
                    } else {
                        status = ModuleStatus::Pending;
                    }
                }
                if status == ModuleStatus::Skipped {
                    notes.push(format!("skipped: reason {next_id}"));
                }
                status
            } else {
                derived_parent_status(&children)
            };
            let module_type = match rng.below(3) {
                0 => None,
                1 => Some(ModuleTypeRef {
                    name: "research".into(),
                    subtype: None,
                }),
                _ => Some(ModuleTypeRef {
                    name: "prototype".into(),
                    subtype: Some("front-end".into()),
                }),
            };
            WorkflowModule {
                id,
                title: format!("Title {next_id}: with colon"),
                module_type,
                status,
                notes,
                children,
            }
        })
        .collect()
}

#[test]
fn rendered_valid_workflows_parse_back_identically() {
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    for _ in 0..2000 {
        let mut next_id = 0;
        let mut active_left = true;
        let workflow = Workflow {
            modules: random_tree(&mut rng, 0, &mut next_id, &mut active_left),
        };
        let text = render_workflow(&workflow);
        let parsed = parse_workflow(&text).unwrap_or_else(|error| panic!("{error}\n{text}"));
        assert_eq!(parsed, workflow, "{text}");
        assert_eq!(render_workflow(&parsed), text);
    }
}

#[test]
fn single_line_corruptions_of_valid_workflows_are_rejected_at_that_line() {
    let mut rng = Rng(0x0dd5_eed5_0000_0042);
    for _ in 0..500 {
        let mut next_id = 0;
        let mut active_left = true;
        let workflow = Workflow {
            modules: random_tree(&mut rng, 0, &mut next_id, &mut active_left),
        };
        let text = render_workflow(&workflow);
        let lines: Vec<&str> = text.lines().collect();
        let target = rng.below(lines.len() as u64) as usize;
        let mut broken: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        broken[target] = format!(
            "{}\t",
            broken[target]
                .replacen("- [", "- (", 1)
                .replacen("> ", "x ", 1)
        );
        let rejoined = broken.join("\n");
        let found = parse_workflow(&rejoined).expect_err(&rejoined);
        assert!(
            found.0.iter().any(|issue| issue.line == Some(target + 1)),
            "{found:?}\n{rejoined}"
        );
    }
}
