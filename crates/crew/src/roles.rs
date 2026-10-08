//! Who may fill a role: `roles.<role>.workers` when set, else the crew. Every selection starts
//! here, and role conflicts (an owner reviewing its own card, a judge of its own plan) are
//! blocked here.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::CrewRole;

use crate::engine::host::Host;

pub mod cards;

/// In order, without repeats: `roles.<role>.workers` when set, else the seats of the one of the
/// five roles it follows, or of the role that one follows while it has none of its own. The
/// main role is `main_agent` alone, and a role nobody can fill falls back to it too.
pub fn candidates(settings: &Settings, role: RoleName) -> Vec<WorkerRef> {
    if role == RoleName::Main {
        return vec![settings.main_agent.clone()];
    }
    let configured = settings
        .roles
        .get(&role)
        .map(|role| role.workers.as_slice())
        .unwrap_or_default();
    let seat = CrewRole::followed_by(role);
    let pool = if !configured.is_empty() {
        configured.to_vec()
    } else if let Some(followed) = settings.crew.followed(seat) {
        // What isn't set follows what is, the followed role's own workers included: a judge
        // follows reviewers named for the reviewer role, as your words name them.
        candidates(settings, followed.fine_role())
    } else {
        settings.seats(seat)
    };
    let mut workers: Vec<WorkerRef> = Vec::with_capacity(pool.len());
    for worker in pool {
        if !workers.contains(&worker) {
            workers.push(worker);
        }
    }
    if workers.is_empty() {
        workers.push(settings.main_agent.clone());
    }
    workers
}

/// The worker's model family from the registry.
pub fn family(host: &Host, worker: &WorkerRef) -> String {
    host.services().registry.family(worker)
}

/// What the transcript calls `worker`, such as `GPT 5.6 on Codex`, rather than its id.
pub fn display_name(worker: &WorkerRef) -> String {
    DisplayNames::default().worker(worker)
}

/// The model alone where it runs on its own CLI, such as `Opus 5.5`, for a narrow place.
pub fn short_name(worker: &WorkerRef) -> String {
    DisplayNames::default().short(worker)
}

/// Why `worker` can't take `role` for this card, if it can't. `owner` is whoever made the work
/// the role judges: the card's owner, or the plan's author for a judge.
pub fn conflict(
    role: RoleName,
    worker: &WorkerRef,
    owner: Option<&WorkerRef>,
    families: &dyn Fn(&WorkerRef) -> String,
) -> Option<String> {
    let owner = owner?;
    let what = match role {
        RoleName::Reviewer => "review its own task",
        RoleName::Adjudicator => "adjudicate its own task",
        RoleName::Judge => "judge its own plan",
        RoleName::Approver => "approve its own work",
        _ => return None,
    };
    same_agent(worker, owner, families).then(|| format!("{worker} can't {what}"))
}

/// Two references name the same agent when the harness matches and so does the model. A
/// reference without a model runs the harness's default, which only the family can tell apart.
fn same_agent(a: &WorkerRef, b: &WorkerRef, families: &dyn Fn(&WorkerRef) -> String) -> bool {
    if a.worker != b.worker {
        return false;
    }
    match (&a.model, &b.model) {
        (Some(x), Some(y)) => x == y,
        _ => families(a) == families(b),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::roles::RoleSettings;

    use super::*;

    fn worker(text: &str) -> WorkerRef {
        text.parse().unwrap()
    }

    fn family_of(worker: &WorkerRef) -> String {
        match worker.worker.as_str() {
            "claude-code" => "claude".to_owned(),
            "codex" => "gpt".to_owned(),
            _ => "gemini".to_owned(),
        }
    }

    #[test]
    fn candidates_come_from_the_role_else_the_crew() {
        let mut settings = Settings {
            crew: vec![worker("claude-code/opus"), worker("codex"), worker("codex")].into(),
            ..Settings::default()
        };
        assert_eq!(
            candidates(&settings, RoleName::Reviewer),
            vec![worker("claude-code/opus"), worker("codex")]
        );
        settings.roles.insert(
            RoleName::Reviewer,
            RoleSettings {
                workers: vec![worker("agy/gemini-3.6-pro")],
                ..RoleSettings::default()
            },
        );
        assert_eq!(
            candidates(&settings, RoleName::Reviewer),
            vec![worker("agy/gemini-3.6-pro")]
        );
        assert_eq!(
            candidates(&settings, RoleName::Main),
            vec![settings.main_agent.clone()]
        );
        settings.crew.clear();
        assert_eq!(
            candidates(&settings, RoleName::Worker),
            vec![settings.main_agent.clone()]
        );
    }

    #[test]
    fn role_conflicts_blocked() {
        let owner = worker("claude-code/opus");
        let blocked = [
            RoleName::Reviewer,
            RoleName::Adjudicator,
            RoleName::Judge,
            RoleName::Approver,
        ];
        for role in blocked {
            let reason = conflict(role, &owner, Some(&owner), &family_of);
            assert!(reason.is_some(), "{role:?} of its own work");
        }
        assert_eq!(
            conflict(RoleName::Reviewer, &owner, Some(&owner), &family_of).as_deref(),
            Some("claude-code/opus can't review its own task")
        );
        // The harness's default model may be the owner's model, and the family says so.
        let default_model = worker("claude-code");
        assert!(conflict(RoleName::Reviewer, &default_model, Some(&owner), &family_of).is_some());
        // Another model on the same harness is another agent, as a same-vendor crew needs.
        let sonnet = worker("claude-code/sonnet");
        assert!(conflict(RoleName::Reviewer, &sonnet, Some(&owner), &family_of).is_none());
        assert!(
            conflict(
                RoleName::Reviewer,
                &worker("codex"),
                Some(&owner),
                &family_of
            )
            .is_none()
        );
        // Roles that judge nobody's work have nothing to conflict with.
        assert!(conflict(RoleName::Worker, &owner, Some(&owner), &family_of).is_none());
        assert!(conflict(RoleName::Reviewer, &owner, None, &family_of).is_none());
    }
}
