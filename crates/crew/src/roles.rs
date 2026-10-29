//! Who may fill a role: `roles.<role>.models` when set, else the crew. Every selection starts
//! here, and role conflicts (a worker reviewing its own task, a judge of its own plan) are
//! blocked here.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::council::Seating;
use argonauts_core::names::{ModelRef, RoleName, SeatId};
use argonauts_core::session_runner::model_run;
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::CrewRole;

use crate::engine::host::Host;

pub mod tasks;

/// In order, without repeats: `roles.<role>.models` when set, else the models of the one of
/// the five roles it follows, or of the role that one follows while it has none of its own. The
/// main role is `crew.main` alone, and a role nobody can fill falls back to it too.
pub fn candidates(settings: &Settings, role: RoleName) -> Vec<ModelRef> {
    if role == RoleName::Main {
        return vec![settings.crew.main.clone()];
    }
    let configured = settings
        .roles
        .get(&role)
        .map(|role| role.models.as_slice())
        .unwrap_or_default();
    let crew_role = CrewRole::followed_by(role);
    let pool = if !configured.is_empty() {
        configured.to_vec()
    } else if let Some(followed) = settings.crew.followed(crew_role) {
        // What isn't set follows what is, the followed role's own workers included: a judge
        // follows reviewers named for the reviewer role, as your words name them.
        candidates(settings, followed.fine_role())
    } else {
        settings.models_for(crew_role)
    };
    let mut workers: Vec<ModelRef> = Vec::with_capacity(pool.len());
    for worker in pool {
        if !workers.contains(&worker) {
            workers.push(worker);
        }
    }
    if workers.is_empty() {
        workers.push(settings.crew.main.clone());
    }
    workers
}

/// The worker's model provider from the registry.
pub fn provider(host: &Host, worker: &ModelRef) -> String {
    host.services().registry.provider(worker)
}

/// The model a session of `worker` runs, as its runner names it, so a bare CLI counts as the
/// model it runs.
pub fn runs(host: &Host, worker: &ModelRef) -> ModelRef {
    model_run(
        Some(host.services().workers.as_ref()),
        host.settings(),
        worker,
    )
}

/// What the transcript calls `worker`, such as `GPT 5.6 on Codex`, rather than its id.
pub fn display_name(worker: &ModelRef) -> String {
    DisplayNames::default().worker(worker)
}

/// The model alone where it runs on its own CLI, such as `Opus 5.5`, for a narrow place.
pub fn short_name(worker: &ModelRef) -> String {
    DisplayNames::default().short(worker)
}

/// What you read a council seat by: its model, numbered when it sits more than once, as
/// `Opus 5.5 · 2`. `None` for a seat that doesn't sit.
pub fn seat_name(seat: &SeatId, seating: &Seating) -> Option<String> {
    let seat = crate::council::seats::find(seating, seat)?;
    Some(crate::council::seats::name(seat, &DisplayNames::default()))
}

/// Why `model` can't take `role` for this task, if it can't. `worker` is whoever made the work
/// the role judges: the task's worker, or the plan's author for a judge.
pub fn conflict(
    role: RoleName,
    model: &ModelRef,
    worker: Option<&ModelRef>,
    providers: &dyn Fn(&ModelRef) -> String,
) -> Option<String> {
    let worker = worker?;
    let what = match role {
        RoleName::Reviewer => "review its own task",
        RoleName::Judge => "judge its own work",

        _ => return None,
    };
    same_agent(model, worker, providers).then(|| format!("{} can't {what}", display_name(model)))
}

/// Two references name the same agent when the CLI matches and so does the model. A
/// reference without a model runs the CLI's default, which only the provider can tell apart.
fn same_agent(a: &ModelRef, b: &ModelRef, providers: &dyn Fn(&ModelRef) -> String) -> bool {
    if a.agent != b.agent {
        return false;
    }
    match (&a.model, &b.model) {
        (Some(x), Some(y)) => x == y,
        _ => providers(a) == providers(b),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::roles::RoleSettings;

    use super::*;

    fn worker(text: &str) -> ModelRef {
        text.parse().unwrap()
    }

    /// Notes such as "GPT 5.6 Luna on Codex's draft failed" name a model as people read it,
    /// never by its id.
    #[test]
    fn a_worker_is_named_as_people_read_it() {
        let name = display_name(&worker("codex/gpt-5.6-luna"));
        assert_eq!(name, "GPT 5.6 Luna on Codex");
        assert!(!name.contains("codex/") && !name.contains("gpt-5.6-luna"));
    }

    fn provider_of(worker: &ModelRef) -> String {
        match worker.agent.as_str() {
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
                models: vec![worker("agy/gemini-3.6-pro")],
                ..RoleSettings::default()
            },
        );
        assert_eq!(
            candidates(&settings, RoleName::Reviewer),
            vec![worker("agy/gemini-3.6-pro")]
        );
        assert_eq!(
            candidates(&settings, RoleName::Main),
            vec![settings.crew.main.clone()]
        );
        settings.crew.clear();
        assert_eq!(
            candidates(&settings, RoleName::Worker),
            vec![settings.crew.main.clone()]
        );
    }

    #[test]
    fn role_conflicts_blocked() {
        let opus = worker("claude-code/opus");
        let blocked = [RoleName::Reviewer, RoleName::Judge];
        for role in blocked {
            let reason = conflict(role, &opus, Some(&opus), &provider_of);
            assert!(reason.is_some(), "{role:?} of its own work");
        }
        assert_eq!(
            conflict(RoleName::Reviewer, &opus, Some(&opus), &provider_of).as_deref(),
            Some("Opus on Claude Code can't review its own task")
        );
        // The CLI's default model may be the worker's model, and the provider says so.
        let default_model = worker("claude-code");
        assert!(
            conflict(
                RoleName::Reviewer,
                &default_model,
                Some(&opus),
                &provider_of
            )
            .is_some()
        );
        // Another model on the same CLI is another agent, as a same-agent crew needs.
        let sonnet = worker("claude-code/sonnet");
        assert!(conflict(RoleName::Reviewer, &sonnet, Some(&opus), &provider_of).is_none());
        assert!(
            conflict(
                RoleName::Reviewer,
                &worker("codex"),
                Some(&opus),
                &provider_of
            )
            .is_none()
        );
        // Roles that judge nobody's work have nothing to conflict with.
        assert!(conflict(RoleName::Worker, &opus, Some(&opus), &provider_of).is_none());
        assert!(conflict(RoleName::Reviewer, &opus, None, &provider_of).is_none());
    }
}
