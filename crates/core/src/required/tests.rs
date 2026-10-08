use std::path::Path;

use super::*;

/// A file of required steps, as you'd write one.
const PICTURED: &str = r#"
# Whoever writes a workflow, you or the orchestrator or Claude Code, these steps are in it.
[[step]]
name = "Benchmark"
role = "workers"
does = "Run the benchmarks and report any slowdown over 5%."
when = "before sign-off"          # first · after plan · after build · before sign-off · last

[[step]]
name = "Secrets scan"
role = "reviewers"
does = "Scan the change for keys, tokens and passwords."
when = "after build"
"#;

/// A request that fixes the task list.
const THREE_TASKS: &str = "for this one, do exactly these three tasks, in this order: migrate \
    the schema, backfill the old rows, switch reads to the new table. don't split it \
    differently.";

fn names(required: &Required) -> Vec<&str> {
    required
        .steps
        .iter()
        .map(|step| step.name.as_str())
        .collect()
}

fn write(folder: &Path, text: &str) {
    std::fs::create_dir_all(folder).expect("folder");
    std::fs::write(folder.join(FILE), text).expect("file");
}

#[test]
fn the_pictured_file_requires_its_two_steps() {
    let steps = parse(PICTURED).expect("valid");
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].name, "Benchmark");
    assert_eq!(steps[0].role, CrewRole::Workers);
    assert_eq!(steps[0].when.as_deref(), Some("before sign-off"));
    assert_eq!(steps[1].role, CrewRole::Reviewers);
    assert_eq!(
        steps[1].does.as_deref(),
        Some("Scan the change for keys, tokens and passwords.")
    );
}

#[test]
fn every_problem_in_a_file_is_named_at_once() {
    let problems = parse(
        r#"
[[step]]
name = "Benchmark"
role = "tester"

[[step]]
name = "Scan"
when = "sometime"

[[step]]
name = "benchmark"

[[step]]
name = "  "
"#,
    )
    .expect_err("invalid");
    assert_eq!(problems.len(), 4, "{problems:?}");
    assert!(problems[0].contains("\"tester\""), "{problems:?}");
    assert!(problems[1].contains("before <step>"), "{problems:?}");
    assert!(problems[2].contains("two steps"), "{problems:?}");
    assert!(problems[3].contains("no name"), "{problems:?}");
    let typo = parse("[[step]]\nname = \"x\"\nrol = \"workers\"\n").expect_err("unknown key");
    assert!(typo[0].contains("rol"), "{typo:?}");
}

#[test]
fn a_step_left_bare_goes_to_the_workers_before_the_sign_off() {
    let steps = parse("[[step]]\nname = \"Lint\"\nrole = \"reviewer\"\n[[step]]\nname = \"x\"\n")
        .expect("valid");
    assert_eq!(steps[0].role, CrewRole::Reviewers);
    assert_eq!(steps[1].role, CrewRole::Workers);
    assert_eq!(steps[1].when, None);
    assert!(brief::step_line(&steps[1]).contains("before the sign-off"));
}

#[test]
fn what_the_setup_writes_reads_back_the_same() {
    let steps = parse(PICTURED).expect("valid");
    assert_eq!(parse(&steps_toml(&steps)).expect("valid"), steps);
    assert!(parse(STARTER).expect("only comments").is_empty());
}

#[test]
fn the_repository_wins_over_yours_and_the_request_over_both() {
    let home = tempfile::tempdir().expect("dir");
    let yours = home.path().join("yours");
    let project = home.path().join("project");
    write(
        &yours,
        "[[step]]\nname = \"Benchmark\"\nwhen = \"last\"\n[[step]]\nname = \"Changelog\"\n",
    );
    write(&project, PICTURED);
    let read = super::read(
        &[&yours, &project, &home.path().join("none")],
        "every workflow must start with a benchmark",
    );
    assert!(read.problems.is_empty(), "{:?}", read.problems);
    assert_eq!(
        names(&read.required),
        ["Benchmark", "Changelog", "Secrets scan"]
    );
    let benchmark = read.required.step("benchmark").expect("benchmark");
    assert_eq!(benchmark.when.as_deref(), Some("first"));
}

#[test]
fn a_step_the_setup_kept_wins_over_the_files_and_the_request_over_it() {
    let home = tempfile::tempdir().expect("dir");
    write(home.path(), PICTURED);
    let kept = |name: &str, when: &str| RequiredStep {
        name: name.to_owned(),
        role: CrewRole::Workers,
        does: None,
        when: Some(when.to_owned()),
    };
    let read = super::read_with(
        &[home.path()],
        &[kept("Benchmark", "after build"), kept("Changelog", "last")],
        "add a flag",
    );
    assert_eq!(
        names(&read.required),
        ["Benchmark", "Secrets scan", "Changelog"]
    );
    let benchmark = read.required.step("benchmark").expect("benchmark");
    assert_eq!(benchmark.when.as_deref(), Some("after build"));

    let said = super::read_with(
        &[home.path()],
        &[kept("Benchmark", "after build")],
        "every workflow must start with a benchmark",
    );
    let benchmark = said.required.step("benchmark").expect("benchmark");
    assert_eq!(benchmark.when.as_deref(), Some("first"));
}

#[test]
fn a_broken_file_counts_for_nothing_and_says_so() {
    let home = tempfile::tempdir().expect("dir");
    write(
        home.path(),
        "[[step]]\nname = \"Scan\"\nrole = \"nobody\"\n",
    );
    let read = super::read(&[home.path()], "add a flag");
    assert!(read.required.is_empty());
    assert_eq!(read.problems.len(), 1);
    assert!(read.problems[0].contains(FILE), "{:?}", read.problems);
    assert!(read.problems[0].contains("nobody"), "{:?}", read.problems);
}

#[test]
fn the_pictured_request_fixes_its_three_tasks() {
    let read = super::read(&[], THREE_TASKS);
    assert!(read.problems.is_empty(), "{:?}", read.problems);
    assert_eq!(
        read.required.tasks,
        [
            "migrate the schema",
            "backfill the old rows",
            "switch reads to the new table"
        ]
    );
    let labels: Vec<String> = read.required.tasks.iter().map(|task| label(task)).collect();
    assert_eq!(labels, ["migrate-schema", "backfill-rows", "switch-reads"]);
}

#[test]
fn a_task_list_can_be_lines_or_a_sentence() {
    let lines = super::read(
        &[],
        "Do exactly these tasks:\n\n1. Add the column\n2) Fill it\n- Read from it\n\nThanks.",
    );
    assert_eq!(
        lines.required.tasks,
        ["Add the column", "Fill it", "Read from it"]
    );
    let joined = super::read(&[], "only these 3 tasks: write a, test b and ship c.");
    assert_eq!(joined.required.tasks, ["write a", "test b", "ship c"]);
    let semicolons = super::read(&[], "just the following tasks: a, b; c and then d");
    assert_eq!(semicolons.required.tasks, ["a, b", "c and then d"]);
}

#[test]
fn a_count_the_list_disagrees_with_fixes_nothing() {
    let read = super::read(&[], "do exactly these two tasks: a, b, c");
    assert!(read.required.tasks.is_empty());
    assert_eq!(read.problems.len(), 1);
    assert!(read.problems[0].contains("listed 3"), "{:?}", read.problems);
}

#[test]
fn words_without_a_list_fix_no_tasks() {
    for request in [
        "do exactly what the tests ask",
        "only these tasks matter. the rest: later",
        "run tasks in parallel: a, b",
        "fix the three tasks that fail",
    ] {
        let read = super::read(&[], request);
        assert!(read.required.tasks.is_empty(), "{request}");
        assert!(read.problems.is_empty(), "{request}");
    }
}

#[test]
fn a_request_can_say_a_step_every_workflow_must_contain() {
    let read = super::read(
        &[],
        "Add due dates. Every workflow must end with a benchmark run. Each run has to include \
         a secrets scan after the build; and always include a security review step before \
         the sign-off.",
    );
    let required = read.required;
    assert_eq!(
        names(&required),
        ["Benchmark run", "Secrets scan", "Security review"]
    );
    let [benchmark, scan, review] = required.steps.as_slice() else {
        panic!("three steps");
    };
    assert_eq!(benchmark.when.as_deref(), Some("last"));
    assert_eq!(benchmark.role, CrewRole::Workers);
    assert_eq!(
        benchmark.does.as_deref(),
        Some("Every workflow must end with a benchmark run.")
    );
    assert_eq!(scan.when.as_deref(), Some("after build"));
    assert_eq!(scan.role, CrewRole::Reviewers);
    assert_eq!(review.when.as_deref(), Some("before sign-off"));
    assert_eq!(review.role, CrewRole::Reviewers);
}

#[test]
fn words_about_the_code_require_no_step() {
    for request in [
        "make sure the file must end with a newline",
        "the test run must end with exit code 0",
        "every workflow must not end with a deploy",
        "the workflow doesn't have to include a review",
    ] {
        let read = super::read(&[], request);
        assert!(
            read.required.steps.is_empty(),
            "{request}: {:?}",
            read.required
        );
    }
}

fn pictured() -> Required {
    let mut required = Required {
        steps: parse(PICTURED).expect("valid"),
        tasks: Vec::new(),
    };
    required.tasks = super::read(&[], THREE_TASKS).required.tasks;
    required
}

#[test]
fn a_workflow_with_every_step_in_place_passes() {
    let found = check::steps(
        &pictured(),
        &[
            "Plan",
            "Build",
            "Secrets scan",
            "Review",
            "Benchmark",
            "Sign off",
        ],
    );
    assert!(found.is_empty(), "{found:?}");
    assert_eq!(check::ask_to_place(&pictured(), &found), None);
}

#[test]
fn a_missing_or_misplaced_step_is_asked_for_once() {
    let required = pictured();
    let found = check::steps(&required, &["Plan", "secrets-scan", "Build", "Sign off"]);
    assert_eq!(found.missing, ["Benchmark"]);
    assert_eq!(found.misplaced, ["Secrets scan"]);
    let ask = check::ask_to_place(&required, &found).expect("asks");
    assert!(
        ask.contains("Benchmark, before sign-off, by the workers"),
        "{ask}"
    );
    assert!(ask.contains("(missing)"), "{ask}");
    assert!(ask.contains("(in the wrong place)"), "{ask}");
    assert!(ask.contains("Argonauts adds it itself"), "{ask}");
}

#[test]
fn first_and_last_hold_among_steps_that_share_the_place() {
    let required = Required {
        steps: parse(
            "[[step]]\nname = \"A\"\nwhen = \"first\"\n[[step]]\nname = \"B\"\nwhen = \"first\"\n\
             [[step]]\nname = \"Z\"\nwhen = \"last\"\n",
        )
        .expect("valid"),
        tasks: Vec::new(),
    };
    assert!(check::steps(&required, &["B", "A", "Build", "Z"]).is_empty());
    let found = check::steps(&required, &["A", "Build", "B", "Z"]);
    assert_eq!(found.misplaced, ["B"]);
}

#[test]
fn a_plan_that_keeps_your_tasks_passes_by_name_or_label() {
    let required = pictured();
    let by_label = check::tasks(
        &required,
        &["build:migrate-schema", "backfill-rows", "switch-reads"],
    )
    .expect("a fixed list");
    assert!(by_label.is_empty(), "{by_label:?}");
    let by_name = check::tasks(
        &required,
        &[
            "Migrate the schema",
            "Backfill the old rows",
            "Switch reads to the new table",
        ],
    )
    .expect("a fixed list");
    assert!(by_name.is_empty(), "{by_name:?}");
    assert_eq!(check::tasks(&Required::default(), &["x"]), None);
}

#[test]
fn a_merged_split_or_reordered_list_is_named() {
    let required = pictured();
    let merged = check::tasks(&required, &["migrate-and-backfill", "switch-reads"]).expect("list");
    assert_eq!(
        merged.missing,
        ["migrate the schema", "backfill the old rows"]
    );
    assert_eq!(merged.added, ["migrate-and-backfill"]);
    let reordered = check::tasks(
        &required,
        &["backfill-rows", "migrate-schema", "switch-reads"],
    )
    .expect("list");
    assert!(reordered.reordered && reordered.missing.is_empty() && reordered.added.is_empty());
    let ask = check::ask_to_keep_tasks(&required, &merged).expect("asks");
    assert!(ask.contains("1. migrate the schema"), "{ask}");
    assert!(ask.contains("\"migrate-and-backfill\""), "{ask}");
    assert!(ask.contains("ask the user; don't add it"), "{ask}");
    let said = check::said(&check::Steps::default(), Some(&reordered));
    assert_eq!(said, ["Your tasks weren't kept in your order."]);
}

#[test]
fn whoever_writes_the_workflow_is_told_both() {
    let required = pictured();
    let writer = brief::for_writer(&required).expect("lines");
    assert!(
        writer.contains("- Secrets scan, after build, by the reviewers: Scan"),
        "{writer}"
    );
    assert!(writer.contains("2. backfill the old rows"), "{writer}");
    assert!(writer.contains("ask the user; don't add it"), "{writer}");
    let claude = brief::for_claude_code(&required).expect("lines");
    assert!(claude.contains("phase for each of these steps"), "{claude}");
    assert!(
        claude.contains("3. switch reads to the new table (label: switch-reads)"),
        "{claude}"
    );
    assert_eq!(brief::for_writer(&Required::default()), None);
    assert_eq!(brief::for_claude_code(&Required::default()), None);
}
