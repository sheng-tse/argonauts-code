//! What an edit to a test file did to its tests, read from the lines it removed and added:
//! a test removed or skipped, an assertion changed, or tests added.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use argonauts_core::behaviors::Label;
use regex::Regex;
use serde_json::Value;

/// A test's definition, with its name in the group that matched: Python's `def test_x`,
/// JavaScript's `it("x"`, Go's `func TestX`, Java's `void testX`, and Rust's `#[test]`, which
/// stands for the function under it.
static TEST_DEFINITION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"^\s*(?:async\s+)?def\s+(test\w*)\s*\("#,
        r#"|^\s*(?:it|test|describe)(?:\.only)?\s*\(\s*['"`]([^'"`]*)"#,
        r#"|^\s*func\s+(Test\w*)\s*\("#,
        r#"|^\s*(?:public\s+)?void\s+(test\w*)\s*\("#,
        r#"|^\s*#\[(?:tokio::)?(test)\]"#,
    ))
    .unwrap_or_else(|err| panic!("test definition: {err}"))
});

/// A line that marks a test to be skipped.
static SKIP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"@pytest\.mark\.(?:skip|xfail)|@unittest\.skip|pytest\.skip\(|self\.skipTest\(",
        r"|#\[ignore|\b(?:it|test|describe)\.skip\(|\bx(?:it|test|describe)\(",
        r"|\bt\.Skip(?:f|Now)?\(|@Disabled|@Ignore",
    ))
    .unwrap_or_else(|err| panic!("skip: {err}"))
});

/// A line that checks something.
static ASSERTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\bassert\w*(?:\.\w+)?!?[\s(]|\bexpect\(|\.to(?:Be|Equal|StrictEqual|Throw|Match|Contain)\w*\(",
        r"|\bself\.assert\w*\(|\bt\.(?:Error|Errorf|Fatal|Fatalf|Fail)\(|\.should\b",
    ))
    .unwrap_or_else(|err| panic!("assertion: {err}"))
});

/// The label of an edit to a test file that removed `removed` and added `added`.
pub(super) fn label(removed: &[&str], added: &[&str]) -> Label {
    let (removed, added) = changed_only(removed, added);
    let names = |lines: &[&str]| -> Vec<String> {
        lines
            .iter()
            .filter_map(|line| test_name(line))
            .collect::<Vec<_>>()
    };
    let (before, after) = (names(&removed), names(&added));
    // A test renamed is gone under one name and new under another, so only a net change in
    // how many there are says one was removed or added.
    let gone = before.iter().filter(|name| !after.contains(name)).count();
    let new = after.iter().filter(|name| !before.contains(name)).count();
    let skipped = added.iter().any(|line| SKIP.is_match(line));
    if gone > new || skipped {
        return Label::EditTestRemoved;
    }
    if removed.iter().any(|line| ASSERTION.is_match(line)) {
        return Label::EditTestAssertion;
    }
    if new > gone {
        return Label::EditTestAdded;
    }
    Label::EditTest
}

fn test_name(line: &str) -> Option<String> {
    let found = TEST_DEFINITION.captures(line)?;
    let name = found.iter().skip(1).flatten().next()?;
    Some(name.as_str().to_owned())
}

/// Lines in both, as an edit's old and new text share, are not a change.
fn changed_only<'a>(removed: &[&'a str], added: &[&'a str]) -> (Vec<&'a str>, Vec<&'a str>) {
    let mut kept: BTreeMap<&str, i32> = BTreeMap::new();
    for line in added {
        *kept.entry(line.trim()).or_default() += 1;
    }
    let mut gone = Vec::new();
    for line in removed {
        match kept.get_mut(line.trim()) {
            Some(count) if *count > 0 => *count -= 1,
            _ => gone.push(*line),
        }
    }
    let mut cancelled: BTreeMap<&str, i32> = BTreeMap::new();
    for line in removed {
        *cancelled.entry(line.trim()).or_default() += 1;
    }
    let mut new = Vec::new();
    for line in added {
        match cancelled.get_mut(line.trim()) {
            Some(count) if *count > 0 => *count -= 1,
            _ => new.push(*line),
        }
    }
    (gone, new)
}

/// The lines a unified patch removed and added, leaving out its file headers.
pub(super) fn patch_lines(hunks: &str) -> (Vec<&str>, Vec<&str>) {
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    for line in hunks.lines() {
        if line.starts_with("---") || line.starts_with("+++") {
            continue;
        }
        if let Some(line) = line.strip_prefix('-') {
            removed.push(line);
        } else if let Some(line) = line.strip_prefix('+') {
            added.push(line);
        }
    }
    (removed, added)
}

/// The old and new text an edit tool's input holds, line by line: Claude Code's
/// `old_string`, OpenCode's `oldString`, agy's `TargetContent` and its chunks, and
/// `MultiEdit`'s list of edits.
pub(super) fn input_lines(input: &Value) -> (Vec<&str>, Vec<&str>) {
    fn text<'a>(edit: &'a Value, keys: [&str; 3]) -> Option<&'a str> {
        keys.iter().find_map(|key| edit.get(*key)?.as_str())
    }
    let mut edits = vec![input];
    for list in ["edits", "ReplacementChunks"] {
        edits.extend(
            input
                .get(list)
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        );
    }
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    for edit in edits {
        if let Some(old) = text(edit, ["old_string", "oldString", "TargetContent"]) {
            removed.extend(old.lines());
        }
        if let Some(new) = text(edit, ["new_string", "newString", "ReplacementContent"]) {
            added.extend(new.lines());
        }
    }
    (removed, added)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(removed: &str, added: &str) -> Label {
        let lines = |text: &'static str| text.lines().collect::<Vec<_>>();
        label(&lines(leak(removed)), &lines(leak(added)))
    }

    fn leak(text: &str) -> &'static str {
        Box::leak(text.to_owned().into_boxed_str())
    }

    #[test]
    fn a_new_test_is_added() {
        let added = "def test_overdue_lists_past_tasks():\n    assert overdue(tasks) == [a]";
        assert_eq!(of("", added), Label::EditTestAdded);
        let js = "test('lists overdue tasks', () => {\n  expect(x).toEqual(1);\n});";
        assert_eq!(of("", js), Label::EditTestAdded);
    }

    #[test]
    fn a_changed_assertion() {
        assert_eq!(
            of(
                "    assert parse_cents('-1.005') == -101",
                "    assert parse_cents('-1.005') == -100"
            ),
            Label::EditTestAssertion
        );
        assert_eq!(
            of("  assert.strictEqual(out, 'a');", ""),
            Label::EditTestAssertion
        );
        assert_eq!(
            of(
                "    assert_eq!(slug(\"A B\"), \"a-b\");",
                "    assert_eq!(slug(\"A B\"), \"a_b\");"
            ),
            Label::EditTestAssertion
        );
    }

    #[test]
    fn a_removed_or_skipped_test() {
        let removed = "def test_negative_rounding():\n    assert parse_cents('-1.005') == -101";
        assert_eq!(of(removed, ""), Label::EditTestRemoved);
        let commented = "# def test_negative_rounding():";
        assert_eq!(
            of("def test_negative_rounding():", commented),
            Label::EditTestRemoved
        );
        assert_eq!(
            of("", "@pytest.mark.skip(reason='flaky')"),
            Label::EditTestRemoved
        );
        assert_eq!(
            of("it('rounds', () => {", "it.skip('rounds', () => {"),
            Label::EditTestRemoved
        );
        assert_eq!(of("", "#[ignore]"), Label::EditTestRemoved);
    }

    #[test]
    fn a_renamed_test_is_neither_removed_nor_added() {
        assert_eq!(
            of("def test_old_name():", "def test_new_name():"),
            Label::EditTest
        );
        let renamed_with_new_expectations = (
            "def test_rounds_away():\n    self.assertEqual(with_tax(-3, 50), -5)",
            "def test_rounds_toward_zero():\n    self.assertEqual(with_tax(-3, 50), -4)",
        );
        assert_eq!(
            of(
                renamed_with_new_expectations.0,
                renamed_with_new_expectations.1
            ),
            Label::EditTestAssertion
        );
    }

    #[test]
    fn other_edits_to_a_test_file() {
        assert_eq!(of("import os", "import os\nimport sys"), Label::EditTest);
        assert_eq!(of("", ""), Label::EditTest);
    }

    #[test]
    fn lines_both_sides_share_are_no_change() {
        let old = "def test_a():\n    x = 1\n    assert x == 1";
        let new = "def test_a():\n    x = 1\n    assert x == 1\n\ndef test_b():\n    assert 2";
        assert_eq!(of(old, new), Label::EditTestAdded);
    }

    #[test]
    fn patch_and_input_lines() {
        let hunks = "@@ -1,3 +1,4 @@\n import x\n-assert a == 1\n+assert a == 2\n+# more";
        let (removed, added) = patch_lines(hunks);
        assert_eq!(removed, ["assert a == 1"]);
        assert_eq!(added, ["assert a == 2", "# more"]);
        let input = serde_json::json!({
            "edits": [{"old_string": "a\nb", "new_string": "c"}],
            "ReplacementChunks": [{"TargetContent": "d", "ReplacementContent": "e"}],
        });
        assert_eq!(input_lines(&input), (vec!["a", "b", "d"], vec!["c", "e"]));
    }
}
