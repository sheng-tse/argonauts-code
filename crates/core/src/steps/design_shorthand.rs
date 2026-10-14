//! The main agent's brief as a cheap model writes it, read the one way each near-miss can mean
//! before the schema checks it: a check written as `"diff"` or as a test's name, a criterion with
//! no short name or a repeated id, a run named in five words, a list of one given as the item
//! itself. Whatever can't be read this way stays as it came, so the schema or code sends it back to
//! the main agent.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};
use unicode_width::UnicodeWidthStr;

use crate::labels;

/// Words a run's name can lose without losing what the task is.
const FILLER: &[&str] = &[
    "a", "an", "and", "at", "by", "for", "from", "in", "into", "its", "of", "on", "per", "the",
    "to", "with",
];

/// Checks written as text that mean a reading of the diff.
const REVIEW: &[&str] = &[
    "review",
    "diff",
    "read",
    "reading",
    "code review",
    "diff review",
    "read the diff",
    "review the diff",
    "reading of the diff",
    "a reading of the diff",
];

/// Programs a test command starts with, so a check written as text that starts with one is a
/// command. None is an everyday word.
const RUNNERS: &[&str] = &[
    "./gradlew",
    "bun",
    "cargo",
    "ctest",
    "deno",
    "dotnet",
    "gradle",
    "jest",
    "mocha",
    "mvn",
    "npm",
    "npx",
    "nox",
    "pnpm",
    "pytest",
    "python",
    "python3",
    "rspec",
    "tox",
    "vitest",
    "yarn",
];

/// Programs whose names are everyday words, a command only before one of these, as in
/// `make test` or `go vet ./...`.
const WORD_RUNNERS: &[&str] = &["go", "make"];
const WORD_RUNNER_TASKS: &[&str] = &["test", "tests", "check", "lint", "vet", "ci", "verify"];

/// Other programs a check's command may start with, none an everyday word.
const PROGRAMS: &[&str] = &[
    "bash", "bazel", "bundle", "cabal", "dart", "elixir", "flutter", "hatch", "julia", "lein",
    "mix", "nix", "node", "perl", "php", "phpunit", "poetry", "rake", "ruby", "sbt", "sh", "swift",
    "uv", "zig", "zsh",
];

/// Words that make a run of plain words a sentence rather than a command, such as `run the
/// tests` or `tests pass`.
const PROSE: &[&str] = &[
    "all", "are", "be", "ensure", "is", "it", "must", "pass", "passes", "run", "runs", "should",
    "succeed", "succeeds", "sure", "verify", "work", "works",
];

/// How many words a run's name may have.
const NAME_WORDS: usize = 4;
/// The words and width a derived short name stays within.
const SHORT_WORDS: usize = 3;
const SHORT_CELLS: usize = 24;

/// The brief with its readable near-misses read. Anything that isn't an object is left alone.
pub fn design_brief(value: Value) -> Value {
    let Value::Object(mut brief) = value else {
        return value;
    };
    for list in ["params", "plan", "questions"] {
        brief.entry(list).or_insert_with(|| json!([]));
    }
    brief.entry("first_try").or_insert(Value::Null);
    // One of the two says what runs, so only the other may be left out.
    let given = |key: &str| brief.get(key).is_some_and(|value| !value.is_null());
    if given("based_on") || given("workflow") {
        for nullable in ["based_on", "workflow"] {
            brief.entry(nullable).or_insert(Value::Null);
        }
    }
    if brief.get("why").is_none_or(Value::is_null) {
        brief.insert("why".to_owned(), json!(""));
    }
    for list in ["done_when", "plan", "questions"] {
        if let Some(value) = brief.get_mut(list) {
            as_list(value);
        }
    }
    if let Some(params) = brief.get_mut("params") {
        read_params(params);
    }
    if let Some(Value::String(description)) = brief.get_mut("description") {
        *description = first_line(description);
    }
    let description = brief
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let name = brief
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let name = run_name(name, &description);
    if !name.is_empty() {
        brief.insert("name".to_owned(), Value::String(name));
    }
    if let Some(workflow) = brief.get_mut("workflow") {
        as_yaml(workflow);
    }
    if let Some(Value::Object(first)) = brief.get_mut("first_try")
        && let Some(steps) = first.get_mut("steps")
    {
        as_yaml(steps);
    }
    if let Some(Value::Array(criteria)) = brief.get_mut("done_when") {
        for criterion in criteria.iter_mut() {
            read_criterion(criterion);
        }
        number(criteria);
    }
    if let Some(Value::Array(tasks)) = brief.get_mut("plan") {
        tasks.iter_mut().for_each(read_task);
    }
    if let Some(Value::Array(questions)) = brief.get_mut("questions") {
        for question in questions.iter_mut() {
            if let Some(options) = question.get_mut("options") {
                as_list(options);
            }
        }
    }
    Value::Object(brief)
}

/// A list of one written as the item itself, or none written as `null`.
fn as_list(value: &mut Value) {
    match value {
        Value::Array(_) => {}
        Value::Null => *value = json!([]),
        _ => *value = Value::Array(vec![value.take()]),
    }
}

/// Params written as a map of name to value, and values written as numbers or booleans.
fn read_params(params: &mut Value) {
    if let Value::Object(map) = params {
        let one = map.len() == 2 && map.contains_key("name") && map.contains_key("value");
        *params = if one {
            Value::Array(vec![params.take()])
        } else {
            Value::Array(
                map.iter()
                    .map(|(name, value)| json!({ "name": name, "value": value }))
                    .collect(),
            )
        };
    }
    as_list(params);
    let Value::Array(settings) = params else {
        return;
    };
    for setting in settings {
        if let Some(value) = setting.get_mut("value") {
            match value {
                Value::Number(_) | Value::Bool(_) => *value = Value::String(value.to_string()),
                Value::Null => *value = json!(""),
                _ => {}
            }
        }
    }
}

/// A workflow or a phase's steps written as JSON rather than YAML text. JSON is YAML, so its
/// text reads the same.
fn as_yaml(value: &mut Value) {
    if matches!(value, Value::Object(_) | Value::Array(_)) {
        *value = Value::String(value.to_string());
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// Lowercase words joined with `-`, less filler words when there are more than four, or the
/// description's first words when the main agent left the name out. More than four words that all
/// matter stay, and the rule sends them back.
fn run_name(name: &str, description: &str) -> String {
    let mut words = words(name);
    if words.is_empty() {
        words = words_of_note(description);
        words.truncate(NAME_WORDS);
    } else if words.len() > NAME_WORDS {
        let kept = words_of_note(name);
        if (2..=NAME_WORDS).contains(&kept.len()) {
            words = kept;
        }
    }
    if words.is_empty() {
        return name.trim().to_owned();
    }
    words.join("-")
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn words_of_note(text: &str) -> Vec<String> {
    words(text)
        .into_iter()
        .filter(|word| !FILLER.contains(&word.as_str()))
        .collect()
}

/// The plain form two spellings of one example's name share: `Council`, `council` and
/// `council` are all `council`. A name with other marks stays as written.
pub(crate) fn example_name(name: &str) -> String {
    let trimmed = name.trim();
    let plain = trimmed
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '));
    if !plain {
        return trimmed.to_owned();
    }
    words(trimmed).join("-")
}

/// One done-when criterion: a sentence alone, a check in shorthand, a missing short name.
fn read_criterion(criterion: &mut Value) {
    if let Value::String(text) = criterion {
        *criterion = json!({ "text": text });
    }
    let Value::Object(map) = criterion else {
        return;
    };
    let check = match map.get("check") {
        None | Some(Value::Null) => Some(json!("review")),
        Some(Value::String(text)) => check_from_text(text),
        Some(Value::Object(check)) => check_from_object(check),
        Some(_) => None,
    };
    if let Some(check) = check {
        map.insert("check".to_owned(), check);
    }
    let short = map.get("short").and_then(Value::as_str).map(str::trim);
    if short.is_none_or(str::is_empty) {
        let text = map.get("text").and_then(Value::as_str).unwrap_or_default();
        let derived = short_from(text);
        if !derived.is_empty() {
            map.insert("short".to_owned(), Value::String(derived));
        }
    }
}

/// `"diff"` is a reading of the diff, `"test_median_empty"` a test, and `"pytest tests/"` a
/// command. Plain prose isn't read as any of them.
fn check_from_text(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    let lower = trimmed.trim_end_matches('.').to_lowercase();
    if REVIEW.contains(&lower.as_str()) {
        return Some(json!("review"));
    }
    let mut words = lower.split_whitespace();
    let (first, second) = (words.next().unwrap_or_default(), words.next());
    let command = RUNNERS.contains(&first)
        || (WORD_RUNNERS.contains(&first)
            && second.is_some_and(|task| WORD_RUNNER_TASKS.contains(&task)));
    if command {
        return Some(json!({ "command": trimmed }));
    }
    // A test's name, not the word alone, which says only that tests run.
    let one_token = !trimmed.is_empty() && !trimmed.contains(char::is_whitespace);
    let bare = matches!(lower.as_str(), "test" | "tests" | "tested" | "testing");
    (one_token && !bare && lower.contains("test")).then(|| json!({ "test": trimmed }))
}

/// Whether `name` can be a test's name as its runner knows it: one token with a word in it,
/// such as `test_cap` or `tests/test_bucket.py::test_cap`. Prose such as `test passes`, or the
/// word `tests` alone, names no test.
pub fn names_a_test(name: &str) -> bool {
    let name = name.trim();
    let bare = matches!(
        name.to_lowercase().as_str(),
        "test" | "tests" | "tested" | "testing"
    );
    let word = name
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|part| part.starts_with(|c: char| c.is_alphabetic() || c == '_'));
    word && !bare && !name.contains(char::is_whitespace)
}

/// Whether `command` reads as a command rather than a sentence such as `run the tests`: it
/// starts with a program, or has a flag, a path, or anything else no sentence has.
pub fn starts_with_program(command: &str) -> bool {
    let lower = command.trim().to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let Some(first) = words.first() else {
        return false;
    };
    let known = RUNNERS.contains(first)
        || PROGRAMS.contains(first)
        || (WORD_RUNNERS.contains(first)
            && words
                .get(1)
                .is_some_and(|task| WORD_RUNNER_TASKS.contains(task)));
    let plain = words.iter().all(|word| {
        word.trim_end_matches(['.', ','])
            .chars()
            .all(char::is_alphabetic)
    });
    let sentence = words.iter().any(|word| {
        let word = word.trim_end_matches(['.', ',']);
        PROSE.contains(&word) || FILLER.contains(&word)
    });
    known || !(plain && words.len() > 1 && sentence)
}

/// A check object under another key: `{"tests": …}`, `{"cmd": …}`, `{"review": true}`, or a
/// `type` that names a reading of the diff.
fn check_from_object(check: &Map<String, Value>) -> Option<Value> {
    if check.len() != 1 {
        return None;
    }
    let (key, value) = check.iter().next()?;
    let text = value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    match key.as_str() {
        "tests" | "test_name" => text.map(|test| json!({ "test": test })),
        "cmd" | "run" | "shell" => text.map(|command| json!({ "command": command })),
        "review" | "diff" => Some(json!("review")),
        "type" | "kind" | "by" | "method" => text
            .and_then(check_from_text)
            .filter(|check| check == "review"),
        _ => None,
    }
}

/// A few words for narrow places, from the criterion's sentence: `Rate limiter creates` from
/// "Rate limiter creates separate buckets for each key."
fn short_from(text: &str) -> String {
    let mut short = String::new();
    let words = text
        .split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|word| !word.is_empty())
        .skip_while(|word| matches!(word.to_lowercase().as_str(), "a" | "an" | "the"));
    for word in words.take(SHORT_WORDS) {
        let next = if short.is_empty() {
            word.to_owned()
        } else {
            format!("{short} {word}")
        };
        if next.width() > SHORT_CELLS && !short.is_empty() {
            break;
        }
        short = next;
    }
    crate::text::sentence_case(&short)
}

/// `G1`, `G2`, ... in order, when an id is missing, not text, or used twice.
fn number(criteria: &mut [Value]) {
    let ids: Vec<Option<&str>> = criteria
        .iter()
        .map(|criterion| {
            criterion
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
        })
        .collect();
    let distinct: BTreeSet<&str> = ids.iter().flatten().copied().collect();
    if distinct.len() == criteria.len() {
        return;
    }
    for (n, criterion) in criteria.iter_mut().enumerate() {
        if let Value::Object(map) = criterion {
            map.insert("id".to_owned(), Value::String(format!("G{}", n + 1)));
        }
    }
}

/// A plan task with no item takes its task's first words, as a task's title gives its item.
fn read_task(task: &mut Value) {
    let Value::Object(map) = task else {
        return;
    };
    let item = map.get("item").and_then(Value::as_str).map(str::trim);
    if item.is_none_or(str::is_empty) {
        let text = map.get("task").and_then(Value::as_str).unwrap_or_default();
        let derived = labels::item_from_title(text, "");
        if !derived.is_empty() {
            map.insert("item".to_owned(), Value::String(derived));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::steps::design::{DesignOutput, DesignWorkflow};
    use crate::steps::{Step, output_schema};

    fn valid(value: &Value) -> Vec<String> {
        let schema = output_schema::<DesignWorkflow>();
        let validator = jsonschema::validator_for(&schema).unwrap();
        validator
            .iter_errors(value)
            .map(|e| e.to_string())
            .collect()
    }

    fn read(value: Value) -> Value {
        DesignWorkflow::normalize(value)
    }

    /// Haiku's first brief for a council run on 10-04 named it `rate-limiter-per-api-key`,
    /// which code refused for its five words, and a new session started over.
    #[test]
    fn a_name_in_five_words_loses_its_filler() {
        let brief = read(json!({
            "name": "rate-limiter-per-api-key",
            "description": "Make rate limiter per API key with Retry-After headers and tests.",
            "based_on": "council", "why": "", "workflow": null,
            "done_when": [{"id": "G1", "short": "Per-API-key buckets", "check": "review",
                           "text": "Rate limiter creates separate buckets for each X-Api-Key value."}],
        }));
        assert_eq!(valid(&brief), Vec::<String>::new());
        assert_eq!(brief["name"], "rate-limiter-api-key");
    }

    #[test]
    fn checks_in_shorthand_read_as_the_schema_says() {
        let checks = [
            (json!("review"), json!("review")),
            (json!("Diff"), json!("review")),
            (json!("read the diff."), json!("review")),
            (json!(null), json!("review")),
            (
                json!("test_median_empty"),
                json!({"test": "test_median_empty"}),
            ),
            (
                json!("tests/test_bucket.py::test_cap"),
                json!({"test": "tests/test_bucket.py::test_cap"}),
            ),
            (json!("pytest tests/"), json!({"command": "pytest tests/"})),
            (
                json!("cargo test -p core"),
                json!({"command": "cargo test -p core"}),
            ),
            (json!({"tests": "test_cap"}), json!({"test": "test_cap"})),
            (json!("make test"), json!({"command": "make test"})),
            (
                json!({"cmd": "make check"}),
                json!({"command": "make check"}),
            ),
            (json!({"type": "review"}), json!("review")),
        ];
        for (written, read_as) in checks {
            let brief = read(json!({
                "name": "fix-it", "description": "Fix it", "based_on": "captain",
                "done_when": [{"id": "G1", "short": "It", "text": "It works", "check": written}],
            }));
            assert_eq!(brief["done_when"][0]["check"], read_as, "{written}");
            assert_eq!(valid(&brief), Vec::<String>::new(), "{written}");
        }
    }

    #[test]
    fn prose_where_a_check_goes_stays_and_the_schema_refuses_it() {
        let brief = read(json!({
            "name": "fix-it", "description": "Fix it", "based_on": "captain",
            "done_when": [{"id": "G1", "short": "It", "text": "It works",
                           "check": "make sure it works"}],
        }));
        assert_eq!(brief["done_when"][0]["check"], "make sure it works");
        assert!(!valid(&brief).is_empty());
    }

    #[test]
    fn a_criterion_gets_a_short_name_and_an_id_it_left_out() {
        let brief = read(json!({
            "name": "fix-median", "description": "Fix median", "based_on": "captain",
            "done_when": [
                "median([]) raises ValueError",
                {"id": "G1", "text": "The median of an even list averages the middle pair",
                 "check": "review"},
                {"id": "G1", "short": "", "text": "Tests pass", "check": {"command": "pytest"}},
            ],
        }));
        assert_eq!(valid(&brief), Vec::<String>::new());
        let brief: DesignOutput = serde_json::from_value(brief).unwrap();
        let ids: Vec<&str> = brief.done_when.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["G1", "G2", "G3"]);
        let shorts: Vec<&str> = brief.done_when.iter().map(|c| c.short.as_str()).collect();
        assert_eq!(
            shorts,
            ["Median raises ValueError", "Median of an", "Tests pass"]
        );
        assert_eq!(brief.done_when[0].text, "median([]) raises ValueError");
    }

    #[test]
    fn names_are_lowercase_words_and_a_missing_one_comes_from_the_description() {
        let named = |name: &str| {
            let brief = read(json!({"name": name, "description": "Add a JSON flag to the export"}));
            brief["name"].as_str().unwrap().to_owned()
        };
        assert_eq!(named("fix-slugify-median"), "fix-slugify-median");
        assert_eq!(named("Fix Slugify_Median"), "fix-slugify-median");
        assert_eq!(named("add-a-json-flag-to-export"), "add-json-flag-export");
        assert_eq!(named(""), "add-json-flag-export");
        let unnamed = read(json!({"description": "Fix the slugify dashes"}));
        assert_eq!(unnamed["name"], "fix-slugify-dashes");
        // Five words that all matter stay, for the rule to send back.
        assert_eq!(
            named("split-parse-render-cache-layers"),
            "split-parse-render-cache-layers"
        );
    }

    #[test]
    fn lists_of_one_json_workflows_and_left_out_fields_read_as_meant() {
        let brief = read(json!({
            "name": "look-once",
            "description": "Look once\nthen stop",
            "based_on": "Council",
            "params": {"check": "diff", "rounds": 2},
            "workflow": {"name": "look", "phases": [{"name": "Look", "steps": []}]},
            "done_when": {"id": "G1", "short": "Looked", "text": "It looked", "check": "review"},
            "plan": {"task": "Collapse each run of dashes"},
            "questions": [{"topic": "Dashes", "text": "Trim?", "options": "Yes"}],
            "first_try": {"name": "Fix", "detail": "a worker fixes it",
                              "steps": [{"kind": "build", "label": "fix:it"}]},
        }));
        assert_eq!(valid(&brief), Vec::<String>::new());
        let brief: DesignOutput = serde_json::from_value(brief).unwrap();
        assert_eq!(brief.description, "Look once");
        // Only code that knows the examples' names can read `based_on`.
        assert_eq!(brief.based_on.as_deref(), Some("Council"));
        let params: Vec<(&str, &str)> = brief
            .params
            .iter()
            .map(|p| (p.name.as_str(), p.value.as_str()))
            .collect();
        assert_eq!(params, [("check", "diff"), ("rounds", "2")]);
        assert!(brief.workflow.unwrap().contains("\"phases\""));
        assert_eq!(brief.done_when.len(), 1);
        assert_eq!(brief.plan[0].item, "collapse-each");
        assert_eq!(brief.questions[0].options, ["Yes"]);
        assert!(brief.first_try.unwrap().steps.contains("fix:it"));
        assert_eq!(brief.why, "");
    }

    /// Prose given as a test's name or a command stays as written, for code to send back: the
    /// 10-04 brief's G3 and G5 were `{"test": "test passes"}`, which nothing can run.
    #[test]
    fn prose_in_a_check_object_names_no_test_and_no_command() {
        let brief = read(json!({
            "name": "fix-it", "description": "Fix it", "based_on": "captain",
            "done_when": [{"id": "G3", "short": "Capped", "text": "Refills are capped",
                           "check": {"test": "test passes"}}],
        }));
        assert_eq!(
            brief["done_when"][0]["check"],
            json!({"test": "test passes"})
        );
        for name in ["test passes", "tests", "caps refills at capacity", "", "::"] {
            assert!(!names_a_test(name), "{name}");
        }
        for name in [
            "test_cap",
            "tests/test_bucket.py::test_cap",
            "TestBucket",
            "cap_spec",
        ] {
            assert!(names_a_test(name), "{name}");
        }
        for command in [
            "run the tests",
            "tests pass",
            "make sure it works",
            "",
            "all good.",
        ] {
            assert!(!starts_with_program(command), "{command}");
        }
        for command in [
            "python -m pytest tests/",
            "cargo test",
            "make check",
            "swift test",
            "just test",
            "./run_tests.sh",
            "pytest",
        ] {
            assert!(starts_with_program(command), "{command}");
        }
    }

    /// Example names are file stems, so your own `quick_fix.yaml` goes by `quick_fix`, and only
    /// another spelling of exactly one example's name is read as it.
    #[test]
    fn based_on_reads_as_the_one_example_it_spells() {
        let examples: Vec<String> = ["council", "quick_fix", "Hotfix", "look_once", "look-once"]
            .map(str::to_owned)
            .into();
        let read_as = |written: &str| {
            let mut brief: DesignOutput = serde_json::from_value(read(json!({
                "name": "fix-it", "description": "Fix it", "based_on": written,
                "done_when": [{"id": "G1", "short": "It", "text": "It works", "check": "review"}],
            })))
            .unwrap();
            brief.read_based_on(&examples);
            brief.based_on.unwrap_or_default()
        };
        assert_eq!(read_as("Council"), "council");
        assert_eq!(read_as("quick_fix"), "quick_fix");
        assert_eq!(read_as("Quick Fix"), "quick_fix");
        assert_eq!(read_as("Hotfix"), "Hotfix");
        assert_eq!(read_as("hotfix"), "Hotfix");
        // Two examples spelled alike: only the exact name picks one.
        assert_eq!(read_as("look_once"), "look_once");
        assert_eq!(read_as("Look Once"), "Look Once");
        assert_eq!(read_as("nonesuch"), "nonesuch");

        // A brief read back from the log keeps the main agent's spelling, which still names the
        // example it ran.
        let written: DesignOutput = serde_json::from_value(read(json!({
            "name": "fix-it", "description": "Fix it", "based_on": "Quick Fix",
            "done_when": [{"id": "G1", "short": "It", "text": "It works", "check": "review"}],
        })))
        .unwrap();
        assert!(written.based_on_names("quick_fix") && written.based_on_names("quick-fix"));
        assert!(!written.based_on_names("council"));
    }

    #[test]
    fn a_brief_that_says_nothing_runs_stays_refused() {
        let brief = read(json!({
            "name": "look-once", "description": "Look once",
            "done_when": [{"id": "G1", "short": "Looked", "text": "It looked", "check": "review"}],
        }));
        assert!(brief.get("based_on").is_none() && brief.get("workflow").is_none());
        assert!(!valid(&brief).is_empty());
    }
}
