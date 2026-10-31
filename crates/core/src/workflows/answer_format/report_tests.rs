//! A build report whose fields ran on inside its answer, as Haiku 5.5 on Claude Code writes one
//! when it writes the long `answer` first and closes it with `</answer>`: the fields after it
//! arrive as `<parameter>` blocks inside the string, the last one often unclosed.

use serde_json::{Value, json};

use super::{fits, mended_to_fit, strict};
use crate::workflows::steps::output_schema;
use crate::workflows::steps::task::{Build, TaskReport};

fn format() -> Value {
    output_schema::<Build>()
}

const FILES: [&str; 4] = [
    "src/store.js",
    "src/cli.js",
    "test/store.test.js",
    "test/cli.test.js",
];

/// The first try's shape: every field the format requires is there, and only a list with a
/// default leaked.
fn first_try() -> Value {
    json!({
        "answer": "Added `--due` to `tasks add`, a due date in `tasks list`, and `tasks overdue`. \
                   `node --test` passes 11 tests.</answer>\n<parameter name=\"change_requests\">[]",
        "concerns": ["Assumed: `overdue` uses the local date."],
        "files_changed": FILES,
        "memory_proposals": [],
        "questions": [],
        "status": "done",
        "summary": "Due dates in the tasks CLI, with tests.",
        "tests_run": ["node --test: 11 pass, 0 fail"]
    })
}

/// A last try's shape: the required lists are inside the answer too, each closed but the last.
fn last_try() -> Value {
    json!({
        "answer": "Added --due, a due date in list, and overdue. node --test passes 11 tests.\
                   </answer>\n<parameter name=\"change_requests\">[]</parameter>\n\
                   <parameter name=\"concerns\">[\"Assumed: --due may come anywhere after add.\", \
                   \"Assumed: overdue uses the local date.\"]</parameter>\n\
                   <parameter name=\"files_changed\">[\"src/store.js\", \"src/cli.js\", \
                   \"test/store.test.js\", \"test/cli.test.js\"]</parameter>\n\
                   <parameter name=\"memory_proposals\">[]",
        "questions": [],
        "status": "done",
        "summary": "Due dates in the tasks CLI, with tests. Nothing was committed.",
        "tests_run": ["node --test: 11 pass, 0 fail"]
    })
}

/// Claude Code gets the report's format as written, so the first try passes its check, and
/// the mend then takes the leaked list out of the answer.
#[test]
fn a_first_try_that_leaked_only_a_list_with_a_default_passes_and_is_mended() {
    assert!(fits(&format(), &first_try()));
    let all_required = crate::workflows::steps::strict_schema(format());
    assert!(
        !fits(&all_required, &first_try()),
        "with every field required, Claude Code refused it"
    );
    let mended = mended_to_fit(&format(), &first_try()).expect("mended");
    assert_eq!(mended["change_requests"], json!([]));
    assert!(
        mended["answer"]
            .as_str()
            .unwrap()
            .ends_with("passes 11 tests.")
    );
    assert!(fits(&format(), &mended));
}

/// A try that misses the required lists is whole once mended: the session ends done with its
/// report.
#[test]
fn a_last_try_that_leaked_required_fields_mends_into_a_whole_report() {
    assert!(!fits(&format(), &last_try()));
    let mended = mended_to_fit(&format(), &last_try()).expect("mended");
    let report: TaskReport = serde_json::from_value(mended).unwrap();
    assert_eq!(report.files_changed, FILES);
    assert_eq!(
        report.assumptions(),
        [
            "--due may come anywhere after add",
            "overdue uses the local date"
        ]
    );
    assert!(report.change_requests.is_empty() && report.memory_proposals.is_empty());
    assert_eq!(
        report.answer,
        "Added --due, a due date in list, and overdue. node --test passes 11 tests."
    );
}

/// A required field that is nowhere, not even inside the answer, leaves the try a miss.
#[test]
fn a_try_missing_a_required_field_everywhere_stays_a_miss() {
    let mut missing = last_try();
    let answer = missing["answer"].as_str().unwrap();
    let (kept, _) = answer
        .split_once("\n<parameter name=\"files_changed\">")
        .unwrap();
    missing["answer"] = Value::String(kept.to_owned());
    assert_eq!(mended_to_fit(&format(), &missing), None);
}

/// Codex's strict format asks for every field, the lists with a default as `null` allowed, and
/// the long answer last.
#[test]
fn codex_is_asked_for_every_field_with_the_answer_last() {
    let strict = strict(&format());
    let required = strict["required"].as_array().unwrap();
    assert_eq!(required.last(), Some(&json!("answer")));
    assert_eq!(
        strict["properties"]["change_requests"]["type"],
        json!(["array", "null"])
    );
}
