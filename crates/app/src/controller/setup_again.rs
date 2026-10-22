//! `/setup` when you already have settings asks first whether to go through every question
//! again, so a slip in the `/` menu never starts the guided setup over your settings. Either way
//! it ends in one `/setup` row: the setup's own, or that your settings stayed.

use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::names::ChoiceId;

use super::{Ctx, Effect};
use crate::choices::{self, InitAnswer, choose};

const YES: &str = "yes";

#[derive(Default)]
pub(crate) struct SetupAgainState {
    pending: Option<ChoiceId>,
}

pub(crate) fn owns_choice(state: &SetupAgainState, id: &ChoiceId) -> bool {
    state.pending.as_ref() == Some(id)
}

/// The guided setup at once when you have no settings yet, and otherwise the question.
pub(crate) fn ask(ctx: &mut Ctx, state: &mut SetupAgainState) -> Vec<Effect> {
    let global = ctx.env.global_config();
    if !global.exists() {
        return vec![Effect::OpenSetup];
    }
    let answer = |value: &str, label: &str, detail: &str| InitAnswer {
        label: label.to_owned(),
        value: value.to_owned(),
        detail: Some(detail.to_owned()),
    };
    let options = choices::init_options(&[
        answer(
            YES,
            "Yes, go through them",
            "Each one shows what your settings say now.",
        ),
        answer(
            "no",
            "No, keep my settings",
            "/config changes one at a time.",
        ),
    ]);
    let id = ctx.next_choice_id();
    ctx.send(AppEvent::Choice {
        choice: choices::app_choice(
            id.clone(),
            ChoiceKind::Init,
            "Go through every setup question again?",
            vec![format!(
                "Your settings are in {}.",
                ctx.env.display(&global)
            )],
            options,
        ),
    });
    state.pending = Some(id);
    Vec::new()
}

pub(crate) fn answer_choice(
    ctx: &Ctx,
    state: &mut SetupAgainState,
    id: ChoiceId,
    action: &AnswerAction,
) -> Vec<Effect> {
    state.pending = None;
    if *action == choose(YES) {
        // The setup's questions follow, and its one row says what they left.
        close(ctx, id, "");
        vec![Effect::OpenSetup]
    } else {
        close(ctx, id, &crate::setup::setup_row(KEPT));
        Vec::new()
    }
}

/// Esc keeps your settings.
pub(crate) fn set_aside(ctx: &Ctx, state: &mut SetupAgainState, id: ChoiceId) {
    state.pending = None;
    close(ctx, id, &crate::setup::setup_row(KEPT));
}

const KEPT: &str = "Kept your settings";

fn close(ctx: &Ctx, id: ChoiceId, outcome: &str) {
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: outcome.to_owned(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::test_support::{CliOptions, TestCtx};

    fn asked(parts: &mut TestCtx) -> Option<argonauts_core::app::AppChoice> {
        while let Ok(event) = parts.events.try_recv() {
            if let AppEvent::Choice { choice } = event {
                return Some(choice);
            }
        }
        None
    }

    #[tokio::test]
    async fn with_settings_it_asks_before_going_through_them_again() {
        let (mut parts, _) = TestCtx::new(CliOptions::default());
        let mut state = SetupAgainState::default();
        assert!(ask(&mut parts.ctx, &mut state).is_empty());
        let choice = asked(&mut parts).expect("the question");
        assert_eq!(choice.title, "Go through every setup question again?");
        let effects = answer_choice(&parts.ctx, &mut state, choice.id.clone(), &choose(YES));
        assert!(matches!(effects[..], [Effect::OpenSetup]));
        assert_eq!(closed(&mut parts).as_deref(), Some(""));

        assert!(ask(&mut parts.ctx, &mut state).is_empty());
        let choice = asked(&mut parts).expect("the question");
        set_aside(&parts.ctx, &mut state, choice.id.clone());
        assert!(!owns_choice(&state, &choice.id));
        assert_eq!(
            closed(&mut parts).as_deref(),
            Some("/setup ⎿  Kept your settings")
        );
    }

    fn closed(parts: &mut TestCtx) -> Option<String> {
        while let Ok(event) = parts.events.try_recv() {
            if let AppEvent::ChoiceClosed { outcome, .. } = event {
                return Some(outcome);
            }
        }
        None
    }

    #[tokio::test]
    async fn without_settings_the_setup_opens_at_once() {
        let (mut parts, _) = TestCtx::new(CliOptions::default());
        std::fs::remove_file(parts.ctx.env.global_config()).unwrap();
        let mut state = SetupAgainState::default();
        let effects = ask(&mut parts.ctx, &mut state);
        assert!(matches!(effects[..], [Effect::OpenSetup]));
    }
}
