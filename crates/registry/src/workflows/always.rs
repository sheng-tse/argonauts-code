//! Writing steps into `workflows/always.toml`, for a setup you saved. A step of the same name
//! is changed where it stands, and everything else in the file, comments too, stays as it was.

use std::io::ErrorKind;
use std::path::Path;

use argonauts_core::workflows::required::{self, RequiredStep, same_name};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

/// The first lines of a file the setup starts.
const HEADER: &str = "# Steps every workflow must contain, whoever writes it: you, the \
orchestrator or Claude Code.\n# The guide's workflows page says how they're placed and checked.\n\n";

/// Adds `steps` to the file at `path`, or puts each in place of the step of its name. A file
/// that isn't valid is left alone, and the problem says why.
pub fn save_required_steps(path: &Path, steps: &[RequiredStep]) -> Result<(), String> {
    let shown = path.display();
    let before = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => HEADER.to_owned(),
        Err(err) => return Err(format!("{shown} can't be read: {err}")),
    };
    let listed = required::parse(&before).map_err(|problems| {
        format!(
            "{shown} isn't valid, so nothing was added to it: {}",
            problems.join("; ")
        )
    })?;
    // A file of comments alone keeps them above the steps: as a document, they'd trail them.
    let after = if listed.is_empty() {
        format!("{}\n\n{}", before.trim_end(), required::steps_toml(steps))
    } else {
        edited(&before, steps).map_err(|problem| format!("{shown}: {problem}"))?
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("{shown}: {err}"))?;
    }
    std::fs::write(path, after.trim_start()).map_err(|err| format!("{shown}: {err}"))
}

/// `before` with each of `steps` in place of the step of its name, or after the last.
fn edited(before: &str, steps: &[RequiredStep]) -> Result<String, String> {
    let mut doc: DocumentMut = before
        .parse()
        .map_err(|err| format!("it doesn't read: {err}"))?;
    if !doc.contains_key("step") {
        doc.insert("step", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    let Some(listed) = doc["step"].as_array_of_tables_mut() else {
        return Err("`step` must be a list of [[step]] tables".to_owned());
    };
    for step in steps {
        let known = listed.iter_mut().find(|table| {
            table
                .get("name")
                .and_then(Item::as_str)
                .is_some_and(|name| same_name(name, &step.name))
        });
        match known {
            Some(table) => fill(table, step),
            None => {
                let mut table = Table::new();
                fill(&mut table, step);
                listed.push(table);
            }
        }
    }
    Ok(doc.to_string())
}

fn fill(table: &mut Table, step: &RequiredStep) {
    table["name"] = value(step.name.as_str());
    table["role"] = value(step.role.as_str());
    match &step.does {
        Some(does) => table["does"] = value(does.as_str()),
        None => {
            table.remove("does");
        }
    }
    match &step.when {
        Some(when) => table["when"] = value(when.as_str()),
        None => {
            table.remove("when");
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::crew::CrewRole;

    use super::*;

    fn step(name: &str, when: Option<&str>) -> RequiredStep {
        RequiredStep {
            name: name.to_owned(),
            role: CrewRole::Worker,
            does: Some(format!("Run {name}.")),
            when: when.map(str::to_owned),
        }
    }

    #[test]
    fn a_new_file_gets_its_steps_under_a_header() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("workflows").join(required::FILE);
        save_required_steps(&path, &[step("Benchmark", Some("last"))]).expect("saved");
        let text = std::fs::read_to_string(&path).expect("written");
        assert!(text.starts_with("# Steps every workflow"), "{text}");
        assert_eq!(
            required::parse(&text).expect("valid"),
            [step("Benchmark", Some("last"))]
        );
    }

    #[test]
    fn a_step_of_the_same_name_changes_where_it_stands_and_comments_stay() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join(required::FILE);
        std::fs::write(
            &path,
            "# mine\n[[step]]\nname = \"benchmark\"\nwhen = \"first\"\n\n[[step]]\nname = \"Lint\"\n",
        )
        .expect("file");
        save_required_steps(&path, &[step("Benchmark", None), step("Scan", None)]).expect("saved");
        let text = std::fs::read_to_string(&path).expect("written");
        assert!(text.starts_with("# mine"), "{text}");
        let names: Vec<String> = required::parse(&text)
            .expect("valid")
            .into_iter()
            .map(|step| step.name)
            .collect();
        assert_eq!(names, ["Benchmark", "Lint", "Scan"]);
        assert!(!text.contains("first"), "{text}");
    }

    #[test]
    fn a_file_that_isnt_valid_is_left_alone() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join(required::FILE);
        std::fs::write(&path, "[[step]]\nname = \"x\"\nrole = \"nobody\"\n").expect("file");
        let err = save_required_steps(&path, &[step("Benchmark", None)]).expect_err("refused");
        assert!(err.contains("nobody"), "{err}");
        assert!(
            std::fs::read_to_string(&path)
                .expect("kept")
                .contains("nobody")
        );
    }
}
