//! Several tries at one task, as a workflow's step that starts more than one worker asks:
//! the task's own session makes the first try in its worktree, and each other try is a task
//! of its own in a worktree from the same base, by a fresh session of the same model, or for
//! drafts, of a worker of another maker. None sees the others. The step after them picks the
//! try whose work the task keeps, by one checker or by a vote; the others stay on their
//! branches, unmerged.

use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::Party;
use argonauts_core::events::session::InputPartKind;
use argonauts_core::events::task::{TaskEvent, TaskSpec, TaskState};
use argonauts_core::names::ModelRef;
use argonauts_core::session_runner::Access;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::steps::fallback::{PickTry, PickTryInput};
use argonauts_core::{Note, NoteLevel, TaskId};
use argonauts_runner::Count;

use super::checker::{self, Checker};
use super::steps::{PickedBy, TriedBy};
use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::merge::branch::task_branch;
use crate::prompts;
use crate::roles::{self, display_name};
use crate::task;
use crate::task::checks::{self, CheckVerdict};
use crate::task::pipeline::{self, Until};
use crate::task::support::{part, record_of, record_task, state_of};
use crate::task::worktree::{self, TaskTree};
use crate::workflow::StepAt;

/// The most of one try's change the picker reads. A longer one is cut, and says so.
const DIFF_CHARS: usize = 60_000;

/// How many tries `count` makes: as many as it says, or one per worker set up.
fn how_many(workers: &[ModelRef], count: Count) -> usize {
    match count {
        Count::Fixed(n) => usize::try_from(n).unwrap_or(usize::MAX),
        Count::All => workers.len(),
    }
    .max(1)
}

/// Who makes each of the tries `count` asks for, the task's `worker` first. Tries of its model
/// are as many as `count` says, or one per worker set up. Drafts are one by a worker of each
/// maker set up, its own maker first, or as many as `count` says, taking the makers in turn;
/// with one maker set up, two fresh sessions of its model draft.
pub(super) fn makers_of_tries(
    worker: &ModelRef,
    workers: &[ModelRef],
    provider: &dyn Fn(&ModelRef) -> String,
    count: Count,
    by: TriedBy,
) -> Vec<ModelRef> {
    match by {
        TriedBy::SameModel => vec![worker.clone(); how_many(workers, count)],
        TriedBy::EachMaker => {
            let models: Vec<ModelRef> = std::iter::once(worker).chain(workers).cloned().collect();
            let each = checker::one_per_maker(&models, provider);
            let n = match count {
                Count::Fixed(_) => how_many(workers, count),
                Count::All => each.len().max(2),
            };
            each.iter().cycle().take(n).cloned().collect()
        }
    }
}

/// The workers set up that are ready to run, or every worker set up when none is.
fn workers_set_up(host: &Host) -> Vec<ModelRef> {
    let mut workers = roles::candidates(host.settings(), CrewRole::Worker.fine_role());
    let available = host.services().workers.available();
    if workers.iter().any(|model| available.contains(model)) {
        workers.retain(|model| available.contains(model));
    }
    workers
}

/// Makes the tries `count` asks for at `task` side by side and waits until each has passed its
/// checks or stopped. Returns where the task's own try stopped: the work goes on only once it
/// passes, so a try that stopped for a decision waits for it, and the tries that passed wait
/// with it.
pub(super) async fn make(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    count: Count,
    by: TriedBy,
) -> EngineResult<TaskState> {
    let record = record_of(host, task)?;
    let spec = record.spec;
    let own = worktree::ensure(host, task).await?;
    let workers = workers_set_up(host);
    // With no worker yet, each try's worker is picked as the task's would be.
    let makers: Vec<Option<ModelRef>> = match record.worker.or_else(|| spec.worker.clone()) {
        Some(worker) => {
            let provider = |model: &ModelRef| roles::provider(host, model);
            makers_of_tries(&worker, &workers, &provider, count, by)
                .into_iter()
                .map(Some)
                .collect()
        }
        None => vec![None; how_many(&workers, count)],
    };
    let mut others = Vec::new();
    for (n, maker) in makers.iter().enumerate().skip(1).map(|(at, m)| (at + 1, m)) {
        let id = try_id(task, n)?;
        if host.read(|state| state.tasks.get(&id).is_none()) {
            task::create(host, try_spec(&spec, &id, n, maker.as_ref()))?;
            // From the task's own base, so the try it keeps holds that try's work alone.
            let tree = TaskEvent::Worktree {
                path: host.repo().worktrees.join(id.as_str()),
                branch: task_branch(host.run_id(), &id),
                base: own.base.clone(),
            };
            record_task(host, &id, tree)?;
        }
        others.push(id);
    }
    let mut runs = vec![pipeline::drive(host, task, at, Until::ChecksPassed)];
    runs.extend(
        others
            .iter()
            .map(|id| pipeline::drive(host, id, at, Until::ChecksPassed)),
    );
    let ends = futures::future::join_all(runs).await;
    let mut states = Vec::with_capacity(ends.len());
    for end in ends {
        states.push(end?);
    }
    let own_state = states[0];
    let waiting: Vec<String> = states
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, state)| **state == TaskState::InReview)
        .map(|(at, _)| (at + 1).to_string())
        .collect();
    if own_state != TaskState::InReview && !waiting.is_empty() {
        let tries = match waiting.as_slice() {
            [one] => format!("try {one} passed its checks and waits"),
            many => format!("tries {} passed their checks and wait", many.join(", ")),
        };
        let state = format!("{own_state:?}").to_lowercase();
        let words = argonauts_core::text::state_words(&state);
        let text = format!(
            "{} · its own try is {words}, so {tries} until it goes on",
            spec.title
        );
        note(host, task, NoteLevel::Warn, text)?;
    }
    Ok(own_state)
}

/// The `step` after the tries picks the one whose work `task` keeps, `by` one checker of
/// `role` or a vote, reading every try that passed its checks, numbered and unnamed. When it
/// isn't the task's own, the task's branch moves to it and its checks run again there. `Err`
/// says why no try could be kept.
pub(super) async fn keep_one(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    step: &str,
    role: CrewRole,
    by: PickedBy,
) -> EngineResult<Result<(), String>> {
    let tries = tries_of(host, task);
    let passed: Vec<(usize, TaskId)> = tries
        .iter()
        .filter(|(_, id)| state_of(host, id).ok() == Some(TaskState::InReview))
        .cloned()
        .collect();
    let title = record_of(host, task)?.spec.title;
    let kept = match passed.as_slice() {
        [] => {
            return Ok(Err(format!(
                "none of its {} tries passed its checks",
                tries.len()
            )));
        }
        [(n, id)] => {
            if tries.len() > 1 {
                let text = format!(
                    "{title} · only try {n} of {} passed its checks, so it is kept",
                    tries.len()
                );
                note(host, task, NoteLevel::Info, text)?;
            }
            (*n, id.clone())
        }
        _ => match by {
            PickedBy::Checker => {
                let Some(checker) = checker::of_task(host, task, role)? else {
                    return Ok(Err(format!("it has no worker yet to take its {step} step")));
                };
                let kept = Box::pin(picked(host, task, at, role, &checker, &passed, &title));
                kept.await?
            }
            PickedBy::Vote => {
                let voters = checker::voters(host, role);
                Box::pin(voted(host, task, at, role, &voters, &passed, &title)).await?
            }
        },
    };
    if kept.1 == *task {
        return Ok(Ok(()));
    }
    Box::pin(take_in(host, task, &kept.1)).await
}

/// The try `checker` picks among those that `passed`, or the first of them, with a note
/// saying why, when it couldn't be asked or named none.
async fn picked(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    role: CrewRole,
    checker: &Checker,
    passed: &[(usize, TaskId)],
    title: &str,
) -> EngineResult<(usize, TaskId)> {
    let first = passed[0].clone();
    let by = display_name(&checker.model);
    let (chosen, why) = match pick_of(host, task, at, role, &checker.model, passed).await? {
        Ok(pick) => pick,
        Err(why) => {
            first_stands(host, task, title, first.0, &format!("{by} {why}"))?;
            return Ok(first);
        }
    };
    let chosen = passed[chosen].clone();
    let text = format!(
        "{title} · {by} kept try {} of the {} that passed its checks · {why}",
        chosen.0,
        passed.len(),
    );
    note(host, task, NoteLevel::Info, text)?;
    if checker.same_maker {
        note(
            host,
            task,
            NoteLevel::Info,
            checker::same_maker_line(&checker.model),
        )?;
    }
    Ok(chosen)
}

/// The try that most of `voters` pick among those that `passed`, each voter blind to the
/// others and to who made each try. A tie keeps the earliest of the tied tries; with no vote
/// counted, the first that passed is kept.
async fn voted(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    role: CrewRole,
    voters: &[ModelRef],
    passed: &[(usize, TaskId)],
    title: &str,
) -> EngineResult<(usize, TaskId)> {
    let asks = voters
        .iter()
        .map(|voter| Box::pin(pick_of(host, task, at, role, voter, passed)));
    let answers = futures::future::join_all(asks).await;
    let mut votes = vec![0usize; passed.len()];
    let mut whys: Vec<Option<String>> = vec![None; passed.len()];
    for answer in answers {
        if let Ok((chosen, why)) = answer? {
            votes[chosen] += 1;
            whys[chosen].get_or_insert(why);
        }
    }
    let cast: usize = votes.iter().sum();
    let most = votes.iter().copied().max().unwrap_or(0);
    let Some(won) = votes
        .iter()
        .position(|count| *count == most)
        .filter(|_| cast > 0)
    else {
        first_stands(host, task, title, passed[0].0, "no vote could be counted")?;
        return Ok(passed[0].clone());
    };
    let tied = votes.iter().filter(|count| **count == most).count() > 1;
    let mut text = format!(
        "{title} · the vote kept try {} of the {} that passed its checks, with {most} of {cast} \
         votes",
        passed[won].0,
        passed.len(),
    );
    if tied {
        text.push_str(", the earliest of the tied tries");
    }
    if let Some(why) = &whys[won] {
        text.push_str(&format!(" · {why}"));
    }
    note(host, task, NoteLevel::Info, text)?;
    Ok(passed[won].clone())
}

/// Asks `by` which of the tries that `passed` the task keeps. `Ok` holds where the pick sits
/// in `passed` and why; `Err` why there is none.
async fn pick_of(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    role: CrewRole,
    by: &ModelRef,
    passed: &[(usize, TaskId)],
) -> EngineResult<Result<(usize, String), String>> {
    let record = record_of(host, task)?;
    let input = PickTryInput {
        task_id: task.to_string(),
        title: record.spec.title.clone(),
        task: record.spec.task.clone(),
        tries: u32::try_from(passed.len()).unwrap_or(u32::MAX),
    };
    let mut ask = Ask::new(
        Some(at.clone()),
        role.fine_role(),
        by.clone(),
        prompts::fallback::PICK,
        input,
    );
    ask.task = Some(task.clone());
    ask.access = Access::NoTools;
    ask.blind = true;
    ask.independent_of = record.worker.or(record.spec.worker).into_iter().collect();
    for (number, (_, id)) in passed.iter().enumerate() {
        let tree = worktree::ensure(host, id).await?;
        let label = format!("try {}", number + 1);
        ask.parts.push(part(
            InputPartKind::Evidence,
            label,
            diff_of(host, &tree).await,
            None,
        ));
    }
    let answer = match host.ask::<PickTry>(ask).await {
        Ok(answered) => answered.output,
        Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
        Err(err) => return Ok(Err(format!("couldn't answer ({err})"))),
    };
    let chosen = usize::try_from(answer.pick)
        .ok()
        .and_then(|number| number.checked_sub(1))
        .filter(|at| *at < passed.len());
    Ok(match chosen {
        Some(chosen) => Ok((chosen, answer.why.trim().to_owned())),
        None => Err("named none of them".to_owned()),
    })
}

/// Moves the task's branch to the try it keeps, which started from the same base, takes that
/// try's report as the task's newest, gives the task to the try's worker when another model
/// made it, and runs the checks there. A branch that already holds that try, as after a
/// resume, stays as it is.
async fn take_in(host: &Host, task: &TaskId, kept: &TaskId) -> EngineResult<Result<(), String>> {
    let own = worktree::ensure(host, task).await?;
    let theirs = worktree::ensure(host, kept).await?;
    let git = worktree::tree_git(host, &own.path).await?;
    let Some(head) = git.branch_head(&host.repo().root, &theirs.branch).await? else {
        return Ok(Err(format!(
            "the try it kept has no branch {}",
            theirs.branch
        )));
    };
    let drafted = record_of(host, kept)?;
    // A draft of another maker's makes the work that maker's, so no check of the task comes
    // from it and work sent back goes to it. Given first, so a resume finds it given.
    let ours = record_of(host, task)?;
    let ours = ours.worker.or(ours.spec.worker);
    if let Some(author) = drafted.worker.clone().or(drafted.spec.worker.clone())
        && ours.as_ref() != Some(&author)
    {
        let moved = TaskEvent::Reassigned {
            from: ours,
            to: author,
            by: Party::ArgonautsCode,
            rule_override: None,
        };
        record_task(host, task, moved)?;
    }
    let own_head = git.resolve(&own.path, "HEAD").await?;
    if git.is_ancestor(&own.path, &head, &own_head).await? {
        return Ok(Ok(()));
    }
    git.reset_hard(&own.path, &head).await?;
    worktree::save_diff(host, task, &git, &own, &head).await?;
    let report = drafted.reports.last().cloned();
    if let Some(report) = &report {
        record_task(host, task, TaskEvent::Reported(Box::new(report.clone())))?;
    }
    match checks::run_in(host, task, &own, report.as_ref()).await? {
        CheckVerdict::Passed => Ok(Ok(())),
        CheckVerdict::Failed { output } => Ok(Err(format!(
            "the try it kept fails the checks in its own worktree: {output}"
        ))),
        CheckVerdict::Stopped { reason } => Ok(Err(reason)),
    }
}

/// The task's tries in order, numbered from 1: its own, then each made beside it.
fn tries_of(host: &Host, task: &TaskId) -> Vec<(usize, TaskId)> {
    let mut tries = vec![(1, task.clone())];
    for n in 2.. {
        let Ok(id) = try_id(task, n) else { break };
        if host.read(|state| state.tasks.get(&id).is_none()) {
            break;
        }
        tries.push((n, id));
    }
    tries
}

fn try_id(task: &TaskId, n: usize) -> EngineResult<TaskId> {
    TaskId::new(format!("{task}-try-{n}")).map_err(|err| EngineError::Refused(err.to_string()))
}

/// The `n`th try as a task of its own: the whole task again, by `worker` when it is given, else
/// by the task's own.
fn try_spec(spec: &TaskSpec, id: &TaskId, n: usize, worker: Option<&ModelRef>) -> TaskSpec {
    TaskSpec {
        id: id.clone(),
        title: format!("{} (try {n})", spec.title),
        worker: worker.cloned().or_else(|| spec.worker.clone()),
        reviewers: Vec::new(),
        predictions: None,
        item: spec.item.as_ref().map(|item| format!("{item}-try-{n}")),
        way: None,
        ..spec.clone()
    }
}

/// What the try changed, as a reviewer would read it.
async fn diff_of(host: &Host, tree: &TaskTree) -> String {
    let diff = host
        .git()
        .review_diff(&tree.path, &tree.base, "HEAD")
        .await
        .unwrap_or_else(|err| format!("(its diff couldn't be read: {err})"));
    match diff.char_indices().nth(DIFF_CHARS) {
        Some((cut, _)) => format!("{}\n(the diff goes on; this is its start)", &diff[..cut]),
        None => diff,
    }
}

fn first_stands(host: &Host, task: &TaskId, title: &str, n: usize, why: &str) -> EngineResult<()> {
    let text = format!("{title} · try {n} passed first and is kept, since {why}");
    note(host, task, NoteLevel::Warn, text)
}

fn note(host: &Host, task: &TaskId, level: NoteLevel, text: String) -> EngineResult<()> {
    host.record(Some(task), None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::task::{TaskKind, TaskWay, Way};
    use argonauts_core::workflow::WorkflowRef;

    fn model(text: &str) -> ModelRef {
        text.parse().unwrap()
    }

    fn maker(model: &ModelRef) -> String {
        model
            .to_string()
            .split('/')
            .next()
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn tries_are_by_the_workers_model_and_drafts_by_each_maker() {
        let (gpt, luna, qwen) = (
            model("codex/gpt"),
            model("codex/luna"),
            model("opencode/qwen"),
        );
        let workers = [luna.clone(), gpt.clone(), qwen.clone()];
        let tries = makers_of_tries(&gpt, &workers, &maker, Count::Fixed(3), TriedBy::SameModel);
        assert_eq!(tries, [gpt.clone(), gpt.clone(), gpt.clone()]);
        let all = makers_of_tries(&gpt, &workers, &maker, Count::All, TriedBy::SameModel);
        assert_eq!(all.len(), 3);

        // Its own maker first, then one worker of each other maker: never a sibling.
        let drafts = makers_of_tries(&gpt, &workers, &maker, Count::All, TriedBy::EachMaker);
        assert_eq!(drafts, [gpt.clone(), qwen.clone()]);
        let three = makers_of_tries(&gpt, &workers, &maker, Count::Fixed(3), TriedBy::EachMaker);
        assert_eq!(three, [gpt.clone(), qwen, gpt.clone()]);
        let one = std::slice::from_ref(&gpt);
        let alone = makers_of_tries(&gpt, one, &maker, Count::All, TriedBy::EachMaker);
        assert_eq!(alone, [gpt.clone(), gpt]);
    }

    #[test]
    fn a_try_is_the_whole_task_again_under_a_name_of_its_own() {
        let spec = TaskSpec {
            id: TaskId::numbered(1),
            title: "Due docs".to_owned(),
            task: "Write the due date docs.".to_owned(),
            kind: TaskKind::Build,
            owned_paths: vec!["docs.md".to_owned()],
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            worker: Some("codex/gpt".parse().unwrap()),
            reviewers: vec!["antigravity-cli/gemini".parse().unwrap()],
            limit: Default::default(),
            predictions: None,
            item: Some("due-docs".to_owned()),
            way: Some(TaskWay {
                how: Way::Workflow {
                    name: WorkflowRef::new("best-of-n"),
                },
                asked_by_you: true,
                steps: None,
            }),
        };
        let id = try_id(&spec.id, 2).unwrap();
        let second = try_spec(&spec, &id, 2, None);
        assert_eq!(id.as_str(), "task-001-try-2");
        assert_eq!(second.title, "Due docs (try 2)");
        assert_eq!(second.task, spec.task);
        assert_eq!(second.owned_paths, spec.owned_paths);
        assert_eq!(second.worker, spec.worker);
        let qwen: ModelRef = "opencode/qwen".parse().unwrap();
        let drafted = try_spec(&spec, &id, 2, Some(&qwen));
        assert_eq!(drafted.worker, Some(qwen));
        assert_eq!(second.item.as_deref(), Some("due-docs-try-2"));
        assert!(second.reviewers.is_empty() && second.way.is_none());
    }
}
