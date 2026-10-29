//! What a script the orchestrator writes must do, read from a rehearsal's agents by their roles,
//! their access and their order, never by the names of their phases: the planner names phases
//! for each request.
//!
//! Two things are checked. Every reviewer, and every judge that signs off on built work, answers
//! with a yes-or-no `accept`, since that is the one field a verdict is read by. And every step the
//! workflow file marks `keep` is carried out: a step before the building starts by agents that
//! only read and work on no task, in the file's order, before any agent writes; a step after it
//! by an agent of its role once the building has started.

use argonauts_core::settings::crew::CrewRole;

use crate::file::{Count, WorkflowFile};
use crate::rehearse::Asked;

/// The labels of the reviewers and sign-off judges whose schema has no boolean `accept`.
pub fn without_verdicts(asked: &[Asked]) -> Vec<String> {
    let first_write = asked.iter().position(|call| call.writes);
    asked
        .iter()
        .enumerate()
        .filter(|(at, call)| {
            let reviews = call.role == CrewRole::Reviewer
                || (call.not_the_author && !call.writes && call.task.is_some());
            let signs_off =
                call.role == CrewRole::Judge && first_write.is_some_and(|first| *at > first);
            (reviews || signs_off) && !call.gives_verdict
        })
        .map(|(_, call)| call.label.clone())
        .collect()
}

/// What the script leaves out of the steps `file` keeps, one line each in role words, such as
/// `Propose: 3 workers with access 'read' and no task, one per worker model, before any agent
/// writes (the script has 1)`. `models` says how many models a role has. Empty when every kept
/// step is carried out.
pub fn left_out(
    file: &WorkflowFile,
    asked: &[Asked],
    models: &dyn Fn(CrewRole) -> u32,
) -> Vec<String> {
    let first_write = asked
        .iter()
        .position(|call| call.writes)
        .unwrap_or(asked.len());
    let planner = file.planner();
    let mut missing = Vec::new();
    // Where the next kept step's agents may start: after the agents of the one before it.
    let mut from = 0;
    // How many agents the kept step before proposed with, which a step that answers them matches.
    let mut proposals: Option<usize> = None;
    for (index, step) in file.steps.iter().enumerate().filter(|(_, step)| step.keep) {
        let before_building = planner.is_some_and(|planner| index <= planner);
        if !before_building {
            let found = asked
                .iter()
                .enumerate()
                .skip(from.max(first_write))
                .find(|(_, call)| call.role == step.role);
            match found {
                Some((at, _)) => from = at + 1,
                None => missing.push(format!(
                    "{}: an agent of the {} role once the building has started",
                    step.name, step.role
                )),
            }
            continue;
        }
        if step.role != CrewRole::Worker {
            let start = from.min(first_write);
            let found = asked[start..first_write]
                .iter()
                .position(|call| call.role == step.role && !call.writes);
            match found {
                Some(at) => from = start + at + 1,
                None => missing.push(format!(
                    "{}: {} after the steps before it and before any agent writes",
                    step.name,
                    a_role(step.role)
                )),
            }
            continue;
        }
        let of_role = models(step.role).max(1) as usize;
        let every_model = step.count == Some(Count::All);
        let needed = match (proposals, step.count) {
            // A step that answers proposals answers each of them.
            (Some(proposed), _) if !step.independent => proposed,
            // Two proposals make a choice.
            (_, Some(Count::All)) if step.independent => of_role.max(2),
            (_, Some(Count::All)) => of_role,
            (_, Some(Count::Fixed(count))) => count as usize,
            (_, None) => 1,
        };
        let readers: Vec<(usize, &Asked)> = asked
            .iter()
            .enumerate()
            .take(first_write)
            .skip(from)
            .filter(|(_, call)| {
                call.role == CrewRole::Worker && !call.writes && call.task.is_none()
            })
            .take(needed)
            .collect();
        let covered = !every_model
            || (0..of_role as u32)
                .all(|model| readers.iter().any(|(_, call)| call.model_index == model));
        if readers.len() < needed || !covered {
            let each = if every_model {
                ", one per worker model"
            } else {
                ""
            };
            let after = if proposals.is_some() && !step.independent {
                ", one for each proposal, after them"
            } else {
                ""
            };
            missing.push(format!(
                "{}: {needed} {} with access 'read' and no task{each}{after}, before any agent \
                 writes (the script has {})",
                step.name,
                if needed == 1 { "worker" } else { "workers" },
                readers.len()
            ));
        }
        if let Some((last, _)) = readers.last() {
            from = last + 1;
        }
        proposals = Some(needed);
    }
    missing
}

fn a_role(role: CrewRole) -> String {
    match role {
        CrewRole::Orchestrator => "an orchestrator".to_owned(),
        other => format!("a {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(label: &str, role: CrewRole, model: u32, writes: bool) -> Asked {
        Asked {
            label: label.to_owned(),
            phase: None,
            role,
            model_index: model,
            task: writes.then(|| "the-task".to_owned()),
            writes,
            not_the_author: false,
            gives_verdict: false,
        }
    }

    fn council() -> WorkflowFile {
        WorkflowFile::parse(crate::COUNCIL_STEPS)
            .expect("reads")
            .expect("has steps")
    }

    fn three(_: CrewRole) -> u32 {
        3
    }

    fn proposed_and_challenged() -> Vec<Asked> {
        let mut asked = Vec::new();
        for n in 0..3 {
            asked.push(call(&format!("propose:p{n}"), CrewRole::Worker, n, false));
        }
        for n in 0..3 {
            asked.push(call(&format!("challenge:p{n}"), CrewRole::Worker, n, false));
        }
        asked.push(call("decide:plan", CrewRole::Judge, 0, false));
        asked.push(call("build:the-task", CrewRole::Worker, 0, true));
        asked
    }

    #[test]
    fn a_council_that_proposes_challenges_and_decides_keeps_its_steps() {
        assert_eq!(
            left_out(&council(), &proposed_and_challenged(), &three),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_council_that_goes_straight_to_building_is_told_what_it_left_out() {
        let asked = vec![call("build:the-task", CrewRole::Worker, 0, true)];
        assert_eq!(
            left_out(&council(), &asked, &three),
            [
                "Propose: 3 workers with access 'read' and no task, one per worker model, before \
                 any agent writes (the script has 0)",
                "Challenge: 3 workers with access 'read' and no task, one per worker model, one \
                 for each proposal, after them, before any agent writes (the script has 0)",
                "Decide: a judge after the steps before it and before any agent writes",
            ]
        );
    }

    #[test]
    fn proposals_from_one_model_or_after_the_building_dont_count() {
        let mut asked = proposed_and_challenged();
        for proposal in asked.iter_mut().take(3) {
            proposal.model_index = 0;
        }
        let missing = left_out(&council(), &asked, &three);
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].starts_with("Propose: 3 workers"), "{missing:?}");

        // A judge that decides only once a worker is writing decides nothing.
        let mut late = proposed_and_challenged();
        let judge = late.remove(6);
        late.push(judge);
        assert_eq!(
            left_out(&council(), &late, &three),
            ["Decide: a judge after the steps before it and before any agent writes"]
        );
    }

    #[test]
    fn one_worker_model_still_proposes_twice() {
        let one = |_: CrewRole| 1;
        let reading = |labels: &[&str]| -> Vec<Asked> {
            let mut asked: Vec<Asked> = labels
                .iter()
                .map(|label| call(label, CrewRole::Worker, 0, false))
                .collect();
            asked.push(call("decide:plan", CrewRole::Judge, 0, false));
            asked
        };
        let once = reading(&["propose:p0", "challenge:p0"]);
        assert_eq!(left_out(&council(), &once, &one).len(), 1);
        let twice = reading(&["propose:p0", "propose:p1", "challenge:p0", "challenge:p1"]);
        assert_eq!(left_out(&council(), &twice, &one), Vec::<String>::new());
    }

    #[test]
    fn a_reviewer_or_a_sign_off_without_accept_is_named() {
        let mut asked = proposed_and_challenged();
        // Decide answers with a plan, which is no verdict.
        asked.push(call("review:the-task", CrewRole::Reviewer, 0, false));
        asked.push(call("signoff:the-task", CrewRole::Judge, 0, false));
        assert_eq!(
            without_verdicts(&asked),
            ["review:the-task", "signoff:the-task"]
        );
        for call in &mut asked {
            call.gives_verdict = true;
        }
        assert!(without_verdicts(&asked).is_empty());
    }
}
