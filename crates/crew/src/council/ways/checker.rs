//! Who checks a task's work at a step of its way. Code picks the checker, never a model, and
//! picks from another maker than the worker whenever one is set up.

use argonauts_core::TaskId;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::council::{CheckRung, Seat};
use argonauts_core::runs::events::task::{TaskSpec, Way};
use argonauts_core::settings::Settings;
use argonauts_core::settings::council::SameModelCheck;
use argonauts_core::settings::crew::CrewRole;

use super::{PickedBy, Setup, TaskStep};
use crate::council::{checks, seats};
use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::roles;
use crate::task::support::record_of;

/// The model whose fresh session takes a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checker {
    pub model: ModelRef,
    /// No other maker is set up, so a fresh session of the model that made the work checks it.
    pub same_maker: bool,
}

impl Checker {
    /// The ladder's rung the checker came from, as the council records it.
    pub fn rung(&self) -> CheckRung {
        if self.same_maker {
            CheckRung::SameMaker
        } else {
            CheckRung::OtherMaker
        }
    }
}

/// A council's checks ladder, with the step's role first among the models set up. The first
/// rung that exists wins:
///
/// 1. A council's seat from another maker than `worker`, a checker seat first.
/// 2. A model of the step's role, `pool`, from another maker.
/// 3. Any model `set_up` from another maker, a local open-weight model included.
/// 4. `worker` itself in a fresh session that doesn't know who made the work.
///
/// Another model of the worker's maker is never picked: a model still favours its own maker's
/// work, and the sibling is usually the weaker reader.
pub fn ladder(
    worker: &ModelRef,
    seats: &[Seat],
    pool: &[ModelRef],
    set_up: &[ModelRef],
    provider: &dyn Fn(&ModelRef) -> String,
) -> Checker {
    let models: Vec<ModelRef> = pool.iter().chain(set_up).cloned().collect();
    // The plan's approval asks once for every same-maker check, so the ladder needn't.
    let rung = checks::ladder(worker, seats, &models, provider, SameModelCheck::Allow);
    Checker {
        model: rung.checker,
        same_maker: rung.rung == CheckRung::SameMaker,
    }
}

/// A council's seats, which check before anyone else; none outside a council.
fn seats_of(names: &DisplayNames, settings: &Settings) -> Vec<Seat> {
    if Setup::of(settings) != Setup::Council {
        return Vec::new();
    }
    seats::seat(settings, names).unwrap_or_default()
}

/// Who checks `task` at a step of `role`. `None` when the task has no worker yet.
pub(crate) fn of_task(host: &Host, task: &TaskId, role: CrewRole) -> EngineResult<Option<Checker>> {
    let record = record_of(host, task)?;
    let Some(worker) = record.worker.or(record.spec.worker) else {
        return Ok(None);
    };
    let settings = host.settings();
    let provider = |model: &ModelRef| roles::provider(host, model);
    let pool = roles::candidates(settings, role.fine_role());
    let set_up = set_up(settings, &host.services().workers.available());
    Ok(Some(ladder(
        &worker,
        &seats_of(host.names(), settings),
        &pool,
        &set_up,
        &provider,
    )))
}

/// Every model set up for a role that reads or builds work and is ready to run.
pub(crate) fn set_up(settings: &Settings, available: &[ModelRef]) -> Vec<ModelRef> {
    let mut models: Vec<ModelRef> = Vec::new();
    for role in [RoleName::Reviewer, RoleName::Judge, RoleName::Worker] {
        for model in roles::candidates(settings, role) {
            if available.contains(&model) && !models.contains(&model) {
                models.push(model);
            }
        }
    }
    models
}

/// The line under a check a fresh session of the worker's own model took.
pub fn same_maker_line(names: &DisplayNames, model: &ModelRef) -> String {
    format!(
        "Same maker: {} made it too, as no other maker is set up · it may favour its own style",
        names.worker(model)
    )
}

/// Whether a step that checks or picks `task`'s work is, with no other maker set up, taken by
/// a fresh session of its worker's own model: a step of the workflow it's done by, or the
/// run's review, which checks every task of a council's plan whatever its way.
pub fn checked_by_its_own_model(host: &Host, task: &TaskSpec) -> bool {
    let Some(worker) = &task.worker else {
        return false;
    };
    let settings = host.settings();
    let provider = |model: &ModelRef| roles::provider(host, model);
    let set_up = set_up(settings, &host.services().workers.available());
    let seats = seats_of(host.names(), settings);
    let reviewers = roles::candidates(settings, CrewRole::Reviewer.fine_role());
    let councils_plan = host.read(|state| state.councils.newest().is_some());
    if Setup::of(settings) == Setup::Council
        && councils_plan
        && ladder(worker, &seats, &reviewers, &set_up, &provider).same_maker
    {
        return true;
    }
    let Some(Way::Workflow { name }) = task.way.as_ref().map(|way| &way.how) else {
        return false;
    };
    let Ok(steps) = super::steps_of(host, name.as_str()) else {
        return false;
    };
    let maker = provider(worker);
    steps.iter().any(|step| match step {
        TaskStep::Pick {
            role,
            by: PickedBy::Vote,
            ..
        } => voters(host, *role)
            .iter()
            .all(|voter| provider(voter) == maker),
        TaskStep::Check { role, .. } | TaskStep::Pick { role, .. } => {
            let pool = roles::candidates(settings, role.fine_role());
            ladder(worker, &seats, &pool, &set_up, &provider).same_maker
        }
        _ => false,
    })
}

/// Who votes on the tries a task keeps: one model of `role` from each maker set up, those
/// ready to run first.
pub(crate) fn voters(host: &Host, role: CrewRole) -> Vec<ModelRef> {
    let mut pool = roles::candidates(host.settings(), role.fine_role());
    let available = host.services().workers.available();
    if pool.iter().any(|model| available.contains(model)) {
        pool.retain(|model| available.contains(model));
    }
    one_per_maker(&pool, &|model| roles::provider(host, model))
}

/// The first model of each maker in `models`, in their order.
pub fn one_per_maker(models: &[ModelRef], provider: &dyn Fn(&ModelRef) -> String) -> Vec<ModelRef> {
    let mut makers: Vec<String> = Vec::new();
    let mut each = Vec::new();
    for model in models {
        let maker = provider(model);
        if !makers.contains(&maker) {
            makers.push(maker);
            each.push(model.clone());
        }
    }
    each
}

/// Whether a check of the worker's own model asks you first: a council's plan with
/// `council.same_model_check = "ask"`. The plan's approval asks once, for every such check.
pub fn asks_first(settings: &Settings) -> bool {
    Setup::of(settings) == Setup::Council
        && settings.council.same_model_check == SameModelCheck::Ask
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::base::names::SeatId;
    use argonauts_core::runs::events::council::SeatRole;

    fn model(text: &str) -> ModelRef {
        text.parse().unwrap()
    }

    fn maker(model: &ModelRef) -> String {
        match model.to_string().split_once('/').map(|(cli, _)| cli) {
            Some("claude-code") => "anthropic".to_owned(),
            Some("codex") => "openai".to_owned(),
            Some(cli) => cli.to_owned(),
            None => String::new(),
        }
    }

    #[test]
    fn a_checker_of_the_role_from_another_maker_comes_first() {
        let opus = model("claude-code/opus");
        let luna = model("codex/luna");
        let flash = model("antigravity-cli/flash");
        let picked = ladder(&opus, &[], &[opus.clone(), flash.clone()], &[luna], &maker);
        assert_eq!(
            picked,
            Checker {
                model: flash,
                same_maker: false
            }
        );
    }

    #[test]
    fn a_councils_checker_seat_from_another_maker_checks_before_the_role() {
        let opus = model("claude-code/opus");
        let luna = model("codex/luna");
        let flash = model("antigravity-cli/flash");
        let seat = |n: u32, model: &ModelRef, role: SeatRole| Seat {
            id: SeatId::numbered(n),
            model: model.clone(),
            role,
            number: None,
        };
        let seats = [
            seat(1, &opus, SeatRole::Checker),
            seat(2, &flash, SeatRole::Proposer),
            seat(3, &luna, SeatRole::Checker),
        ];
        let picked = ladder(&opus, &seats, std::slice::from_ref(&flash), &[], &maker);
        assert_eq!(
            picked,
            Checker {
                model: luna,
                same_maker: false
            }
        );
    }

    #[test]
    fn any_model_set_up_from_another_maker_checks_before_the_same_one() {
        let opus = model("claude-code/opus");
        let qwen = model("opencode/qwen");
        let pool = std::slice::from_ref(&opus);
        let picked = ladder(&opus, &[], pool, &[opus.clone(), qwen.clone()], &maker);
        assert_eq!(picked.model, qwen);
        assert!(!picked.same_maker);
    }

    #[test]
    fn with_one_maker_its_own_model_checks_in_a_fresh_session_never_a_sibling() {
        let opus = model("claude-code/opus");
        let sonnet = model("claude-code/sonnet");
        let picked = ladder(
            &opus,
            &[],
            &[sonnet.clone(), opus.clone()],
            &[sonnet],
            &maker,
        );
        assert_eq!(
            picked,
            Checker {
                model: opus.clone(),
                same_maker: true
            }
        );
        let alone = ladder(&opus, &[], std::slice::from_ref(&opus), &[], &maker);
        assert!(alone.same_maker && alone.model == opus);
    }
}
