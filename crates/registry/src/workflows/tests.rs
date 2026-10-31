use std::path::Path;

use argonauts_core::workflows::workflow::{Count, StepModels, WorkflowRef};
use serde_json::json;
use tempfile::TempDir;

use super::*;

/// A valid one-phase workflow. `steps` is the YAML list under the phase, indented six spaces.
fn workflow_yaml(name: &str, description: &str, steps: &str) -> String {
    format!(
        "name: {name}\ndescription: \"{description}\"\nphases:\n  - name: Do\n    standard: do\n    steps:\n{steps}"
    )
}

fn simple(name: &str, description: &str) -> String {
    workflow_yaml(name, description, "      - kind: build\n")
}

/// Runs the workflow `inner` as its only step.
fn nesting(name: &str, inner: &str) -> String {
    workflow_yaml(
        name,
        "nests",
        &format!("      - kind: workflow\n        workflow: {inner}\n"),
    )
}

struct Folders {
    _root: TempDir,
    global: std::path::PathBuf,
    project: std::path::PathBuf,
}

impl Folders {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let global = root.path().join("home/.argonauts/workflows");
        let project = root.path().join("repo/.argonauts/workflows");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        Self {
            _root: root,
            global,
            project,
        }
    }

    fn dirs<'a>(&'a self, builtins: &'a [(&'a str, &'a str)]) -> WorkflowDirs<'a> {
        WorkflowDirs {
            global: &self.global,
            project: Some(&self.project),
            builtins,
        }
    }
}

fn write(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
}

fn find(name: &str, dirs: &WorkflowDirs<'_>) -> Result<FoundWorkflow, WorkflowError> {
    find_workflow(&WorkflowRef::new(name), dirs)
}

fn problems_of(result: Result<FoundWorkflow, WorkflowError>) -> Vec<String> {
    match result {
        Err(WorkflowError::Invalid { problems, .. }) => problems,
        other => panic!("expected problems, got {other:?}"),
    }
}

#[test]
fn workflow_lookup_project_then_global_then_builtin() {
    let folders = Folders::new();
    let builtin = simple("review-panel", "built-in panel");
    let builtins = [("review-panel", builtin.as_str())];
    let dirs = folders.dirs(&builtins);
    write(
        &folders.global,
        "review-panel.yaml",
        &simple("review-panel", "your panel"),
    );
    write(
        &folders.project,
        "review-panel.yaml",
        &simple("review-panel", "the team's panel"),
    );

    let found = find("review-panel", &dirs).unwrap();
    assert_eq!(found.workflow.description, "the team's panel");
    assert_eq!(
        found.source,
        folders
            .project
            .join("review-panel.yaml")
            .display()
            .to_string()
    );
    assert_eq!(found.text, simple("review-panel", "the team's panel"));
    assert_eq!(found.version, workflow_version(&found.workflow));
    assert_eq!(found.version.len(), 64);

    std::fs::remove_file(folders.project.join("review-panel.yaml")).unwrap();
    let found = find("review-panel", &dirs).unwrap();
    assert_eq!(found.workflow.description, "your panel");
    assert_eq!(
        found.source,
        folders
            .global
            .join("review-panel.yaml")
            .display()
            .to_string()
    );

    std::fs::remove_file(folders.global.join("review-panel.yaml")).unwrap();
    let found = find("review-panel", &dirs).unwrap();
    assert_eq!(found.workflow.description, "built-in panel");
    assert_eq!(found.source, "built-in");
    assert_eq!(found.text, builtin);

    // Without a repo, only the global folder and the built-ins count.
    let no_repo = WorkflowDirs {
        project: None,
        ..dirs
    };
    write(
        &folders.project,
        "review-panel.yaml",
        &simple("review-panel", "the team's panel"),
    );
    assert_eq!(find("review-panel", &no_repo).unwrap().source, "built-in");

    write(&folders.global, "mine.yaml", &simple("mine", "mine"));
    match find("missing", &dirs) {
        Err(WorkflowError::NotFound { name, known }) => {
            assert_eq!(name, "missing");
            assert_eq!(known, "mine, review-panel");
        }
        other => panic!("expected not found, got {other:?}"),
    }

    // A name is a file stem, so it can never reach outside the folders.
    assert!(matches!(
        find("../repo/.argonauts/workflows/review-panel", &dirs),
        Err(WorkflowError::Refused(_))
    ));
}

#[test]
fn workflow_yaml_reads_counts_models_and_params() {
    let text = "\
name: review-panel
description: Two reviewers from other providers
params: { target: diff, reviewers: 2, focus: [tests, docs], limits: { rounds: 1.5 } }
phases:
  - name: Review
    standard: check
    steps:
      - kind: fan_out
        role: reviewer
        count: \"{{reviewers}}\"
        models: \"{{picked}}\"
        blind: true
        different_provider_from: author
        task: Review the {{target}}
      - kind: draft
        count: 3
        models: [codex, claude-code/opus]
";
    let workflow = parse_workflow(text, "review-panel.yaml").unwrap();
    let steps = &workflow.phases[0].steps;
    assert_eq!(steps[0].count, Some(Count::Param("{{reviewers}}".into())));
    assert_eq!(steps[0].models, StepModels::Param("{{picked}}".into()));
    assert_eq!(steps[1].count, Some(Count::Fixed(3)));
    let StepModels::Named(models) = &steps[1].models else {
        panic!("expected named models");
    };
    assert_eq!(models.len(), 2);
    assert_eq!(models[1].model.as_deref(), Some("opus"));
    assert_eq!(
        serde_json::to_value(&workflow.params).unwrap(),
        json!({ "target": "diff", "reviewers": 2, "focus": ["tests", "docs"], "limits": { "rounds": 1.5 } })
    );

    // The version follows the content, not the formatting.
    let reformatted = parse_workflow(
        &text.replace("{ target: diff, ", "{ target: \"diff\", "),
        "x",
    )
    .unwrap();
    assert_eq!(workflow_version(&workflow), workflow_version(&reformatted));
    let mut changed = workflow.clone();
    changed.description.push('!');
    assert_ne!(workflow_version(&workflow), workflow_version(&changed));
}

#[test]
fn workflow_file_validation_errors() {
    let folders = Folders::new();
    let dirs = folders.dirs(&[]);

    // Bad YAML says where.
    write(
        &folders.project,
        "broken.yaml",
        "name: broken\nphases: [\n  - name",
    );
    match find("broken", &dirs) {
        Err(WorkflowError::Parse { file, message }) => {
            assert!(file.ends_with("broken.yaml"), "{file}");
            assert!(message.contains("line 3"), "{message}");
        }
        other => panic!("expected a parse error, got {other:?}"),
    }

    // A misspelled key fails to load instead of vanishing.
    write(
        &folders.project,
        "typo.yaml",
        &workflow_yaml("typo", "", "      - kind: fan_out\n        blnd: true\n"),
    );
    match find("typo", &dirs) {
        Err(WorkflowError::Parse { message, .. }) => {
            assert!(message.contains("unknown field `blnd`"), "{message}");
            assert!(message.contains("line 8"), "{message}");
        }
        other => panic!("expected a parse error, got {other:?}"),
    }

    write(&folders.project, "renamed.yaml", &simple("original", ""));
    let problems = problems_of(find("renamed", &dirs));
    assert_eq!(
        problems,
        ["the name \"original\" doesn't match the file name \"renamed\""]
    );

    write(
        &folders.project,
        "empty-phase.yaml",
        "name: empty-phase\nphases:\n  - name: Look\n    standard: understand\n    steps: []\n",
    );
    assert_eq!(
        problems_of(find("empty-phase", &dirs)),
        ["phase \"Look\" has no steps"]
    );

    write(
        &folders.project,
        "placeholders.yaml",
        "\
name: placeholders
params: { reviewers: 2 }
phases:
  - name: Review
    standard: check
    steps:
      - kind: fan_out
        count: \"{{reviewers}}\"
        models: \"{{picked}}\"
        task: Review {{target}} against {{target}}
      - kind: loop_until
        until: clean
        max: 2
        steps:
          - kind: draft
            count: many
",
    );
    assert_eq!(
        problems_of(find("placeholders", &dirs)),
        [
            "phase \"Review\", step 1 (fan_out): {{picked}} in models isn't declared in params",
            "phase \"Review\", step 1 (fan_out): {{target}} in task isn't declared in params",
            "phase \"Review\", step 2.1 (draft): count \"many\" must be a number or one {{name}} placeholder",
        ]
    );

    write(
        &folders.project,
        "outer.yaml",
        &workflow_yaml(
            "outer",
            "",
            "      - kind: workflow\n        workflow: nowhere\n      - kind: workflow\n        workflow: inner\n        params: { focus: \"{{area}}\", depth: 1 }\n      - kind: workflow\n",
        ),
    );
    write(&folders.project, "inner.yaml", &simple("inner", ""));
    assert_eq!(
        problems_of(find("outer", &dirs)),
        [
            "phase \"Do\", step 1 (workflow): can't find nowhere (outer →  nowhere)",
            "phase \"Do\", step 2 (workflow): {{area}} in params isn't declared in params",
            "phase \"Do\", step 2 (workflow): passes \"depth\", which inner doesn't declare in its params",
            "phase \"Do\", step 2 (workflow): passes \"focus\", which inner doesn't declare in its params",
            "phase \"Do\", step 3 (workflow): names no workflow to run",
        ]
    );

    // Depth: the run's own workflow is 1, and nesting stops at 2.
    write(&folders.global, "top.yaml", &nesting("top", "middle"));
    write(&folders.global, "middle.yaml", &nesting("middle", "bottom"));
    write(&folders.global, "bottom.yaml", &simple("bottom", ""));
    assert_eq!(
        problems_of(find("top", &dirs)),
        [
            "phase \"Do\", step 1 (workflow): nests too deep: top →  middle →  bottom is 3 deep, and the most is 2"
        ]
    );
    assert!(find("middle", &dirs).is_ok());
    let middle = find("middle", &dirs).unwrap().workflow;
    assert_eq!(
        check_workflow(&middle, Some("middle"), &dirs, 1),
        [
            "phase \"Do\", step 1 (workflow): nests too deep: middle →  bottom is 2 deep, and the most is 1"
        ]
    );
    // A setting above the ceiling is held to it.
    assert_eq!(
        check_workflow(&middle, Some("middle"), &dirs, 5),
        Vec::<String>::new()
    );

    write(&folders.global, "ping.yaml", &nesting("ping", "pong"));
    write(&folders.global, "pong.yaml", &nesting("pong", "ping"));
    assert_eq!(
        problems_of(find("ping", &dirs)),
        ["phase \"Do\", step 1 (workflow): loops back to ping: ping →  pong →  ping"]
    );
    write(&folders.global, "itself.yaml", &nesting("itself", "itself"));
    assert_eq!(
        problems_of(find("itself", &dirs)),
        ["phase \"Do\", step 1 (workflow): loops back to itself: itself →  itself"]
    );

    // A nested workflow that doesn't parse says why.
    write(
        &folders.global,
        "uses-broken.yaml",
        &nesting("uses-broken", "broken"),
    );
    let problems = problems_of(find("uses-broken", &dirs));
    assert_eq!(problems.len(), 1);
    assert!(
        problems[0].starts_with(
            "phase \"Do\", step 1 (workflow): broken doesn't load (uses-broken →  broken): "
        ),
        "{problems:?}"
    );

    // Starlark comes later, and a `.star` file isn't passed over for one further down.
    write(&folders.global, "custom.yaml", &simple("custom", ""));
    write(&folders.project, "custom.star", "def run(): pass\n");
    match find("custom", &dirs) {
        Err(WorkflowError::Refused(message)) => {
            assert!(
                message.ends_with("custom.star: Starlark workflows come later"),
                "{message}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    // Built-ins are checked like files.
    let bad_builtin = simple("other-name", "");
    let builtins = [("captain", bad_builtin.as_str())];
    match find("captain", &folders.dirs(&builtins)) {
        Err(WorkflowError::Invalid { file, problems }) => {
            assert_eq!(file, "built-in captain");
            assert_eq!(
                problems,
                ["the name \"other-name\" doesn't match the file name \"captain\""]
            );
        }
        other => panic!("expected problems, got {other:?}"),
    }
}

#[test]
fn workflows_listed_for_the_picker() {
    let folders = Folders::new();
    let captain = simple("captain", "One agent, checked and reviewed");
    let panel = simple("review-panel", "Two reviewers from other providers");
    let builtins = [
        ("captain", captain.as_str()),
        ("review-panel", panel.as_str()),
    ];
    let dirs = folders.dirs(&builtins);
    write(
        &folders.global,
        "review-panel.yaml",
        &simple("review-panel", "Your panel"),
    );
    write(
        &folders.global,
        "broken.yaml",
        "name: broken\nphases: [\n  - name",
    );
    write(&folders.global, "notes.txt", "not a workflow");
    write(
        &folders.project,
        "review-panel.yaml",
        &simple("review-panel", "The team's panel"),
    );
    write(
        &folders.project,
        "team-flow.yaml",
        &simple("team-flow", "Plan, build, check"),
    );
    write(&folders.project, "later.star", "def run(): pass\n");

    let rows = workflows_in(&dirs);
    let summary: Vec<(&str, &str)> = rows
        .iter()
        .map(|row| (row.name.as_str(), row.description.as_str()))
        .collect();
    assert_eq!(summary.len(), 5, "{summary:?}");
    assert_eq!(summary[0], ("captain", "One agent, checked and reviewed"));
    assert_eq!(summary[1], ("review-panel", "The team's panel"));
    assert_eq!(summary[2].0, "broken");
    assert!(summary[2].1.contains("line 3"), "{}", summary[2].1);
    assert_eq!(summary[3], ("later", "Starlark workflows come later"));
    assert_eq!(summary[4], ("team-flow", "Plan, build, check"));

    assert_eq!(rows[0].source, "built-in");
    assert_eq!(
        rows[1].source,
        folders
            .project
            .join("review-panel.yaml")
            .display()
            .to_string()
    );
    assert_eq!(
        rows[2].source,
        folders.global.join("broken.yaml").display().to_string()
    );

    // No folders at all: just the built-ins.
    let empty = WorkflowDirs {
        global: Path::new("/nonexistent/argonauts/workflows"),
        project: None,
        builtins: &builtins,
    };
    let names: Vec<String> = workflows_in(&empty)
        .into_iter()
        .map(|row| row.name)
        .collect();
    assert_eq!(names, ["captain", "review-panel"]);
}

#[test]
fn workflow_copy_never_replaces_a_file() {
    let folders = Folders::new();
    let panel = simple("review-panel", "Two reviewers from other providers");
    let builtins = [("review-panel", panel.as_str())];
    let dirs = WorkflowDirs {
        project: None,
        ..folders.dirs(&builtins)
    };
    let name = WorkflowRef::new("review-panel");

    // The project folder may not exist yet.
    std::fs::remove_dir_all(&folders.project).unwrap();
    let copied = copy_workflow(&name, &dirs, &folders.project).unwrap();
    assert_eq!(copied, folders.project.join("review-panel.yaml"));
    assert_eq!(std::fs::read_to_string(&copied).unwrap(), panel);

    std::fs::write(&copied, "# edited\n").unwrap();
    match copy_workflow(&name, &dirs, &folders.project) {
        Err(WorkflowError::Refused(message)) => {
            assert!(message.contains("already exists"), "{message}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(std::fs::read_to_string(&copied).unwrap(), "# edited\n");

    // Copying your own file copies its text as found, even one that doesn't load yet.
    write(&folders.global, "draft.yaml", "name: draft\nphases: [\n");
    let draft = copy_workflow(&WorkflowRef::new("draft"), &dirs, &folders.project).unwrap();
    assert_eq!(
        std::fs::read_to_string(draft).unwrap(),
        "name: draft\nphases: [\n"
    );

    assert!(matches!(
        copy_workflow(&WorkflowRef::new("missing"), &dirs, &folders.project),
        Err(WorkflowError::NotFound { .. })
    ));
}
