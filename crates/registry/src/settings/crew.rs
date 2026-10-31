//! The `[crew]` table as people write it: `main` beside the four other roles, or one list for
//! everyone.

use argonauts_core::base::names::ModelRef;
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::{CrewRole, Orchestrator};
use toml::{Table, Value};

use super::roles::{AgentIds, off, unknown};
use super::set_by::SetBy;

const CREW: &str = "crew";
const LISTS: [CrewRole; 3] = [CrewRole::Worker, CrewRole::Reviewer, CrewRole::Judge];

/// Rewrites one layer into the keys the settings hold: a bare list,
/// `crew = ["codex", "claude-code"]`, puts everyone in it in the worker, reviewer and judge lists.
pub(crate) fn spell_out(layer: &mut Table) {
    if let Some(Value::Array(everyone)) = layer.get(CREW) {
        let lists = LISTS
            .into_iter()
            .map(|role| (role.as_str().to_owned(), Value::Array(everyone.clone())))
            .collect();
        layer.insert(CREW.to_owned(), Value::Table(lists));
    }
}

/// What isn't set follows what is, across every layer: when some file, flag
/// or request sets the orchestrator and none sets the workers, the workers follow it, and a
/// judge nobody set follows the reviewers someone did. The built-in lists stay only where
/// nothing was said of either.
pub(crate) fn follow_what_is_set(merged: &mut Table, set_by: &SetBy) {
    let Some(Value::Table(crew)) = merged.get_mut(CREW) else {
        return;
    };
    let said = |role: CrewRole| set_by.layer(&format!("{CREW}.{role}")).is_some();
    let follows = [
        (CrewRole::Orchestrator, CrewRole::Worker),
        (CrewRole::Reviewer, CrewRole::Judge),
    ];
    for (followed, follower) in follows {
        if said(followed) && !said(follower) {
            crew.insert(follower.as_str().to_owned(), Value::Array(Vec::new()));
        }
    }
}

fn crew_lists(settings: &Settings) -> Vec<(String, &[ModelRef])> {
    let crew = &settings.crew;
    let orchestrators: &[ModelRef] = match &crew.orchestrator {
        Orchestrator::Main => &[],
        Orchestrator::Models(models) => models,
    };
    [
        (CrewRole::Orchestrator, orchestrators),
        (CrewRole::Worker, crew.worker.as_slice()),
        (CrewRole::Reviewer, crew.reviewer.as_slice()),
        (CrewRole::Judge, crew.judge.as_slice()),
    ]
    .into_iter()
    .map(|(role, list)| (format!("crew.{role}"), list))
    .collect()
}

/// Crew lists that name an agent nobody knows, or one that's off.
pub(crate) fn warnings(settings: &Settings, agents: &AgentIds<'_>) -> Vec<String> {
    let mut warnings = Vec::new();
    for (key, list) in crew_lists(settings) {
        for worker in list {
            if agents.is_off(&worker.agent) {
                warnings.push(off(&key, worker));
            } else if !agents.is_known(&worker.agent) {
                warnings.push(unknown(&key, worker));
            }
        }
    }
    warnings
}

/// What's wrong with the agents a setup proposes for `key`: an agent nobody knows, or one
/// that is off. In a file you wrote these are warnings, since you may install the agent
/// later; a setup Argonauts Code writes for you names only agents that can take part.
pub fn crew_list_problems(settings: &Settings, key: &str, models: &[ModelRef]) -> Vec<String> {
    let turned_on: Vec<String> = settings
        .agents
        .iter()
        .filter(|(_, agent)| agent.enabled)
        .map(|(id, _)| id.clone())
        .collect();
    let agents = AgentIds::new(settings).resolving(&[], turned_on);
    models
        .iter()
        .filter_map(|worker| {
            if !agents.is_known(&worker.agent) {
                Some(unknown(key, worker))
            } else if agents.is_off(&worker.agent) {
                Some(off(key, worker))
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proposed_agent_nobody_knows_is_a_problem() {
        let settings = Settings::default();
        let gemini: ModelRef = "gemini".parse().unwrap();
        let problems = crew_list_problems(&settings, "crew.reviewer", &[gemini]);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("gemini"), "{problems:?}");
        let codex: ModelRef = "codex/gpt-5.6".parse().unwrap();
        assert!(crew_list_problems(&settings, "crew.reviewer", &[codex]).is_empty());
    }

    fn spelled(text: &str) -> String {
        let mut layer: Table = toml::from_str(text).expect("a table");
        spell_out(&mut layer);
        toml::to_string(&layer).expect("writes")
    }

    #[test]
    fn a_crew_table_stays_as_written() {
        let out = spelled("[crew]\nmain = \"codex/gpt-5.6\"\njudge = \"claude-code\"\n");
        assert!(out.contains("main = \"codex/gpt-5.6\""), "{out}");
        assert!(out.contains("judge = \"claude-code\""), "{out}");
    }

    #[test]
    fn one_list_puts_everyone_in_every_role_but_the_orchestrator() {
        let out = spelled("crew = [\"codex\", \"claude-code\"]\n");
        for list in LISTS {
            assert!(
                out.contains(&format!("{list} = [\"codex\", \"claude-code\"]")),
                "{out}"
            );
        }
        assert!(!out.contains("orchestrator"), "{out}");
    }
}
