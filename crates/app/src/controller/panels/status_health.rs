//! What the Status tab says of health: each CLI's models, and how each stands on its own. The models
//! are the ones you use (the main agent's and the crew's), any the health board holds something
//! against, then the rest the registry knows for the CLI.

use argonauts_core::RunId;
use argonauts_core::app::status_tab::{
    CliHealth, CliStatus, Covers, HealthRows, ModelStanding, Standing,
};
use argonauts_core::base::layout::run_files;
use argonauts_core::base::names::{AgentId, ModelRef};
use argonauts_core::models::limits::{AgentHealth, HealthState, ModelHealth, ModelState};
use argonauts_core::models::registry::model_run;
use argonauts_core::runs::events::RunEvent;
use argonauts_core::runs::journal;
use argonauts_core::settings::Settings;

use crate::controller::Ctx;
use crate::views::sessions::RunFacts;

/// Rows a CLI shows at most, so one with a long catalog doesn't push the others off the screen.
/// The models you use and the ones in trouble come first.
const MODELS_SHOWN: usize = 6;

/// What a CLI's default is called when nothing says which model it runs.
const DEFAULT_MODEL: &str = "its default";

/// A job one model does in another's place right now.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::controller) struct InPlace {
    /// The job's label, such as `review:due-tests`.
    label: String,
    by: ModelRef,
    asked: ModelRef,
}

/// The jobs of `run` that a fallback is doing now, read from the run's own log.
pub(in crate::controller) fn at_work(ctx: &Ctx, run: &RunId) -> Vec<InPlace> {
    let path = ctx.env.run_dir(run).join(run_files::EVENTS);
    journal::read_all(&path).map_or_else(|_| Vec::new(), |back| in_place(&back.events))
}

/// The sessions still going that run for a model other than their own: a build task that fell
/// back, or a job whose asked model was out. A run that ended has nobody at work.
fn in_place(events: &[RunEvent]) -> Vec<InPlace> {
    let facts = RunFacts::of(events);
    if facts.finished_at.is_some() {
        return Vec::new();
    }
    let sessions = facts
        .sessions
        .iter()
        .filter(|session| session.ended.is_none());
    sessions
        .filter_map(|session| {
            let asked = session
                .fallback_for
                .clone()
                .or(session.started.asked.clone())?;
            (asked != session.worker).then(|| InPlace {
                label: session.label(),
                by: session.worker.clone(),
                asked,
            })
        })
        .collect()
}

pub(in crate::controller) fn rows(
    ctx: &Ctx,
    clis: &[CliStatus],
    at_work: &[InPlace],
) -> HealthRows {
    let settings = ctx.settings();
    let span = |value: &argonauts_core::settings::Span| value.parse().ok().flatten();
    HealthRows {
        clis: clis
            .iter()
            .map(|cli| cli_health(ctx, cli, at_work))
            .collect(),
        quiet_secs: span(&settings.health.window).map(|window| window.as_secs()),
        cooldown_secs: span(&settings.health.cooldown).map_or(0, |cooldown| cooldown.as_secs()),
    }
}

fn cli_health(ctx: &Ctx, cli: &CliStatus, at_work: &[InPlace]) -> CliHealth {
    let agent_id = AgentId(cli.id.clone());
    let models = if cli.installed {
        let board = ctx.services.health.as_ref();
        let whole = board.state(&agent_id);
        let known = board.models(&agent_id);
        // Whose work each of this CLI's models does, by the names the rows show.
        let covering: Vec<(String, Covers)> = at_work
            .iter()
            .filter(|job| job.by.agent == cli.id)
            .map(|job| (name_of(ctx, &job.by), covers(ctx, job)))
            .collect();
        models_of(ctx, &cli.id, &known)
            .iter()
            .take(MODELS_SHOWN)
            .map(|model| {
                let name = name_of(ctx, model);
                let job = covering.iter().find(|(by, _)| *by == name);
                ModelStanding {
                    covers: job.map(|(_, covers)| covers.clone()),
                    name,
                    ..standing(&whole, &own_health(&known, model))
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    CliHealth {
        id: cli.id.clone(),
        name: cli.name.clone(),
        installed: cli.installed,
        main: cli.main,
        models,
    }
}

/// What a model's row says of the job it does in another's place.
fn covers(ctx: &Ctx, job: &InPlace) -> Covers {
    Covers {
        job: job.label.clone(),
        cli: job.asked.agent.clone(),
        model: name_of(ctx, &job.asked),
    }
}

/// The CLI's models in the order they're listed: the ones you use, the ones the board holds
/// something against, then the rest the registry knows.
fn models_of(ctx: &Ctx, cli: &str, known: &[ModelHealth]) -> Vec<ModelRef> {
    let mut models: Vec<ModelRef> = in_use(ctx.settings());
    models.extend(known.iter().map(|health| health.model.clone()));
    let listed = ctx.services.registry.list();
    models.extend(
        listed
            .into_iter()
            .filter(|entry| !entry.retired)
            .map(|entry| entry.id),
    );
    // One row a model, however it is named: by an alias, by its id, or as its CLI's default.
    let registry = ctx.services.registry.as_ref();
    let models = models
        .into_iter()
        .map(|model| match model_run(registry, &model) {
            Some(run) => ModelRef::new(model.agent, Some(run)),
            None => model,
        })
        .collect();
    once_each(models, cli)
}

/// Those of `models` on `cli`, each once, in the order it first comes.
fn once_each(models: Vec<ModelRef>, cli: &str) -> Vec<ModelRef> {
    let mut kept: Vec<ModelRef> = Vec::new();
    for model in models {
        if model.agent == cli && !kept.contains(&model) {
            kept.push(model);
        }
    }
    kept
}

/// The main agent, then everyone the crew names.
fn in_use(settings: &Settings) -> Vec<ModelRef> {
    let mut models = vec![settings.crew.main.clone()];
    models.extend(settings.crew.members());
    models
}

/// `Opus 5.5`, or the id as you wrote it for a model the catalog doesn't name.
fn name_of(ctx: &Ctx, model: &ModelRef) -> String {
    match model.model.as_deref() {
        Some(id) => ctx
            .names
            .model(&model.agent, id)
            .unwrap_or_else(|| id.to_owned()),
        None => DEFAULT_MODEL.to_owned(),
    }
}

fn own_health(known: &[ModelHealth], model: &ModelRef) -> ModelHealth {
    known
        .iter()
        .find(|health| &health.model == model)
        .cloned()
        .unwrap_or_else(|| ModelHealth::healthy(model))
}

/// One model's row but for its name. A CLI that is down or under test speaks for every model
/// on it, with the model's own trouble as the reason where it has one.
fn standing(whole: &AgentHealth, own: &ModelHealth) -> ModelStanding {
    let state = Standing::of(whole.state, own.state);
    let speaks_for_itself = match whole.state {
        HealthState::Down => own.state == ModelState::Out,
        HealthState::Testing => false,
        HealthState::Healthy | HealthState::Degraded => true,
    };
    let (why, next_check) = if speaks_for_itself {
        (own.reason.clone(), own.next_test_at)
    } else {
        (whole.reason.clone(), whole.next_test_at)
    };
    let troubled = state != Standing::Healthy;
    ModelStanding {
        name: String::new(),
        state,
        covers: None,
        why: why.filter(|_| troubled).unwrap_or_default(),
        // A CLI that is down is tested as a whole, at its own time.
        next_check: match whole.state {
            HealthState::Down => whole.next_test_at,
            _ => next_check.filter(|_| troubled),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::{session_ended, session_started};
    use argonauts_core::base::names::{RoleName, SessionKey};
    use argonauts_core::models::limits::Trouble;
    use argonauts_core::runs::events::RunEventKind;
    use argonauts_core::runs::events::session::{EndReason, SessionEvent};
    use time::macros::datetime;

    fn whole(state: HealthState, reason: Option<&str>) -> AgentHealth {
        AgentHealth {
            agent_id: AgentId("claude-code".into()),
            state,
            since: datetime!(2026-10-08 13:58 UTC),
            reason: reason.map(str::to_owned),
            next_test_at: (state == HealthState::Down).then_some(datetime!(2026-10-08 14:05 UTC)),
            disabled: None,
        }
    }

    fn opus(state: ModelState) -> ModelHealth {
        let out = state == ModelState::Out;
        ModelHealth {
            state,
            reason: out.then(|| "2 overload errors in a row".to_owned()),
            trouble: out.then_some(Trouble::Overload),
            next_test_at: out.then_some(datetime!(2026-10-08 14:03 UTC)),
            ..ModelHealth::healthy(&"claude-code/opus".parse().unwrap())
        }
    }

    #[test]
    fn a_model_that_is_out_says_why_and_when_it_is_tested() {
        let row = standing(&whole(HealthState::Healthy, None), &opus(ModelState::Out));
        assert_eq!(row.state, Standing::Overloaded);
        assert_eq!(row.why, "2 overload errors in a row");
        assert_eq!(row.next_check, Some(datetime!(2026-10-08 14:03 UTC)));
    }

    #[test]
    fn a_healthy_model_on_a_healthy_cli_says_nothing() {
        let row = standing(
            &whole(HealthState::Healthy, Some("the errors stopped")),
            &opus(ModelState::Healthy),
        );
        assert_eq!(row.state, Standing::Healthy);
        assert_eq!((row.why.as_str(), row.next_check), ("", None));
    }

    #[test]
    fn a_cli_that_is_down_speaks_for_every_model_on_it() {
        let down = whole(HealthState::Down, Some("two of its models are out"));
        let out = standing(&down, &opus(ModelState::Out));
        assert_eq!(out.state, Standing::Down);
        assert_eq!(out.why, "2 overload errors in a row");
        assert_eq!(out.next_check, Some(datetime!(2026-10-08 14:05 UTC)));

        let fine = standing(&down, &opus(ModelState::Healthy));
        assert_eq!(fine.state, Standing::Down);
        assert_eq!(fine.why, "two of its models are out");
        assert_eq!(fine.next_check, Some(datetime!(2026-10-08 14:05 UTC)));
    }

    /// A reviewer's session that Sonnet runs because Opus, the one asked, was out.
    fn review_in_opus_place(seq: u64, key: &SessionKey) -> RunEvent {
        let mut event = session_started(seq, key, None, RoleName::Reviewer, "claude-code");
        if let RunEventKind::Session(SessionEvent::Started(started)) = &mut event.kind {
            started.worker = "claude-code/sonnet".parse().unwrap();
            started.label = Some("review:due-tests".to_owned());
            started.asked = Some("claude-code/opus".parse().unwrap());
        }
        event
    }

    #[test]
    fn a_job_done_in_anothers_place_is_named_while_it_goes_on() {
        let (review, build) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let mut events = vec![
            review_in_opus_place(0, &review),
            session_started(1, &build, None, RoleName::Worker, "codex"),
        ];
        let at_work = in_place(&events);
        assert_eq!(at_work.len(), 1, "{at_work:?}");
        assert_eq!(at_work[0].label, "review:due-tests");
        assert_eq!(at_work[0].by.to_string(), "claude-code/sonnet");
        assert_eq!(at_work[0].asked.to_string(), "claude-code/opus");

        // Once the review is in, the model does nobody's work but its own.
        events.push(session_ended(2, &review, EndReason::Completed, 41_000));
        assert_eq!(in_place(&events), []);
    }

    #[test]
    fn the_models_you_use_come_first_and_each_once() {
        let mut settings = Settings::default();
        settings.crew.main = "claude-code/opus".parse().unwrap();
        settings.crew.worker = vec![
            "claude-code/sonnet".parse().unwrap(),
            "codex".parse().unwrap(),
            "claude-code/opus".parse().unwrap(),
        ];
        let mut models = in_use(&settings);
        // One the board holds something against, and the registry's own list after it.
        models.push("claude-code/haiku".parse().unwrap());
        models.push("claude-code/sonnet".parse().unwrap());
        let listed = once_each(models, "claude-code");
        let names: Vec<String> = listed.iter().map(ToString::to_string).collect();
        assert_eq!(names[..2], ["claude-code/opus", "claude-code/sonnet"]);
        assert_eq!(names.last().map(String::as_str), Some("claude-code/haiku"));
        let mut once = names.clone();
        once.dedup();
        once.sort();
        once.dedup();
        assert_eq!(once.len(), names.len(), "{names:?}");
        assert!(names.iter().all(|name| name.starts_with("claude-code")));
    }
}
