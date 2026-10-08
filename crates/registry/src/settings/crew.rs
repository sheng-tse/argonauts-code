//! The `[crew]` table as people write it: `main` beside the four other roles, or one list for
//! everyone.

use argonauts_core::names::WorkerRef;
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::{CrewRole, Orchestrator};
use toml::{Table, Value};

use super::roles::{Workers, off, unknown};
use super::set_by::SetBy;

const CREW: &str = "crew";
const MAIN: &str = "main";
const MAIN_AGENT: &str = "main_agent";
const LISTS: [&str; 3] = ["workers", "reviewers", "judge"];

/// Rewrites one layer into the keys the settings hold. `crew.main` is `main_agent`, the key
/// `/model` saves, so both spellings mean the same thing in whichever file they sit. A bare
/// list, `crew = ["codex", "claude-code"]`, seats everyone in it as worker, reviewer and judge.
pub(crate) fn spell_out(layer: &mut Table) {
    match layer.remove(CREW) {
        Some(Value::Array(everyone)) => {
            let seats = LISTS
                .into_iter()
                .map(|list| (list.to_owned(), Value::Array(everyone.clone())))
                .collect();
            layer.insert(CREW.to_owned(), Value::Table(seats));
        }
        Some(Value::Table(mut crew)) => {
            if let Some(main) = crew.remove(MAIN) {
                layer.entry(MAIN_AGENT).or_insert(main);
            }
            if !crew.is_empty() {
                layer.insert(CREW.to_owned(), Value::Table(crew));
            }
        }
        Some(other) => {
            layer.insert(CREW.to_owned(), other);
        }
        None => {}
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
    let said = |role: &str| set_by.layer(&format!("{CREW}.{role}")).is_some();
    for (leader, follower) in [("orchestrator", "workers"), ("reviewers", "judge")] {
        if said(leader) && !said(follower) {
            crew.insert(follower.to_owned(), Value::Array(Vec::new()));
        }
    }
}

fn seats(settings: &Settings) -> Vec<(String, &[WorkerRef])> {
    let crew = &settings.crew;
    let orchestrators: &[WorkerRef] = match &crew.orchestrator {
        Orchestrator::Main => &[],
        Orchestrator::Models(models) => models,
    };
    [
        (CrewRole::Orchestrator, orchestrators),
        (CrewRole::Workers, crew.workers.as_slice()),
        (CrewRole::Reviewers, crew.reviewers.as_slice()),
        (CrewRole::Judge, crew.judge.as_slice()),
    ]
    .into_iter()
    .map(|(role, list)| (format!("crew.{role}"), list))
    .collect()
}

/// Seats that name an agent nobody knows, or one that's off.
pub(crate) fn warnings(settings: &Settings, workers: &Workers<'_>) -> Vec<String> {
    let mut warnings = Vec::new();
    for (key, list) in seats(settings) {
        for worker in list {
            if workers.is_off(&worker.worker) {
                warnings.push(off(&key, worker));
            } else if !workers.is_known(&worker.worker) {
                warnings.push(unknown(&key, worker));
            }
        }
    }
    warnings
}

/// What's wrong with the seats a setup proposes for `key`: an agent nobody knows, or one
/// that is off. In a file you wrote these are warnings, since you may install the agent
/// later; a setup Argonauts writes for you names only agents that can take the seat.
pub fn seat_problems(settings: &Settings, key: &str, seats: &[WorkerRef]) -> Vec<String> {
    let turned_on: Vec<String> = settings
        .agents
        .iter()
        .filter(|(_, agent)| agent.enabled)
        .map(|(id, _)| id.clone())
        .collect();
    let workers = Workers::new(settings).resolving(&[], turned_on);
    seats
        .iter()
        .filter_map(|worker| {
            if !workers.is_known(&worker.worker) {
                Some(unknown(key, worker))
            } else if workers.is_off(&worker.worker) {
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
    fn a_proposed_seat_nobody_knows_is_a_problem() {
        let settings = Settings::default();
        let gemini: WorkerRef = "gemini".parse().unwrap();
        let problems = seat_problems(&settings, "crew.reviewers", &[gemini]);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("gemini"), "{problems:?}");
        let codex: WorkerRef = "codex/gpt-5.6".parse().unwrap();
        assert!(seat_problems(&settings, "crew.reviewers", &[codex]).is_empty());
    }

    fn spelled(text: &str) -> String {
        let mut layer: Table = toml::from_str(text).expect("a table");
        spell_out(&mut layer);
        toml::to_string(&layer).expect("writes")
    }

    #[test]
    fn main_is_the_main_agent_and_the_rest_stays() {
        let out = spelled("[crew]\nmain = \"codex/gpt-5.6\"\njudge = \"claude-code\"\n");
        assert!(out.contains("main_agent = \"codex/gpt-5.6\""), "{out}");
        assert!(out.contains("judge = \"claude-code\""), "{out}");
        assert!(!out.contains("main = "), "{out}");
        let only_main = spelled("[crew]\nmain = \"codex\"\n");
        assert_eq!(only_main.trim(), "main_agent = \"codex\"");
    }

    #[test]
    fn a_main_agent_in_the_same_file_wins_over_crew_main() {
        let out = spelled("main_agent = \"claude-code\"\n[crew]\nmain = \"codex\"\n");
        assert!(out.contains("main_agent = \"claude-code\""), "{out}");
    }

    #[test]
    fn one_list_seats_everyone_in_every_role_but_the_orchestrator() {
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
