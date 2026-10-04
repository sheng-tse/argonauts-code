//! The built-in workflows, compiled into the binary. A preset is a workflow plus settings.

use argonauts_core::workflow::Workflow;

use crate::workflow::yaml;

/// `(name, YAML source)` for every built-in workflow.
pub const BUILTIN: &[(&str, &str)] = &[
    ("peer-crew", include_str!("../../workflows/peer-crew.yaml")),
    (
        "coordinated-crew",
        include_str!("../../workflows/coordinated-crew.yaml"),
    ),
    ("solo", include_str!("../../workflows/solo.yaml")),
    (
        "solo-reviewed",
        include_str!("../../workflows/solo-reviewed.yaml"),
    ),
    ("best-of-n", include_str!("../../workflows/best-of-n.yaml")),
    (
        "draft-and-vote",
        include_str!("../../workflows/draft-and-vote.yaml"),
    ),
];

pub fn source(name: &str) -> Option<&'static str> {
    BUILTIN
        .iter()
        .find(|(builtin, _)| *builtin == name)
        .map(|(_, text)| *text)
}

/// The parsed built-in, or why it didn't parse.
pub fn load(name: &str) -> Result<Workflow, String> {
    let text = source(name).ok_or_else(|| format!("no built-in workflow is named `{name}`"))?;
    let workflow = yaml::parse(text).map_err(|err| format!("built-in workflow `{name}`: {err}"))?;
    if workflow.name != name {
        return Err(format!(
            "built-in workflow `{name}` calls itself `{}`",
            workflow.name
        ));
    }
    Ok(workflow)
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
        let families = allowed
            .iter()
            .map(|worker| (worker.clone(), worker.to_string()))
            .collect();
        let context = Context {
            settings: &settings,
            allowed: &allowed,
            families: &families,
            lookup: &load,
            depth: 0,
            budget_left_usd: None,
            as_you_asked: &[],
        };
        for (name, _) in BUILTIN {
            let workflow = load(name).unwrap_or_else(|err| panic!("{err}"));
            assert!(!workflow.description.is_empty(), "{name}");
            let text = yaml::print(&workflow).unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(yaml::parse(&text).as_ref(), Ok(&workflow), "{name}");
            if let Err(errors) = validate(&workflow, &context) {
                panic!("{name}: {errors:?}");
            }
        }
        assert!(load("no-such-workflow").is_err());
    }
}
