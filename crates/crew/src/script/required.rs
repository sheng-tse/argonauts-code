//! What a run must contain: the steps `always.toml` requires and the ones a setup kept for the
//! run, and a task list the request fixed. The reading is the one place that knows those files
//! and words (`argonauts_core::required`); the runner places the steps and keeps to the tasks.

use std::path::PathBuf;

use argonauts_core::events::RunEventKind;
use argonauts_core::log::{Note, NoteLevel};
use argonauts_core::required::RequiredStep as KeptStep;
use argonauts_runner::{Library, Required, RequiredStep};

use crate::engine::host::Host;

/// What this run must contain. A file that couldn't be read is left out whole, and the run's
/// notes say why.
pub fn for_run(host: &Host, request: &str) -> Required {
    let (required, problems) = read(&super::library(host), &kept(host), request);
    for problem in problems {
        let note = Note {
            level: NoteLevel::Warn,
            text: problem,
        };
        let _ = host.record(None, None, RunEventKind::Note(note));
    }
    required
}

/// What a request must contain with the workflows in these folders: the steps their
/// `always.toml` requires, the repository's winning over yours, then the steps a setup `kept`,
/// then what the request's own words require, and a task list they fix.
pub fn for_request(library: &Library, kept: &[KeptStep], request: &str) -> Required {
    read(library, kept, request).0
}

/// The names of the steps this run must contain, which its workflow's declaration carries so
/// the screen marks them where the phases that run show.
pub fn names(host: &Host) -> Vec<String> {
    let request = host.read(crate::goal::round::request).unwrap_or_default();
    read(&super::library(host), &kept(host), &request)
        .0
        .steps
        .into_iter()
        .map(|step| step.name)
        .collect()
}

/// The same list as `argonauts_core::required` holds it, which briefs whoever writes the
/// workflow and checks what was written.
pub fn shared(required: &Required) -> argonauts_core::required::Required {
    let steps = required
        .steps
        .iter()
        .map(|step| KeptStep {
            name: step.name.clone(),
            role: step.role,
            does: step.does.clone(),
            when: step.when.clone(),
        })
        .collect();
    argonauts_core::required::Required {
        steps,
        tasks: required.tasks.clone(),
    }
}

/// The steps a setup kept for the conversation or the request that started this run.
fn kept(host: &Host) -> Vec<KeptStep> {
    host.read(|state| {
        state
            .run
            .started
            .as_ref()
            .map(|started| started.required_steps.clone())
            .unwrap_or_default()
    })
}

/// The kept steps go in place of a file's step of the same name, and the request's own words
/// go in place of both.
fn read(library: &Library, kept: &[KeptStep], request: &str) -> (Required, Vec<String>) {
    // A project's `always.toml` that waits for your trust requires nothing yet.
    let folders: Vec<&PathBuf> = [&library.yours, &library.project]
        .into_iter()
        .flatten()
        .filter(|folder| {
            !library
                .waiting
                .contains(&folder.join(argonauts_core::required::FILE))
        })
        .collect();
    let folders: Vec<&std::path::Path> = folders.iter().map(|folder| folder.as_path()).collect();
    let read = argonauts_core::required::read_with(&folders, kept, request);
    let steps = read
        .required
        .steps
        .into_iter()
        .map(|step| RequiredStep {
            name: step.name,
            role: step.role,
            does: step.does,
            when: step.when,
        })
        .collect();
    let required = Required {
        steps,
        tasks: read.required.tasks,
    };
    (required, read.problems)
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::crew::CrewRole;

    use super::*;

    #[test]
    fn always_toml_and_the_requests_words_are_what_a_run_must_contain() {
        let dir = tempfile::tempdir().unwrap();
        let yours = dir.path().join("yours");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&yours).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(argonauts_core::required::FILE),
            "[[step]]\nname = \"benchmark\"\nrole = \"workers\"\ndoes = \"Run the benchmark.\"\n",
        )
        .unwrap();
        let mut library = Library {
            project: Some(project.clone()),
            yours: Some(yours),
            waiting: Vec::new(),
        };
        let required = for_request(&library, &[], "add due dates");
        assert_eq!(required.steps.len(), 1, "{required:?}");
        assert_eq!(required.steps[0].name, "benchmark");
        assert_eq!(required.steps[0].role, CrewRole::Workers);
        assert!(required.tasks.is_empty());

        // One that came with the repository requires nothing until you trust it.
        library.waiting = vec![project.join(argonauts_core::required::FILE)];
        assert!(for_request(&library, &[], "add due dates").is_empty());

        let none = Library::default();
        assert!(for_request(&none, &[], "add due dates").is_empty());
    }

    #[test]
    fn a_step_a_setup_kept_is_required_in_place_of_the_files_step_of_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(argonauts_core::required::FILE),
            "[[step]]\nname = \"benchmark\"\nrole = \"workers\"\ndoes = \"Run the benchmark.\"\n",
        )
        .unwrap();
        let library = Library {
            project: Some(project),
            yours: None,
            waiting: Vec::new(),
        };
        let kept = [
            KeptStep {
                name: "Benchmark".into(),
                role: CrewRole::Reviewers,
                does: Some("Time the hot path.".into()),
                when: Some("last".into()),
            },
            KeptStep {
                name: "security review".into(),
                role: CrewRole::Reviewers,
                does: None,
                when: None,
            },
        ];
        let (required, problems) = read(&library, &kept, "add due dates");
        assert!(problems.is_empty(), "{problems:?}");
        let names: Vec<&str> = required
            .steps
            .iter()
            .map(|step| step.name.as_str())
            .collect();
        assert_eq!(names, ["Benchmark", "security review"]);
        assert_eq!(required.steps[0].role, CrewRole::Reviewers);
        assert_eq!(
            required.steps[0].does.as_deref(),
            Some("Time the hot path.")
        );
        // `/workflow try` counts them the same way.
        assert_eq!(for_request(&library, &kept, "add due dates"), required);
    }
}
