//! Every session's label, such as `fix:slugify` or `read:anthropic`: from its step's pattern, or
//! the kind's default when the main agent left it out, filled with the task's item, the provider,
//! and the round. A task's item is the name its plan gave it (the planner's key, or the main
//! agent's), else its title's first words, and never its id. A task's later sessions (a fallback, a
//! retry, a resume) keep its label, so a row never changes name; anyone else who'd share one gets
//! `-2`.

use argonauts_core::TaskId;
use argonauts_core::labels::{self, LabelParts};
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::steps::StepName;
use argonauts_core::workflow::{StepKind, StepRef};

use crate::merge::merger;
use crate::state::RunState;

/// Who a new session is, as far as its label goes.
pub struct Asking<'a> {
    pub key: &'a SessionKey,
    pub step: Option<&'a StepRef>,
    /// 1 the first time.
    pub round: u32,
    pub role: RoleName,
    pub step_name: Option<StepName>,
    pub task: Option<&'a TaskId>,
    /// The task's item when no task record holds it yet, as for a prediction made while the
    /// plan is divided.
    pub item: Option<&'a str>,
    pub worker: &'a ModelRef,
    pub provider: &'a str,
}

/// What a task goes by where it asks you something: its worker's label, such as `build:flag`,
/// else `build:` and its item. Its id shows only for a task with neither.
pub fn task_label(state: &RunState, task: &TaskId) -> String {
    let worker = state
        .sessions
        .in_order()
        .into_iter()
        .filter(|session| session.task.as_ref() == Some(task))
        .find(|session| session.started.step == Some(StepName::Build))
        .and_then(|session| session.started.label.clone());
    worker
        .or_else(|| {
            let record = state.tasks.get(task)?;
            let item = record
                .spec
                .item
                .clone()
                .filter(|item| !item.is_empty())
                .unwrap_or_else(|| labels::item_from_title(&record.spec.title, "build"));
            (!item.is_empty()).then(|| format!("build:{item}"))
        })
        .unwrap_or_else(|| task.to_string())
}

/// The session's label, unique in the run.
pub fn session_label(state: &RunState, asking: &Asking<'_>) -> String {
    let round = round_of(state, asking);
    if let Some(kept) = earlier_label(state, asking, round) {
        return kept;
    }
    if let Some(named) = named_by_its_call(state, asking) {
        return labels::unique(named, |candidate| taken(state, asking.key, candidate));
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
        .task
        .and_then(|task| state.tasks.get(task))
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
        provider: Some(asking.provider),
        n: Some(n),
        round,
        try_number: state
            .goal
            .pursuit
            .as_ref()
            .and_then(|goal| goal.tries.last())
            .map(|a_try| a_try.n),
        branch: branch.as_deref(),
    };
    let label = labels::fill(&pattern, &parts);
    labels::unique(&label, |candidate| taken(state, asking.key, candidate))
}

/// The label a script's call gave the task `asking` works on, which the task's worker goes by,
/// so the task has one name wherever it shows. A script titles its task with the call's
/// label, where a planned task's title is a sentence, which is no label.
fn named_by_its_call<'s>(state: &'s RunState, asking: &Asking<'_>) -> Option<&'s str> {
    if asking.role != RoleName::Worker || asking.step_name != Some(StepName::Build) {
        return None;
    }
    let title = state.tasks.get(asking.task?)?.spec.title.as_str();
    labels::problem(title).is_none().then_some(title)
}

/// A label a session was given outright. It keeps the one it has when it resumes, and takes
/// `-2` when another session already has the name.
pub fn given_label(state: &RunState, key: &SessionKey, given: &str) -> String {
    if let Some(kept) = state
        .sessions
        .get(key)
        .and_then(|own| own.started.label.clone())
    {
        return kept;
    }
    labels::unique(given, |candidate| taken(state, key, candidate))
}

/// The pattern for this session: the step's own for the agent the step is about, else what its
/// role does on a task (a reviewer reviews, the judge settles), else the step's default.
fn pattern(
    asking: &Asking<'_>,
    step: Option<(StepKind, Option<RoleName>, Option<&str>)>,
) -> String {
    // Work a step's own agents do beside it: a task's tests, review, and settling, and the
    // main agent's and code's own asks.
    let beside = match (asking.step_name, asking.role) {
        (Some(StepName::DesignWorkflow), _) => Some("main:design"),
        (Some(StepName::PlanTry), _) => Some("main:try"),
        (Some(StepName::AcceptanceTests), _) => Some("accept:{item}"),
        (Some(StepName::Predict), _) => Some("predict:{item}"),
        // Named for the job, as every session of the merger's is.
        (Some(StepName::ResolveConflict), _) => Some(merger::SETTLE_CONFLICT),
        (Some(StepName::FallbackSummary), _) => Some("fallback:{item}"),
        (Some(StepName::PickTry), _) => Some("pick:{item}"),
        (Some(StepName::WriteInstructions), _) => Some("write:{item}"),
        (_, RoleName::Reviewer) if asking.task.is_some() => Some("review:{item}"),
        (Some(StepName::Settle), _) if asking.task.is_some() => Some("settle:{item}"),
        (_, RoleName::Experiment) => Some("experiment:{item}"),
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
        None if asking.task.is_some() => format!("{}:{{item}}", asking.role.as_str()),
        None => asking.role.as_str().to_owned(),
    }
}

/// A task keeps its label through fallbacks and retries, and any session keeps its own when it
/// resumes. Sessions without a task are told apart by their worker too, so two readers in one
/// step get two labels.
fn earlier_label(state: &RunState, asking: &Asking<'_>, round: u32) -> Option<String> {
    if let Some(own) = state.sessions.get(asking.key) {
        return own.started.label.clone();
    }
    // A review after changes is a new row, `review:median:r2`, never the first review's.
    let same_round = |label: &str| asking.role != RoleName::Reviewer || round_in(label) == round;
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
                && session.task.as_ref() == asking.task
                && asking.task.is_some();
            same.then(|| started.label.clone())
                .flatten()
                .filter(|label| same_round(label))
        })
}

/// The round a session's label says: a task's review counts the rounds that asked for changes,
/// and everything else is in its step's round.
fn round_of(state: &RunState, asking: &Asking<'_>) -> u32 {
    match (asking.role, asking.task) {
        (RoleName::Reviewer, Some(task)) => {
            (state.tasks.progress(task).change_rounds + 1).max(asking.round)
        }
        _ => asking.round,
    }
}

/// The round a label ends with, 1 when it names none.
fn round_in(label: &str) -> u32 {
    label
        .rsplit_once(":r")
        .and_then(|(_, round)| round.parse().ok())
        .unwrap_or(1)
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
    use argonauts_core::events::task::TaskEvent;

    use crate::state::tests as testing;

    fn worker(name: &str) -> ModelRef {
        ModelRef::new(name, None)
    }

    #[test]
    fn readers_take_their_provider_and_repeats_get_a_number() {
        let mut state = RunState::default();
        let key = SessionKey::numbered(1);
        let claude = worker("claude-code");
        let asking = Asking {
            key: &key,
            step: None,
            round: 1,
            role: RoleName::Reader,
            step_name: Some(StepName::Read),
            task: None,
            item: None,
            worker: &claude,
            provider: "anthropic",
        };
        assert_eq!(session_label(&state, &asking), "reader");

        let mut started = testing::session_started(&claude, RoleName::Reader);
        started.label = Some("reader".to_owned());
        state.apply(&testing::envelope(
            1,
            None,
            Some(key.as_str()),
            RunEventKind::Session(SessionEvent::Started(Box::new(started))),
        ));
        // Its own resume keeps the label.
        assert_eq!(session_label(&state, &asking), "reader");
        // Another session that would share it doesn't.
        let other = SessionKey::numbered(2);
        let codex = worker("codex");
        let second = Asking {
            key: &other,
            worker: &codex,
            provider: "openai",
            ..asking
        };
        assert_eq!(session_label(&state, &second), "reader-2");
    }

    /// A task a script's call made goes by the call's label, so its worker's row and the merge
    /// question name it alike; a planned task's worker is named from its item.
    #[test]
    fn a_scripts_task_goes_by_its_calls_label() {
        let mut state = RunState::default();
        let mut scripted = crate::testutil::task_spec("task-001", &[], Some("codex"));
        scripted.title = "build:rate-limit".to_owned();
        scripted.item = Some("per-api-key".to_owned());
        let mut planned = crate::testutil::task_spec("task-002", &[], Some("codex"));
        planned.title = "Limit each API key's requests".to_owned();
        planned.item = Some("per-api-key".to_owned());
        for (seq, spec) in [(1, scripted), (2, planned)] {
            let task = spec.id.to_string();
            state.apply(&testing::envelope(
                seq,
                Some(task.as_str()),
                None,
                RunEventKind::Task(TaskEvent::Created(Box::new(spec))),
            ));
        }
        let codex = worker("codex");
        let (one, two) = (SessionKey::numbered(1), SessionKey::numbered(2));
        let (first, second) = (TaskId::numbered(1), TaskId::numbered(2));
        let asking = |key, task| Asking {
            key,
            step: None,
            round: 1,
            role: RoleName::Worker,
            step_name: Some(StepName::Build),
            task: Some(task),
            item: None,
            worker: &codex,
            provider: "openai",
        };
        assert_eq!(
            session_label(&state, &asking(&one, &first)),
            "build:rate-limit"
        );
        assert_eq!(
            session_label(&state, &asking(&two, &second)),
            "worker:per-api-key"
        );
    }
}
