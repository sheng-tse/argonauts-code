//! Who may fill a role: `roles.<role>.workers` when set, else the crew. Every selection starts
//! here, and role conflicts (an owner reviewing its own card, a judge of its own plan) are
//! blocked here.

use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::Settings;

use crate::engine::host::Host;

/// In order, without repeats. The default role is `default_agent` alone, and an empty crew
/// falls back to it too.
pub fn candidates(settings: &Settings, role: RoleName) -> Vec<WorkerRef> {
    if role == RoleName::Default {
        return vec![settings.default_agent.clone()];
    }
    let configured = settings
        .roles
        .get(&role)
        .map(|role| role.workers.as_slice())
        .unwrap_or_default();
    let pool = if configured.is_empty() {
        settings.crew.as_slice()
    } else {
        configured
    };
    let mut workers: Vec<WorkerRef> = Vec::with_capacity(pool.len());
    for worker in pool {
        if !workers.contains(worker) {
            workers.push(worker.clone());
        }
    }
    if workers.is_empty() {
        workers.push(settings.default_agent.clone());
    }
    workers
}

/// The worker's model family from the registry.
pub fn family(host: &Host, worker: &WorkerRef) -> String {
    host.services().registry.family(worker)
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
        RoleName::Reviewer => "review its own card",
        RoleName::Adjudicator => "adjudicate its own card",
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
            crew: vec![worker("claude-code/opus"), worker("codex"), worker("codex")],
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
            candidates(&settings, RoleName::Default),
            vec![settings.default_agent.clone()]
        );
        settings.crew.clear();
        assert_eq!(
            candidates(&settings, RoleName::Owner),
            vec![settings.default_agent.clone()]
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
            Some("claude-code/opus can't review its own card")
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
        assert!(conflict(RoleName::Owner, &owner, Some(&owner), &family_of).is_none());
        assert!(conflict(RoleName::Reviewer, &owner, None, &family_of).is_none());
    }
}
