//! The built-in workflows' phases, compiled into the binary. A workflow is one file, and its
//! phases are the part of it the crew runs.

use argonauts_core::workflow::Workflow;
use argonauts_core::workflow_files;

use crate::workflow::yaml;

/// The built-in workflows that have phases of their own, in the order the picker shows them.
pub fn names() -> impl Iterator<Item = &'static str> {
    workflow_files::with_phases().map(|(name, _)| name)
}

/// The file of a built-in workflow that has phases.
pub fn source(name: &str) -> Option<&'static str> {
    workflow_files::with_phases()
        .find(|(builtin, _)| *builtin == name)
        .map(|(_, text)| text)
}

/// The parsed built-in, or why it didn't parse.
pub fn load(name: &str) -> Result<Workflow, String> {
    let no_such = || format!("no built-in workflow is named `{name}`");
    let text = source(name).ok_or_else(no_such)?;
    let workflow = workflow_files::phases(text, name)
        .map_err(|err| format!("built-in workflow `{name}`: {err}"))?
        .ok_or_else(no_such)?;
    if workflow.name != name {
        return Err(format!(
            "built-in workflow `{name}` calls itself `{}`",
            workflow.name
        ));
    }
    Ok(workflow)
}

/// The built-in's phases as the YAML the main agent reads and writes workflows in.
pub fn example(name: &str) -> Result<String, String> {
    yaml::print(&load(name)?)
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::Settings;

    use super::*;
    use crate::workflow::runner::allowed_workers;
    use crate::workflow::validate::{Context, validate};

    #[test]
    fn every_builtin_parses_round_trips_and_validates() {
        let settings = Settings::default();
        let allowed = allowed_workers(&settings);
        let providers = allowed
            .iter()
            .map(|worker| (worker.clone(), worker.to_string()))
            .collect();
        let context = Context {
            settings: &settings,
            allowed: &allowed,
            providers: &providers,
            lookup: &load,
            depth: 0,
            limit_left_usd: None,
            as_you_asked: &[],
        };
        for name in names() {
            let workflow = load(name).unwrap_or_else(|err| panic!("{err}"));
            assert!(!workflow.description.is_empty(), "{name}");
            let text = example(name).unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(yaml::parse(&text).as_ref(), Ok(&workflow), "{name}");
            if let Err(errors) = validate(&workflow, &context) {
                panic!("{name}: {errors:?}");
            }
        }
        assert!(load("no-such-workflow").is_err());
    }
}
