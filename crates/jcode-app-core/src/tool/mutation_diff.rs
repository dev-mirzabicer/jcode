//! Complete changed-line receipts. Human FileTouch cards apply their own
//! preview limits after this result; canonical tool output is never capped here.
use similar::{ChangeTag, TextDiff};
use std::fmt::Write;

pub(super) fn whole_file(old: &str, new: &str) -> String {
    changes(old, new, 1)
}

pub(super) fn changes(old: &str, new: &str, start_line: usize) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();
    let mut old_line = start_line;
    let mut new_line = start_line;
    for change in diff.iter_all_changes() {
        let (line, sign) = match change.tag() {
            ChangeTag::Delete => {
                let line = old_line;
                old_line += 1;
                (line, '-')
            }
            ChangeTag::Insert => {
                let line = new_line;
                new_line += 1;
                (line, '+')
            }
            ChangeTag::Equal => {
                old_line += 1;
                new_line += 1;
                continue;
            }
        };
        let value = change.value().strip_suffix('\n').unwrap_or(change.value());
        writeln!(output, "{line}{sign} {value}").expect("String formatting cannot fail");
        if !change.value().ends_with('\n') {
            output.push_str("\\ No newline at end of file\n");
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changes_keep_indentation_blank_lines_and_all_selected_tail_lines() {
        let new = format!("  indented  \n\n{}TAIL\n", "line\n".repeat(100));
        let result = whole_file("", &new);
        assert!(result.starts_with("1+   indented  \n2+ \n"));
        assert!(result.contains("103+ TAIL\n"));
        assert_eq!(result.lines().count(), 103);
    }
    #[test]
    fn unchanged_lines_advance_positions_without_becoming_diff_output() {
        assert_eq!(
            changes("first\nold\n", "first\nnew\n", 10),
            "11- old\n11+ new\n"
        );
    }
    #[test]
    fn newline_only_changes_are_visible() {
        assert_eq!(
            whole_file("same\n", "same"),
            "1- same\n1+ same\n\\ No newline at end of file\n"
        );
    }
}
