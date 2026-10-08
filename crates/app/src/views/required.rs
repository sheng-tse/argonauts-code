//! The steps you require of every workflow, by name, so the workflow view can mark them as
//! yours. They live in `workflows/always.toml`, in the project's `.argonauts/` and in
//! `~/.argonauts/`, as `[[step]]` entries.
//!
//! Only the names are read here. Whether a workflow really contains the steps is checked where
//! the workflow is written, before it runs; the view and the chat mark what they are shown.

use std::path::{Path, PathBuf};

use argonauts_core::events::workflow::{WorkflowDeclared, WorkflowEvent};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::layout::{project_files, user_files};
use argonauts_core::required::RequiredStep as KeptStep;
use argonauts_core::views::WorkflowView;
use serde::Deserialize;

use crate::env::AppEnv;

const ALWAYS: &str = "always.toml";

#[derive(Debug, Default, Deserialize)]
struct Always {
    #[serde(default)]
    step: Vec<RequiredStep>,
}

#[derive(Debug, Deserialize)]
struct RequiredStep {
    name: String,
}

/// Marks the phases of `view` that are steps you require: the project's, then your own, then
/// those a setup `kept` for the conversation or the run. A step required only by a request's
/// words is added by whoever reads the request.
pub(crate) fn mark(view: &mut WorkflowView, env: &AppEnv, kept: &[KeptStep]) {
    for name in with_kept(env, kept) {
        remember(&mut view.required, name);
    }
}

/// The names `always.toml` lists, then those of the steps a setup kept, each once.
fn with_kept(env: &AppEnv, kept: &[KeptStep]) -> Vec<String> {
    let mut names = required_steps(&always_files(env));
    for step in kept {
        remember(&mut names, step.name.clone());
    }
    names
}

/// `event` as the screen is sent it. A workflow's declaration goes with the steps you require
/// named, so the chat marks them where it lists the phases, as the view does: its own phases,
/// and those of a workflow one of its steps runs, which only that workflow's report names.
/// The journal keeps the declaration as the workflow made it.
pub(crate) fn for_screen(event: &RunEvent, env: &AppEnv) -> RunEvent {
    let mut event = event.clone();
    if let RunEventKind::Workflow(WorkflowEvent::Declared(declared)) = &mut event.kind {
        mark_declared(declared, required_steps(&always_files(env)));
    }
    event
}

/// Adds the steps in `names` to those `declared` already names.
fn mark_declared(declared: &mut WorkflowDeclared, names: Vec<String>) {
    for name in names {
        remember(&mut declared.required, name);
    }
}

fn always_files(env: &AppEnv) -> [PathBuf; 2] {
    [
        env.project_config()
            .with_file_name(project_files::WORKFLOWS)
            .join(ALWAYS),
        env.paths.home().join(user_files::WORKFLOWS).join(ALWAYS),
    ]
}

/// Every step name in `files`, in order, each once. A file that is missing or doesn't parse
/// adds nothing: the check that runs before a workflow starts is what reports it.
fn required_steps(files: &[PathBuf]) -> Vec<String> {
    let mut names = Vec::new();
    for file in files {
        for name in names_in(file) {
            remember(&mut names, name);
        }
    }
    names
}

fn names_in(file: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    toml::from_str::<Always>(&text)
        .map(|always| always.step.into_iter().map(|step| step.name).collect())
        .unwrap_or_default()
}

fn remember(names: &mut Vec<String>, name: String) {
    let name = name.trim().to_owned();
    let known = names.iter().any(|known| known.eq_ignore_ascii_case(&name));
    if !name.is_empty() && !known {
        names.push(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_projects_steps_come_first_and_a_name_counts_once() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.toml");
        let yours = dir.path().join("yours.toml");
        std::fs::write(
            &project,
            r#"
            [[step]]
            name = "Secrets scan"
            role = "reviewers"
            does = "Scan the change for keys, tokens and passwords."
            when = "after build"
            "#,
        )
        .unwrap();
        std::fs::write(
            &yours,
            r#"
            [[step]]
            name = "Benchmark"
            role = "workers"
            when = "before sign-off"

            [[step]]
            name = "secrets scan"
            "#,
        )
        .unwrap();
        assert_eq!(
            required_steps(&[project, yours]),
            ["Secrets scan", "Benchmark"]
        );
    }

    /// A declaration keeps what the workflow itself said it requires, and names every step of
    /// yours after it, each once: a phase of a workflow one of its steps runs may be one.
    #[test]
    fn a_declaration_names_the_required_steps_after_its_own() {
        use argonauts_core::names::WorkflowId;
        use argonauts_core::workflow::StandardPhase;

        let phase = |index: u32, title: &str| argonauts_core::events::workflow::DeclaredPhase {
            index,
            title: title.to_owned(),
            detail: String::new(),
            standard: StandardPhase::Do,
        };
        let mut declared = WorkflowDeclared {
            id: WorkflowId::numbered(1),
            name: "switch-reads".to_owned(),
            description: String::new(),
            phases: vec![
                phase(0, "Plan"),
                phase(1, "secrets scan"),
                phase(2, "Sign off"),
            ],
            required: vec!["Sign off".to_owned()],
        };
        mark_declared(
            &mut declared,
            vec![
                "Secrets scan".to_owned(),
                "sign off".to_owned(),
                "Benchmark".to_owned(),
            ],
        );
        assert_eq!(declared.required, ["Sign off", "Secrets scan", "Benchmark"]);
        let held: Vec<bool> = declared
            .phases
            .iter()
            .map(|phase| declared.requires(phase))
            .collect();
        assert_eq!(held, [false, true, true]);
    }

    /// What the screen is sent names the project's steps on a declaration that holds them. The
    /// event the journal keeps, and every other event, go unchanged.
    #[test]
    fn the_screen_is_sent_a_declaration_with_your_steps_named() {
        use argonauts_core::RunId;
        use argonauts_core::events::workflow::{DeclaredPhase, RunNote};
        use argonauts_core::names::WorkflowId;
        use argonauts_core::paths::Paths;
        use argonauts_core::workflow::StandardPhase;

        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let env = AppEnv::for_repo(Paths::new(home.path()), repo.path(), false).unwrap();
        let workflows = repo.path().join(".argonauts").join("workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(
            workflows.join("always.toml"),
            "[[step]]\nname = \"Benchmark\"\nrole = \"workers\"\n",
        )
        .unwrap();

        let event = |kind| RunEvent {
            seq: 0,
            ts: time::OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new("run-01").unwrap(),
            card_id: None,
            session: None,
            source: "conductor".to_owned(),
            kind: RunEventKind::Workflow(kind),
        };
        let declared = event(WorkflowEvent::Declared(Box::new(WorkflowDeclared {
            id: WorkflowId::numbered(1),
            name: "switch-reads".to_owned(),
            description: String::new(),
            phases: vec![DeclaredPhase {
                index: 0,
                title: "Benchmark".to_owned(),
                detail: String::new(),
                standard: StandardPhase::Do,
            }],
            required: Vec::new(),
        })));
        let RunEventKind::Workflow(WorkflowEvent::Declared(shown)) =
            for_screen(&declared, &env).kind
        else {
            panic!("a declaration stays one");
        };
        assert_eq!(shown.required, ["Benchmark"]);

        let note = event(WorkflowEvent::Note(RunNote {
            text: "a second round".to_owned(),
            workflow: None,
            left: false,
        }));
        assert_eq!(for_screen(&note, &env), note);
    }

    /// A step a setup kept for the conversation or the run is marked as one in a file is.
    #[test]
    fn the_steps_a_setup_kept_are_marked_after_the_files() {
        use argonauts_core::paths::Paths;
        use argonauts_core::settings::crew::CrewRole;

        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let env = AppEnv::for_repo(Paths::new(home.path()), repo.path(), false).unwrap();
        let workflows = repo.path().join(".argonauts").join("workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(
            workflows.join("always.toml"),
            "[[step]]\nname = \"Benchmark\"\nrole = \"workers\"\n",
        )
        .unwrap();
        let kept = |name: &str| KeptStep {
            name: name.to_owned(),
            role: CrewRole::Reviewers,
            does: None,
            when: None,
        };
        assert_eq!(
            with_kept(&env, &[kept("security review"), kept("benchmark")]),
            ["Benchmark", "security review"]
        );
        assert_eq!(with_kept(&env, &[]), ["Benchmark"]);
    }

    #[test]
    fn a_missing_or_broken_file_requires_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let broken = dir.path().join("always.toml");
        std::fs::write(&broken, "[[step]\nname = ").unwrap();
        assert!(required_steps(&[broken, dir.path().join("none.toml")]).is_empty());
    }
}
