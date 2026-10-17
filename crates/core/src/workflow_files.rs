//! A workflow is one file. It says what the workflow is for, then holds its steps
//! (`[[step]]`), the settings it changes, and its phases (`[[phases]]`). Any part may be left
//! out: a file of steps alone, of settings alone, or of phases alone is a workflow too. The
//! built-in ones are files like yours, compiled into the binary.

use serde::Deserialize;

use crate::workflow::{Phase, Workflow, WorkflowParams};

macro_rules! built_in {
    ($($name:literal),* $(,)?) => {
        &[$(($name, $crate::template!(concat!("workflows/", $name, ".toml")))),*]
    };
}

/// `(name, the file's text)` for every built-in workflow, in the order the picker shows them:
/// council first, whose settings are the built-in ones.
pub const BUILT_IN: &[(&str, &str)] = built_in![
    "council",
    "orchestrator",
    "captain",
    "maker-checker",
    "best-of-n",
    "draft-and-vote",
    "ablate-no-blind",
    "ablate-no-anonymize",
    "ablate-no-critique",
    "ablate-fixed-roles",
    "ablate-same-provider",
    "ablate-no-experiments",
];

/// What `workflows/README.md` says: every key of a step, and what a script may use.
pub const README: &str = crate::template!("workflows/README.md");

/// The table a file's steps are written in, one per step.
pub const STEP_TABLE: &str = "[[step]]";
/// The table a file's phases are written in, one per phase.
pub const PHASES_TABLE: &str = "[[phases]]";
/// The table that holds the values a file's phases fill in.
pub const PARAMS_TABLE: &str = "[params]";

pub fn built_in(name: &str) -> Option<&'static str> {
    BUILT_IN
        .iter()
        .find(|(built_in, _)| *built_in == name)
        .map(|(_, text)| *text)
}

/// The built-in workflows that have steps. Captain has none: it is the CLI itself.
pub fn with_steps() -> impl Iterator<Item = (&'static str, &'static str)> {
    BUILT_IN
        .iter()
        .copied()
        .filter(|(_, text)| has_table(text, STEP_TABLE))
}

/// The built-in workflows that have phases of their own. The others only change settings and
/// run the default workflow's.
pub fn with_phases() -> impl Iterator<Item = (&'static str, &'static str)> {
    BUILT_IN
        .iter()
        .copied()
        .filter(|(_, text)| has_table(text, PHASES_TABLE))
}

/// Whether a line of the file opens this table.
pub fn has_table(text: &str, table: &str) -> bool {
    text.lines()
        .any(|line| line.trim_start().starts_with(table))
}

/// The parts of a workflow file its phases are made of. Its steps and the settings it carries
/// are read elsewhere, so every other key passes.
#[derive(Deserialize)]
struct Written {
    name: Option<String>,
    about: Option<String>,
    params: Option<WorkflowParams>,
    phases: Option<Vec<Phase>>,
}

/// A workflow file's phases as a workflow, named after the file when it doesn't name itself.
/// `Ok(None)` is a file without phases. A key no phase or step has is refused, with its line.
pub fn phases(text: &str, file_name: &str) -> Result<Option<Workflow>, String> {
    let written: Written = toml::from_str(text).map_err(|err| err.to_string())?;
    Ok(written.phases.map(|phases| Workflow {
        name: written.name.unwrap_or_else(|| file_name.to_owned()),
        description: written.about.unwrap_or_default(),
        based_on: None,
        params: written.params.unwrap_or_default(),
        phases,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::Count;

    fn names(found: impl Iterator<Item = (&'static str, &'static str)>) -> Vec<&'static str> {
        found.map(|(name, _)| name).collect()
    }

    #[test]
    fn every_built_in_names_itself_and_says_what_it_is_for() {
        for (name, text) in BUILT_IN {
            let file: toml::Table = text.parse().unwrap_or_else(|err| panic!("{name}: {err}"));
            assert_eq!(file["name"].as_str(), Some(*name));
            assert!(!file["about"].as_str().unwrap().is_empty(), "{name}");
            assert_eq!(built_in(name), Some(*text));
        }
        assert_eq!(built_in("no-such-workflow"), None);
    }

    #[test]
    fn the_built_ins_with_steps_and_with_phases() {
        assert_eq!(
            names(with_steps()),
            ["council", "orchestrator", "maker-checker", "best-of-n"]
        );
        assert_eq!(
            names(with_phases()),
            [
                "council",
                "orchestrator",
                "captain",
                "maker-checker",
                "best-of-n",
                "draft-and-vote"
            ]
        );
    }

    #[test]
    fn a_built_ins_phases_come_with_its_name_what_it_is_for_and_its_params() {
        for (name, text) in with_phases() {
            let workflow = phases(text, "the-file")
                .unwrap_or_else(|err| panic!("{name}: {err}"))
                .unwrap();
            assert_eq!(workflow.name, name);
            assert!(!workflow.description.is_empty(), "{name}");
            assert!(!workflow.phases.is_empty(), "{name}");
        }
        let best_of_n = phases(built_in("best-of-n").unwrap(), "the-file")
            .unwrap()
            .unwrap();
        assert_eq!(best_of_n.params.0["tries"], 3);
        assert_eq!(
            best_of_n.phases[1].steps[0].count,
            Some(Count::Param("{{tries}}".to_owned()))
        );
    }

    #[test]
    fn a_file_without_phases_has_none_and_one_that_names_no_workflow_takes_the_files_name() {
        let settings_only = built_in("ablate-no-critique").unwrap();
        assert_eq!(phases(settings_only, "the-file"), Ok(None));

        let unnamed = "[[phases]]\nname = \"Build\"\nstandard = \"do\"\n\n  [[phases.steps]]\n  \
                       kind = \"build\"\n  role = \"worker\"\n";
        let workflow = phases(unnamed, "the-file").unwrap().unwrap();
        assert_eq!(workflow.name, "the-file");
        assert_eq!(workflow.phases[0].steps.len(), 1);
    }

    #[test]
    fn a_key_no_step_has_is_refused_with_its_line() {
        let misspelled = "[[phases]]\nname = \"Build\"\nstandard = \"do\"\n\n[[phases.steps]]\n\
                          kind = \"build\"\nblindly = true\n";
        let err = phases(misspelled, "the-file").unwrap_err();
        assert!(err.contains("blindly"), "{err}");
        assert!(err.contains("line 7"), "{err}");
    }
}
