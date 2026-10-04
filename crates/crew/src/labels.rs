//! Every session's label, such as `fix:slugify` or `read:claude`: from its step's pattern, or
//! the kind's default when the lead left it out, filled with the card's item, the family, and
//! the round. A card's item is the name its plan gave it (the planner's key, or the lead's),
//! else its title's first words, and never its id. A card's later sessions (a handoff, a retry,
//! a resume) keep its label, so a row never changes name; anyone else who'd share one gets
//! `-2`.

use argonauts_core::CardId;
use argonauts_core::labels::{self, LabelParts};
use argonauts_core::names::{RoleName, SessionKey, WorkerRef};
use argonauts_core::steps::StepName;
use argonauts_core::workflow::{StepKind, StepRef};

use crate::state::RunState;

/// Who a new session is, as far as its label goes.
pub struct Asking<'a> {
    pub key: &'a SessionKey,
    pub step: Option<&'a StepRef>,
    /// 1 the first time.
    pub round: u32,
    pub role: RoleName,
    pub step_name: Option<StepName>,
    pub card: Option<&'a CardId>,
    /// The card's item when no card record holds it yet, as for a prediction made while the
    /// plan is divided.
    pub item: Option<&'a str>,
    pub worker: &'a WorkerRef,
    pub family: &'a str,
}

/// The session's label, unique in the run.
pub fn session_label(state: &RunState, asking: &Asking<'_>) -> String {
    if let Some(kept) = earlier_label(state, asking) {
        return kept;
    }
    let phase = asking.step.and_then(|step| {
        let record = state.workflows.by_id.get(&step.workflow)?;
        Some((
            record.phase_definition(step.phase)?.name.clone(),
            record.step_definition(step).cloned(),
        ))
    });
    let (phase, step) = match phase {
        Some((phase, step)) => (phase, step),
        None => (String::new(), None),
    };
    let item = asking
        .card
        .and_then(|card| state.cards.get(card))
        .and_then(|record| {
            record
                .spec
                .item
                .clone()
                .or_else(|| Some(labels::item_from_title(&record.spec.title, &phase)))
        })
        .or_else(|| asking.item.map(str::to_owned))
        .filter(|item| !item.is_empty());
    let pattern = pattern(
        asking,
        step.as_ref()
            .map(|step| (step.kind, step.role, step.label.as_deref())),
    );
    let n = siblings(state, asking) + 1;
    let branch = state
        .run
        .started
        .as_ref()
        .and_then(|started| started.base_branch.clone());
    let parts = LabelParts {
        phase: if phase.is_empty() {
            asking.role.as_str()
        } else {
            &phase
        },
        item: item.as_deref(),
        family: Some(asking.family),
        n: Some(n),
        round: asking.round,
        attempt: state
            .goal
            .pursuit
            .as_ref()
            .and_then(|goal| goal.attempts.last())
            .map(|attempt| attempt.n),
        branch: branch.as_deref(),
    };
    let label = labels::fill(&pattern, &parts);
    labels::unique(&label, |candidate| taken(state, asking.key, candidate))
}

/// The pattern for this session: the step's own for the agent the step is about, else what its
/// role does on a card (a reviewer reviews, an adjudicator settles), else the step's default.
fn pattern(
    asking: &Asking<'_>,
    step: Option<(StepKind, Option<RoleName>, Option<&str>)>,
) -> String {
    // Work a step's own agents do beside it: a card's tests, review, and settling, and the
    // lead's and code's own asks.
    let beside = match (asking.step_name, asking.role) {
        (Some(StepName::DesignWorkflow), _) => Some("lead:design"),
        (Some(StepName::PlanAttempt), _) => Some("lead:attempt"),
        (Some(StepName::AcceptanceTests), _) => Some("accept:{item}"),
        (Some(StepName::Predict), _) => Some("predict:{item}"),
        (Some(StepName::ResolveConflict), _) => Some("resolve:{item}"),
        (Some(StepName::HandoffSummary), _) => Some("handoff:{item}"),
        (_, RoleName::Reviewer) if asking.card.is_some() => Some("review:{item}"),
        (_, RoleName::Adjudicator) if asking.card.is_some() => Some("settle:{item}"),
        (_, RoleName::Spike) => Some("spike:{item}"),
        _ => None,
    };
    if let Some(pattern) = beside {
        return pattern.to_owned();
    }
    match step {
        Some((_, _, Some(pattern))) => pattern.to_owned(),
        Some((kind, role, None)) => {
            labels::default_pattern(kind, role.or(Some(asking.role))).to_owned()
        }
        None if asking.card.is_some() => format!("{}:{{item}}", asking.role.as_str()),
        None => asking.role.as_str().to_owned(),
    }
}

/// A card keeps its label through handoffs and retries, and any session keeps its own when it
/// resumes. Sessions without a card are told apart by their worker too, so two readers in one
/// step get two labels.
fn earlier_label(state: &RunState, asking: &Asking<'_>) -> Option<String> {
    if let Some(own) = state.sessions.get(asking.key) {
        return own.started.label.clone();
    }
    state
        .sessions
        .in_order()
        .into_iter()
        .rev()
        .find_map(|session| {
            let started = &session.started;
            let same = started.workflow_step.as_ref() == asking.step
                && started.role == asking.role
                && started.step == asking.step_name
                && session.card.as_ref() == asking.card
                && asking.card.is_some();
            same.then(|| started.label.clone()).flatten()
        })
}

/// Earlier agents of the same step, round, and role, for `{n}`.
fn siblings(state: &RunState, asking: &Asking<'_>) -> u32 {
    let count = state
        .sessions
        .in_order()
        .into_iter()
        .filter(|session| {
            let started = &session.started;
            &session.key != asking.key
                && started.workflow_step.as_ref() == asking.step
                && started.role == asking.role
                && started.step == asking.step_name
                && started.worker != *asking.worker
                && started.label.is_some()
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn taken(state: &RunState, key: &SessionKey, label: &str) -> bool {
    state
        .sessions
        .by_key
        .values()
        .any(|session| &session.key != key && session.started.label.as_deref() == Some(label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::RunEventKind;
    use argonauts_core::events::session::SessionEvent;

    use crate::state::tests as testing;

    fn worker(name: &str) -> WorkerRef {
        WorkerRef::new(name, None)
    }

    #[test]
    fn readers_take_their_family_and_repeats_get_a_number() {
        let mut state = RunState::default();
        let key = SessionKey::numbered(1);
        let claude = worker("claude-code");
        let asking = Asking {
            key: &key,
            step: None,
            round: 1,
            role: RoleName::Restater,
            step_name: Some(StepName::Restate),
            card: None,
            item: None,
            worker: &claude,
            family: "claude",
        };
        assert_eq!(session_label(&state, &asking), "restater");

        let mut started = testing::session_started(&claude, RoleName::Restater);
        started.label = Some("restater".to_owned());
        state.apply(&testing::envelope(
            1,
            None,
            Some(key.as_str()),
            RunEventKind::Session(SessionEvent::Started(Box::new(started))),
        ));
        // Its own resume keeps the label.
        assert_eq!(session_label(&state, &asking), "restater");
        // Another session that would share it doesn't.
        let other = SessionKey::numbered(2);
        let codex = worker("codex");
        let second = Asking {
            key: &other,
            worker: &codex,
            family: "gpt",
            ..asking
        };
        assert_eq!(session_label(&state, &second), "restater-2");
    }
}
