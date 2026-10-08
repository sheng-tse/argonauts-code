//! A structured result drawn as what it means: a plan as a table of its tasks with who does
//! and who reviews each, a report or a review as sentences, and anything else as its fields in
//! words. The JSON itself stays in the raw log, behind `r`.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::WorkerRef;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use serde_json::{Map, Value};

use super::columns::{self, Align, Cell, Column, Give};
use super::panes;
use crate::text::{sanitize, sanitize_line, wrap_text};
use crate::theme::{Role, Theme};

/// Items of a list shown before `+N more`.
const LIST_ITEMS: usize = 8;

/// A list this short reads as a sentence, `a, b and c`.
const INLINE_ITEMS: usize = 4;
const INLINE_CELLS: usize = 60;

pub(super) struct Draw<'a> {
    pub names: &'a DisplayNames,
    pub theme: &'a Theme,
    pub width: usize,
    /// `→` was pressed: each task of a plan shows what it asks for.
    pub expanded: bool,
    /// Whether to say that `→` expands, which only a view that takes the key does.
    pub hints: bool,
}

/// The rows of `value`, and whether `→` shows more of it. `None` when it isn't an object,
/// which is drawn as the text it is.
pub(super) fn lines(value: &Value, draw: &Draw<'_>) -> Option<(Vec<Line<'static>>, bool)> {
    let object = value.as_object()?;
    if let Some(plan) = Plan::read(object, draw.names) {
        return Some((plan.lines(draw), plan.has_more()));
    }
    Some((sentences(object, draw), false))
}

fn text(value: &Value) -> Option<String> {
    let clean = sanitize_line(value.as_str()?);
    let clean = clean.trim();
    (!clean.is_empty()).then(|| clean.to_owned())
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(text).collect())
        .unwrap_or_default()
}

/// `a`, `a and b`, `a, b and c`.
fn listed(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn head(title: &str, rest: &str, theme: &Theme) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!("{} ", theme.glyphs.bullet),
            theme.style(Role::Success),
        ),
        Span::styled(
            title.to_owned(),
            theme.style(Role::Bright).add_modifier(Modifier::BOLD),
        ),
    ];
    if !rest.is_empty() {
        spans.push(Span::styled(format!(" · {rest}"), theme.style(Role::Text)));
    }
    Line::from(spans)
}

fn indented(text: &str, indent: usize, role: Role, draw: &Draw<'_>) -> Vec<Line<'static>> {
    let pad = [Span::raw(" ".repeat(indent))];
    sanitize(text)
        .lines()
        .flat_map(|row| wrap_text(row, draw.theme.style(role), draw.width, &pad, &pad))
        .collect()
}

/// A plan's tasks: the orchestrator's tasks with their assignments, or a draft's pieces.
struct Plan {
    tasks: Vec<Task>,
    assigned: bool,
}

struct Task {
    key: String,
    title: String,
    worker: String,
    reviewers: String,
    waits_on: Vec<String>,
    /// What the task asks for, shown once expanded.
    asks: String,
    paths: Vec<String>,
}

impl Plan {
    fn read(object: &Map<String, Value>, names: &DisplayNames) -> Option<Self> {
        let (items, key_field, title_field, asks_field, paths_field) =
            match (object.get("cards"), object.get("pieces")) {
                (Some(Value::Array(cards)), _) => (cards, "key", "title", "task", "owned_paths"),
                (None, Some(Value::Array(pieces))) => {
                    (pieces, "name", "description", "description", "paths")
                }
                _ => return None,
            };
        let assignments = object.get("assignments").and_then(Value::as_array);
        let named = |worker: &str| match worker.parse::<WorkerRef>() {
            Ok(worker) => sanitize_line(&names.worker(&worker)).trim().to_owned(),
            Err(_) => sanitize_line(worker).trim().to_owned(),
        };
        let tasks: Vec<Task> = items
            .iter()
            .filter_map(|item| {
                let key = text(item.get(key_field)?)?;
                let assignment = assignments.and_then(|all| {
                    all.iter()
                        .find(|a| a.get("card").and_then(Value::as_str) == Some(key.as_str()))
                });
                let reviewers: Vec<String> = strings(assignment.and_then(|a| a.get("reviewers")))
                    .iter()
                    .map(|reviewer| named(reviewer))
                    .collect();
                let title = item.get(title_field).and_then(text).unwrap_or_default();
                Some(Task {
                    worker: assignment
                        .and_then(|a| a.get("owner"))
                        .and_then(Value::as_str)
                        .map(named)
                        .unwrap_or_default(),
                    reviewers: reviewers.join(", "),
                    waits_on: strings(item.get("depends_on")),
                    // A draft's piece has no more to say than its one line.
                    asks: item
                        .get(asks_field)
                        .and_then(Value::as_str)
                        .filter(|_| asks_field != title_field)
                        .unwrap_or_default()
                        .to_owned(),
                    paths: strings(item.get(paths_field)),
                    key,
                    title,
                })
            })
            .collect();
        (!tasks.is_empty()).then_some(Self {
            assigned: tasks.iter().any(|task| !task.worker.is_empty()),
            tasks,
        })
    }

    fn has_more(&self) -> bool {
        self.tasks
            .iter()
            .any(|task| !task.asks.trim().is_empty() || !task.paths.is_empty())
    }

    /// `● Plan · 3 tasks`, a header row, a row per task, and under them which task waits on
    /// which. The names give way last: who reviews goes first, then the worker, then the
    /// description is cut.
    fn lines(&self, draw: &Draw<'_>) -> Vec<Line<'static>> {
        let theme = draw.theme;
        let dim = theme.style(Role::Dim);
        let plain = theme.style(Role::Text);
        let strong = theme.style(Role::Bright).add_modifier(Modifier::BOLD);
        let cell = |text: &str, style| -> Cell { vec![Span::styled(text.to_owned(), style)] };
        let headers = ["task", "what it does", "worker", "reviewed by"];
        let shown = if self.assigned { 4 } else { 2 };
        let rows: Vec<Vec<Cell>> = self
            .tasks
            .iter()
            .map(|task| {
                vec![
                    cell(&task.key, strong),
                    cell(&task.title, plain),
                    cell(&task.worker, plain),
                    cell(&task.reviewers, plain),
                ]
            })
            .collect();
        let header: Vec<Cell> = headers.iter().map(|name| cell(name, dim)).collect();
        let mut fitted: Vec<Column> = (0..shown)
            .map(|i| {
                Column::widest(
                    Align::Left,
                    rows.iter().map(|row| &row[i]).chain([&header[i]]),
                )
            })
            .collect();
        let mut gives = Vec::new();
        if self.assigned {
            gives.extend([
                Give::Cut { column: 3, min: 12 },
                Give::Cut { column: 1, min: 16 },
                Give::Drop(3),
                Give::Cut { column: 2, min: 8 },
            ]);
        }
        gives.push(Give::Cut { column: 1, min: 8 });
        columns::fit(&mut fitted, draw.width.saturating_sub(2), &gives);

        let lead = || vec![Span::raw("  ")];
        let noun = if self.tasks.len() == 1 {
            "task"
        } else {
            "tasks"
        };
        let mut lines = vec![
            head("Plan", &format!("{} {noun}", self.tasks.len()), theme),
            columns::line(lead(), &fitted, &header, None, draw.width),
        ];
        for (task, row) in self.tasks.iter().zip(&rows) {
            lines.push(columns::line(lead(), &fitted, row, None, draw.width));
            if draw.expanded {
                lines.extend(indented(&task.asks, 4, Role::Dim, draw));
                if !task.paths.is_empty() {
                    let owns = format!("owns {}", listed(&task.paths));
                    lines.extend(indented(&owns, 4, Role::Dim, draw));
                }
            }
        }
        let mut notes: Vec<String> = self
            .tasks
            .iter()
            .filter(|task| !task.waits_on.is_empty())
            .map(|task| format!("{} waits on {}", task.key, listed(&task.waits_on)))
            .collect();
        if self.has_more() && draw.hints {
            let hint = if draw.expanded {
                "→ folds the tasks"
            } else {
                "→ expands the tasks"
            };
            notes.push(panes::key_names(hint, theme));
        }
        if !notes.is_empty() {
            let lead = [Span::styled(format!("  {}  ", theme.glyphs.result), dim)];
            let rest = [Span::raw("     ")];
            lines.extend(wrap_text(&notes.join(" · "), dim, draw.width, &lead, &rest));
        }
        lines
    }
}

/// What kind of result an object is, by the fields that say how it came out.
fn title_of(object: &Map<String, Value>) -> (&'static str, String) {
    let word = |key: &str| {
        object
            .get(key)
            .and_then(text)
            .map(|word| word.replace('_', " "))
    };
    if let Some(verdict) = word("verdict") {
        ("Review", verdict)
    } else if let Some(status) = word("status") {
        ("Report", status)
    } else {
        ("Result", String::new())
    }
}

/// Fields that say the result in prose, drawn first and unlabelled.
const PROSE: [&str; 4] = ["answer", "answer_markdown", "summary", "approach"];

/// Fields the title already told, or that are for code alone.
const UNSAID: [&str; 3] = ["status", "verdict", "memory_proposals"];

fn sentences(object: &Map<String, Value>, draw: &Draw<'_>) -> Vec<Line<'static>> {
    let (title, outcome) = title_of(object);
    let mut lines = vec![head(title, &outcome, draw.theme)];
    let mut said: Vec<String> = Vec::new();
    for key in PROSE {
        // A report's answer and its summary often say the same thing; once is enough.
        if let Some(prose) = object.get(key).and_then(Value::as_str)
            && !prose.trim().is_empty()
            && !said.iter().any(|known| known == prose.trim())
        {
            said.push(prose.trim().to_owned());
            lines.extend(indented(prose.trim(), 2, Role::Text, draw));
        }
    }
    for (key, value) in object {
        if PROSE.contains(&key.as_str()) || UNSAID.contains(&key.as_str()) {
            continue;
        }
        lines.extend(field(key, value, 2, draw));
    }
    lines
}

/// `files_changed` reads `Files changed`.
fn label(key: &str) -> String {
    let words = sanitize_line(key).replace('_', " ");
    let mut chars = words.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// One field in words. Nothing is drawn for a field that says nothing: an empty list, an empty
/// text, a `false`, a `null`.
fn field(key: &str, value: &Value, indent: usize, draw: &Draw<'_>) -> Vec<Line<'static>> {
    let name = label(key);
    let said = |words: &str| indented(&format!("{name}: {words}"), indent, Role::Text, draw);
    match value {
        Value::Null | Value::Bool(false) => Vec::new(),
        Value::Bool(true) => said("yes"),
        Value::Number(number) => said(&number.to_string()),
        Value::String(_) => text(value).map(|words| said(&words)).unwrap_or_default(),
        Value::Array(items) => list(&name, items, indent, draw),
        Value::Object(inner) => {
            let mut lines = indented(&name, indent, Role::Dim, draw);
            let before = lines.len();
            for (key, value) in inner {
                lines.extend(field(key, value, indent + 2, draw));
            }
            if lines.len() == before {
                lines.clear();
            }
            lines
        }
    }
}

fn list(name: &str, items: &[Value], indent: usize, draw: &Draw<'_>) -> Vec<Line<'static>> {
    let rows: Vec<String> = items.iter().filter_map(item_words).collect();
    if rows.is_empty() {
        return Vec::new();
    }
    let short = rows.len() <= INLINE_ITEMS
        && rows.iter().map(String::len).sum::<usize>() <= INLINE_CELLS
        && items.iter().all(Value::is_string);
    if short {
        return indented(
            &format!("{name}: {}", listed(&rows)),
            indent,
            Role::Text,
            draw,
        );
    }
    let mut lines = indented(&format!("{name} · {}", rows.len()), indent, Role::Dim, draw);
    let dot = [Span::styled(
        format!("{}· ", " ".repeat(indent + 2)),
        draw.theme.style(Role::Dim),
    )];
    let rest = [Span::raw(" ".repeat(indent + 4))];
    for row in rows.iter().take(LIST_ITEMS) {
        lines.extend(wrap_text(
            row,
            draw.theme.style(Role::Text),
            draw.width,
            &dot,
            &rest,
        ));
    }
    if rows.len() > LIST_ITEMS {
        let more = format!("+{} more", rows.len() - LIST_ITEMS);
        lines.extend(indented(&more, indent + 2, Role::Dim, draw));
    }
    lines
}

/// A list item on one line: a text as it is, an object as its values joined, with a file and
/// line read as `src/due.rs:12`.
fn item_words(item: &Value) -> Option<String> {
    match item {
        Value::Object(object) => {
            let mut words = Vec::new();
            if let Some(path) = object.get("path").and_then(text) {
                words.push(match object.get("line").and_then(Value::as_u64) {
                    Some(line) => format!("{path}:{line}"),
                    None => path,
                });
            }
            for (key, value) in object {
                if key == "path" || key == "line" {
                    continue;
                }
                match value {
                    Value::String(_) => words.extend(text(value)),
                    Value::Number(number) => words.push(format!("{} {number}", label(key))),
                    Value::Bool(true) => words.push(label(key)),
                    _ => {}
                }
            }
            (!words.is_empty()).then(|| words.join(" · "))
        }
        Value::Number(number) => Some(number.to_string()),
        other => text(other),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::text::{plain_text, str_width};
    use crate::theme::Glyphs;

    /// Fields are drawn in the order the object holds them: as written where the build keeps
    /// that order, as the app's does, and by name otherwise. So the fixtures here write their
    /// fields by name, and read the same either way.
    fn drawn(value: &Value, width: usize, expanded: bool) -> (String, bool) {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let names = DisplayNames::default();
        let draw = Draw {
            names: &names,
            theme: &theme,
            width,
            expanded,
            hints: true,
        };
        let (lines, more) = lines(value, &draw).unwrap();
        for line in &lines {
            let row: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(str_width(&row) <= width, "{row:?} is wider than {width}");
        }
        (plain_text(&lines), more)
    }

    fn plan() -> Value {
        json!({
            "plan": {"summary": "Add due dates in three tasks.", "approach": "", "pieces": [],
                     "choices": [], "risks": [], "test_strategy": "", "new_dependencies": []},
            "cards": [
                {"key": "iface", "title": "Due-date stubs", "task": "Define the Due type.",
                 "kind": "interface", "owned_paths": ["src/due.rs"], "read_only_paths": [],
                 "depends_on": [], "criteria": []},
                {"key": "impl", "title": "Store, list, overdue", "task": "Store and list.",
                 "kind": "build", "owned_paths": ["src/store.rs", "src/list.rs"],
                 "read_only_paths": [], "depends_on": ["iface"], "criteria": []},
                {"key": "tests", "title": "Tests for each command", "task": "Cover each.",
                 "kind": "build", "owned_paths": [], "read_only_paths": [],
                 "depends_on": [], "criteria": []}
            ],
            "assignments": [
                {"card": "iface", "owner": "claude-code", "reviewers": ["antigravity-cli/gemini"]},
                {"card": "impl", "owner": "codex", "reviewers": ["antigravity-cli/gemini"]},
                {"card": "tests", "owner": "claude-code", "reviewers": ["antigravity-cli/gemini"]}
            ]
        })
    }

    #[test]
    fn a_plan_is_a_table_of_its_tasks() {
        let (text, more) = drawn(&plan(), 100, false);
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows[0], "● Plan · 3 tasks");
        assert_eq!(
            rows[1],
            "  task   what it does            worker       reviewed by"
        );
        assert_eq!(
            rows[2],
            "  iface  Due-date stubs          Claude Code  Gemini on Antigravity CLI"
        );
        assert_eq!(
            rows[3],
            "  impl   Store, list, overdue    Codex        Gemini on Antigravity CLI"
        );
        assert_eq!(
            rows[4],
            "  tests  Tests for each command  Claude Code  Gemini on Antigravity CLI"
        );
        assert_eq!(rows[5], "  ⎿  impl waits on iface · → expands the tasks");
        assert_eq!(rows.len(), 6, "{text}");
        assert!(more);
        assert!(!text.contains('{'), "{text}");
    }

    #[test]
    fn expanding_a_plan_shows_what_each_task_asks_for() {
        let (text, _) = drawn(&plan(), 100, true);
        assert!(text.contains("    Store and list."), "{text}");
        assert!(
            text.contains("    owns src/store.rs and src/list.rs"),
            "{text}"
        );
        assert!(text.contains("→ folds the tasks"), "{text}");
    }

    #[test]
    fn a_narrow_plan_keeps_the_tasks_and_gives_up_who_reviews() {
        let (text, _) = drawn(&plan(), 48, false);
        assert!(text.contains("iface"), "{text}");
        assert!(text.contains("Claude Code"), "{text}");
        assert!(!text.contains("reviewed by"), "{text}");
        let (text, _) = drawn(&plan(), 30, false);
        assert!(text.contains("tests"), "{text}");
        assert!(!text.contains("reviewed by"), "{text}");
    }

    #[test]
    fn a_drafts_pieces_are_a_plan_with_no_one_assigned() {
        let draft = json!({
            "summary": "Two pieces.",
            "pieces": [
                {"name": "store", "description": "Keep the dates", "paths": [],
                 "interfaces": [], "depends_on": []},
                {"name": "cli", "description": "Show what is overdue", "paths": [],
                 "interfaces": [], "depends_on": ["store"]}
            ]
        });
        let (text, more) = drawn(&draft, 80, false);
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows[0], "● Plan · 2 tasks");
        assert_eq!(rows[1], "  task   what it does");
        assert_eq!(rows[3], "  cli    Show what is overdue");
        assert_eq!(rows[4], "  ⎿  cli waits on store");
        assert!(!more);
    }

    #[test]
    fn a_report_reads_as_sentences() {
        let report = json!({
            "status": "done",
            "summary": "Added the overdue command.",
            "files_changed": ["src/cli.rs", "tests/overdue.rs"],
            "tests_run": ["cargo test overdue: 12 passed"],
            "concerns": [],
            "questions": [],
            "memory_proposals": [{"text": "x"}],
            "answer": "Added the overdue command."
        });
        let (text, more) = drawn(&report, 80, false);
        assert_eq!(
            text,
            "● Report · done\n  Added the overdue command.\n  Files changed: src/cli.rs and \
             tests/overdue.rs\n  Tests run: cargo test overdue: 12 passed"
        );
        assert!(!more);
    }

    #[test]
    fn a_review_names_its_verdict_and_lists_its_comments() {
        let review = json!({
            "verdict": "request_changes",
            "summary": "The date comparison is off by one.",
            "comments": [
                {"body": "Compare dates, not timestamps.", "line": 12, "path": "src/due.rs",
                 "severity": "major"},
                {"body": "Add a task with no date.", "line": null, "path": "tests/overdue.rs",
                 "severity": "minor"}
            ],
            "tamper_suspected": false
        });
        let (text, _) = drawn(&review, 80, false);
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows[0], "● Review · request changes");
        assert_eq!(rows[1], "  The date comparison is off by one.");
        assert_eq!(rows[2], "  Comments · 2");
        assert_eq!(
            rows[3],
            "    · src/due.rs:12 · Compare dates, not timestamps. · major"
        );
        assert_eq!(
            rows[4],
            "    · tests/overdue.rs · Add a task with no date. · minor"
        );
        assert!(!text.contains("Tamper"), "{text}");
    }

    #[test]
    fn a_shape_nothing_knows_still_reads_as_words() {
        let value = json!({
            "chosen": "b",
            "notes": ["one", "two"],
            "scores": {"a": 3, "b": 5},
            "ties_broken": true
        });
        let (text, _) = drawn(&value, 60, false);
        assert_eq!(
            text,
            "● Result\n  Chosen: b\n  Notes: one and two\n  Scores\n    A: 3\n    B: 5\n  Ties \
             broken: yes"
        );
    }

    #[test]
    fn text_that_is_not_an_object_is_left_to_be_drawn_as_text() {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let names = DisplayNames::default();
        let draw = Draw {
            names: &names,
            theme: &theme,
            width: 60,
            expanded: false,
            hints: true,
        };
        assert!(lines(&json!(["a", "b"]), &draw).is_none());
        assert!(lines(&json!("plain"), &draw).is_none());
    }
}
