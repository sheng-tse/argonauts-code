//! A task done the way the plan says, on every CLI, since the engine runs each way itself: in
//! the worker's own session, by agents of its model that each take a part, or step by step
//! through a workflow, whose tries the step after them picks among. A script's writer and the
//! build step of a workflow's phases both start a task's first turn here.

use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::task::{TaskEvent, TaskSpec, TaskState, Way};
use argonauts_core::{Note, NoteLevel, TaskId, look::labels};

use super::{checker, tries};
use crate::council::ways::{self, TaskStep};
use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::Host;
use crate::merge::branch::task_branch;
use crate::review::{self, Verdict, with_findings};
use crate::state::RunState;
use crate::task::checks::{self, CheckVerdict};
use crate::task::machine::Trigger;
use crate::task::pipeline::{self, Until};
use crate::task::support::{record_of, record_task, state_of};
use crate::task::{self, worktree};
use crate::workflow::StepAt;

/// The plan's task that a script's task named `name` is: the one whose key or title it is,
/// however each is written (`Due field` is `due-field`).
pub(crate) fn planned(host: &Host, name: &str) -> Option<TaskSpec> {
    let wanted = labels::id_slug(name, 6);
    host.read(|state| {
        let graph = state.divide.graph.as_ref()?;
        graph
            .tasks
            .iter()
            .find(|spec| {
                spec.item.as_deref() == Some(name)
                    || spec.title.eq_ignore_ascii_case(name)
                    || labels::id_slug(&spec.title, 6) == wanted
            })
            .cloned()
    })
}

/// The parts of a task done by agents, one per agent, when its paths split into them.
fn parts_of(spec: &TaskSpec) -> Option<Vec<Vec<String>>> {
    let Some(Way::Agents { count }) = spec.way.as_ref().map(|way| &way.how) else {
        return None;
    };
    let parts = ways::parts(&spec.owned_paths, *count);
    (parts.len() >= 2).then_some(parts)
}

/// The task as its own session reads it: for agents, with the first of the parts named as its
/// own and the others' files left to them. Any other task reads as it is.
pub fn briefed(mut spec: TaskSpec) -> TaskSpec {
    if let Some(parts) = parts_of(&spec)
        && !spec.task.ends_with(&first_part_words(&parts))
    {
        spec.task = first_part_brief(&spec.task, &parts);
    }
    spec
}

/// What the task's own session is told when it takes the first of the parts.
fn first_part_brief(task: &str, parts: &[Vec<String>]) -> String {
    format!("{task}{}", first_part_words(parts))
}

fn first_part_words(parts: &[Vec<String>]) -> String {
    let others: Vec<String> = parts[1..].iter().map(|paths| paths.join(", ")).collect();
    format!(
        "\n\nYour part is {}. Agents of your model do the other parts at the same time \
         ({}), so leave their files to them.",
        parts[0].join(", "),
        others.join("; ")
    )
}

/// The task's words before its own session was given the first part.
fn whole_task<'a>(spec: &'a TaskSpec, parts: &[Vec<String>]) -> &'a str {
    spec.task
        .strip_suffix(&first_part_words(parts))
        .unwrap_or(&spec.task)
}

/// Why `spec` can't be done its way before anything starts on it: a task for agents whose paths
/// don't split one part per agent, or a workflow one task can't follow.
pub(crate) fn refusal(host: &Host, spec: &TaskSpec) -> Option<String> {
    match spec.way.as_ref().map(|way| &way.how) {
        Some(Way::Agents { .. }) => ways::agents_misfit(spec),
        Some(Way::Workflow { name }) => ways::steps_of(host, name.as_str()).err(),
        _ => None,
    }
}

/// How the first turn at a task ended: where it stopped, or why its way couldn't finish.
pub(crate) type Turn = Result<TaskState, String>;

/// The first turn at `task`, its way as its spec says.
///
/// Each way's work is boxed, so the writer's future stays as small as it was with one way,
/// and a session's deep stack of checks still fits the thread it runs on.
pub(crate) async fn first_turn(host: &Host, task: &TaskId, at: &StepAt) -> EngineResult<Turn> {
    let spec = record_of(host, task)?.spec;
    match spec.way.as_ref().map(|way| &way.how) {
        Some(Way::Agents { .. }) => match parts_of(&spec) {
            Some(parts) => Box::pin(with_agents(host, &spec, at, &parts)).await,
            None => Ok(Err(ways::agents_misfit(&spec).unwrap_or_default())),
        },
        Some(Way::Workflow { name }) => match ways::steps_of(host, name.as_str()) {
            Ok(steps) => Box::pin(by_workflow(host, task, at, &steps)).await,
            Err(why) => Ok(Err(why)),
        },
        _ => Ok(Ok(Box::pin(pipeline::drive(
            host,
            task,
            at,
            Until::ChecksPassed,
        ))
        .await?)),
    }
}

/// A task of the approved plan as a workflow's build step runs it: its first turn done its way,
/// up to the point its checks pass, then on through the run's review and merge as any task
/// goes. A way that couldn't finish leaves the task for you to decide, with why, and a way that
/// can't run at all, which the plan approval warned of, leaves the task to its own session.
///
/// The way's work holds up after a restart: each piece of it picks up where it was or answers
/// from the log, so a task driven again goes through its way again before its review.
pub(crate) async fn drive_planned(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
) -> EngineResult<TaskState> {
    let record = record_of(host, task)?;
    let how = record.spec.way.as_ref().map(|way| &way.how);
    let before_review = matches!(
        record.state,
        TaskState::Waiting
            | TaskState::Ready
            | TaskState::Running
            | TaskState::Checking
            | TaskState::InReview
    );
    if matches!(how, Some(Way::CliWorkflow)) && host.settings().workflows.max_depth > 0 {
        host.shared().let_run_its_own_workflow(task);
    }
    let its_own = !matches!(how, Some(Way::Agents { .. } | Way::Workflow { .. }));
    if its_own || !before_review {
        return pipeline::drive(host, task, at, Until::Merged).await;
    }
    if record.state == TaskState::Waiting {
        if !host.read(|state| pipeline::dependencies_done(state, task)) {
            return Ok(TaskState::Waiting);
        }
        let reason = "its dependencies are merged into the run";
        pipeline::transition(host, task, Trigger::DependenciesMerged, reason)?;
    }
    if let Some(why) = refusal(host, &record.spec) {
        let text = format!(
            "{} is done in its own session, since {why}",
            record.spec.title
        );
        note(host, task, NoteLevel::Warn, text)?;
        return pipeline::drive(host, task, at, Until::Merged).await;
    }
    match Box::pin(first_turn(host, task, at)).await? {
        Ok(TaskState::InReview) => {}
        Ok(state) => return Ok(state),
        Err(why) => {
            let text = format!("{} needs you: {why}", record.spec.title);
            note(host, task, NoteLevel::Warn, text)?;
            if state_of(host, task)? == TaskState::InReview {
                pipeline::transition(host, task, Trigger::ChangesRequestedAgain, &why)?;
            }
        }
    }
    pipeline::drive(host, task, at, Until::Merged).await
}

/// Whether `task` is a part or a try of another task, which only that task's way drives, takes
/// in or sets aside: a build step neither starts it nor waits for it to merge.
pub(crate) fn driven_by_its_way(state: &RunState, task: &TaskId) -> bool {
    let Some((rest, number)) = task.as_str().rsplit_once('-') else {
        return false;
    };
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let (parent, of_agents) = match (rest.strip_suffix("-part"), rest.strip_suffix("-try")) {
        (Some(parent), _) => (parent, true),
        (None, Some(parent)) => (parent, false),
        (None, None) => return false,
    };
    let Ok(parent) = TaskId::new(parent) else {
        return false;
    };
    let way = state
        .tasks
        .get(&parent)
        .and_then(|record| record.spec.way.as_ref());
    match way.map(|way| &way.how) {
        Some(Way::Agents { .. }) => of_agents,
        Some(Way::Workflow { .. }) => !of_agents,
        _ => false,
    }
}

fn note(host: &Host, task: &TaskId, level: NoteLevel, text: String) -> EngineResult<()> {
    host.record(Some(task), None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}

/// The task's parts side by side, one agent of its worker's model each. The task's own session
/// takes the first part in its worktree; each other part is a task of its own, in a worktree
/// from the same base. Once every part's checks pass, the others' work is merged into the
/// task's worktree and the checks run on the whole, so its change and its review hold every
/// part.
async fn with_agents(
    host: &Host,
    spec: &TaskSpec,
    at: &StepAt,
    parts: &[Vec<String>],
) -> EngineResult<Turn> {
    let task = &spec.id;
    let own = worktree::ensure(host, task).await?;
    let mut others = Vec::new();
    for (n, paths) in parts.iter().enumerate().skip(1) {
        let part = part_id(task, n + 1)?;
        if host.read(|state| state.tasks.get(&part).is_none()) {
            let words = whole_task(spec, parts);
            task::create(host, part_spec(spec, words, &part, n + 1, paths))?;
            // From the task's own base, so merging the part brings in its work alone.
            let tree = TaskEvent::Worktree {
                path: host.repo().worktrees.join(part.as_str()),
                branch: task_branch(host.run_id(), &part),
                base: own.base.clone(),
            };
            record_task(host, &part, tree)?;
        }
        others.push(part);
    }
    let mut runs = vec![pipeline::drive(host, task, at, Until::ChecksPassed)];
    runs.extend(
        others
            .iter()
            .map(|part| pipeline::drive(host, part, at, Until::ChecksPassed)),
    );
    let ends = futures::future::join_all(runs).await;
    let mut states = Vec::with_capacity(ends.len());
    for end in ends {
        states.push(end?);
    }
    for (part, state) in others.iter().zip(&states[1..]) {
        if *state != TaskState::InReview {
            let title = record_of(host, part)?.spec.title;
            return Ok(Err(format!(
                "the agent on {title} stopped before its checks passed: it is {}",
                argonauts_core::base::text::state_words(&format!("{state:?}").to_lowercase())
            )));
        }
    }
    if states[0] != TaskState::InReview {
        return Ok(Ok(states[0]));
    }
    let git = worktree::tree_git(host, &own.path).await?;
    for part in &others {
        let title = record_of(host, part)?.spec.title;
        let branch = task_branch(host.run_id(), part);
        let Some(head) = git.branch_head(&host.repo().root, &branch).await? else {
            continue;
        };
        let message = format!("Take in {title}");
        match git.merge_no_ff(&own.path, &head, &message).await? {
            crate::git::MergeOutcome::Merged(_) => {}
            crate::git::MergeOutcome::Conflict(files) => {
                return Ok(Err(format!(
                    "the part {title} doesn't merge into the task: {}",
                    files.join(", ")
                )));
            }
        }
    }
    match checks::run_in(host, task, &own, None).await? {
        CheckVerdict::Passed => Ok(Ok(TaskState::InReview)),
        CheckVerdict::Failed { output } => {
            Ok(Err(format!("the parts together fail the checks: {output}")))
        }
        CheckVerdict::Stopped { reason } => Ok(Err(reason)),
    }
}

/// The task done by the workflow `steps` describe: its worker's own session builds, and a
/// fresh checker reads the change at each check, from another maker whenever one
/// is set up. A check that doesn't accept it sends it back as often as
/// `review.max_change_rounds` allows. Work sent back to a step of tries goes to the try the
/// task kept, so the tries are made once.
async fn by_workflow(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    steps: &[TaskStep],
) -> EngineResult<Turn> {
    let most_rounds = host.settings().review.max_change_rounds;
    let (mut next, mut rounds, mut times_sent_back) = (0, 0, 0);
    let mut sent_back: Option<String> = None;
    while let Some(step) = steps.get(next) {
        match step {
            TaskStep::Build { does, .. } => {
                let state = match (
                    state_of(host, task)?,
                    sent_back.take().or_else(|| does.clone()),
                ) {
                    (TaskState::InReview, Some(words)) => {
                        pipeline::revise(host, task, at, &words).await?
                    }
                    (TaskState::InReview, None) => TaskState::InReview,
                    _ => pipeline::drive(host, task, at, Until::ChecksPassed).await?,
                };
                if state != TaskState::InReview {
                    return Ok(Ok(state));
                }
                next += 1;
            }
            TaskStep::Tries { count, by, .. } => {
                let state = match (state_of(host, task)?, sent_back.take()) {
                    (TaskState::InReview, Some(words)) => {
                        pipeline::revise(host, task, at, &words).await?
                    }
                    _ => Box::pin(tries::make(host, task, at, *count, *by)).await?,
                };
                if state != TaskState::InReview {
                    return Ok(Ok(state));
                }
                next += 1;
            }
            TaskStep::Pick { step, role, by } => {
                let kept = tries::keep_one(host, task, at, step, *role, *by);
                if let Err(why) = Box::pin(kept).await? {
                    return Ok(Err(why));
                }
                next += 1;
            }
            TaskStep::Check {
                step,
                role,
                send_back_to,
            } => {
                let Some(checker) = checker::of_task(host, task, *role)? else {
                    return Ok(Err(format!("it has no worker yet to take its {step} step")));
                };
                rounds += 1;
                let tree = worktree::ensure(host, task).await?;
                let verdict =
                    review::checked_by(host, task, rounds, &checker, Some(&tree.path)).await?;
                match verdict {
                    Verdict::Approved => next += 1,
                    Verdict::ChangesRequested { comments } if times_sent_back < most_rounds => {
                        times_sent_back += 1;
                        let mut words = format!("Sent back by {step}:");
                        for comment in &comments {
                            words.push_str(&format!("\n- {comment}"));
                        }
                        sent_back = Some(words);
                        next = send_back_to.unwrap_or_else(|| last_build(steps, next));
                    }
                    verdict @ Verdict::ChangesRequested { .. } => {
                        let fixes = match times_sent_back {
                            1 => "1 round of fixes".to_owned(),
                            n => format!("{n} rounds of fixes"),
                        };
                        let what = format!("{step} asked for changes again after {fixes}");
                        return Ok(Err(with_findings(what, &verdict)));
                    }
                    Verdict::NeedsYou { reasons } => {
                        return Ok(Err(format!(
                            "its {step} step sent it to you: {}",
                            reasons.join("; ")
                        )));
                    }
                }
            }
        }
    }
    Ok(Ok(state_of(host, task)?))
}

/// The build step a check with no `send_back_to` sends work back to: the last before it.
fn last_build(steps: &[TaskStep], check: usize) -> usize {
    steps[..check]
        .iter()
        .rposition(TaskStep::builds)
        .unwrap_or(0)
}

fn part_id(task: &TaskId, n: usize) -> EngineResult<TaskId> {
    TaskId::new(format!("{task}-part-{n}")).map_err(|err| EngineError::Refused(err.to_string()))
}

/// The `n`th part as a task of its own: the task's `words` with its part named, its paths,
/// and its worker's model.
fn part_spec(spec: &TaskSpec, words: &str, id: &TaskId, n: usize, paths: &[String]) -> TaskSpec {
    TaskSpec {
        id: id.clone(),
        title: part_title(paths),
        task: format!(
            "{words}\n\nYour part is {}. Agents of your model do the other parts at the same \
             time, so leave every other file to them.",
            paths.join(", ")
        ),
        owned_paths: paths.to_vec(),
        reviewers: Vec::new(),
        predictions: None,
        item: spec.item.as_ref().map(|item| format!("{item}-{n}")),
        way: None,
        ..spec.clone()
    }
}

/// A part's name from its first path: `test/routes/orders.test.ts` reads "Orders".
fn part_title(paths: &[String]) -> String {
    let Some(first) = paths.first() else {
        return "Part".to_owned();
    };
    let fixed: Vec<&str> = first
        .split('/')
        .take_while(|part| !part.contains(['*', '?', '[', '{']))
        .filter(|part| !part.is_empty())
        .collect();
    let last = fixed.last().copied().unwrap_or(first.as_str());
    let stem = last
        .split('.')
        .find(|piece| !piece.is_empty())
        .unwrap_or(last);
    let name = ways::workflow_words(stem);
    match paths.len() {
        1 => name,
        n => format!("{name} and {} more", n - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::settings::crew::CrewRole;

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn a_part_is_named_for_its_first_path() {
        assert_eq!(
            part_title(&paths(&["test/routes/orders.test.ts"])),
            "Orders"
        );
        assert_eq!(
            part_title(&paths(&["web/**", "docs/web.md"])),
            "Web and 1 more"
        );
        assert_eq!(part_title(&paths(&[".github/ci.yml"])), "Ci");
    }

    #[test]
    fn the_first_part_is_told_which_files_the_others_take() {
        let parts = vec![
            paths(&["test/orders.test.ts"]),
            paths(&["test/users.test.ts"]),
            paths(&["test/import.test.ts"]),
        ];
        assert_eq!(
            first_part_brief("Write the route tests.", &parts),
            "Write the route tests.\n\nYour part is test/orders.test.ts. Agents of your model do \
             the other parts at the same time (test/users.test.ts; test/import.test.ts), so \
             leave their files to them."
        );
    }

    #[test]
    fn a_task_for_agents_is_briefed_with_its_own_part_once() {
        let spec = TaskSpec {
            id: TaskId::numbered(1),
            title: "Both".to_owned(),
            task: "Write both.".to_owned(),
            kind: argonauts_core::runs::events::task::TaskKind::Build,
            owned_paths: paths(&["a.rs", "b.rs"]),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            worker: None,
            reviewers: Vec::new(),
            limit: Default::default(),
            predictions: None,
            item: None,
            way: Some(argonauts_core::runs::events::task::TaskWay {
                how: Way::Agents { count: 2 },
                asked_by_you: false,
                steps: None,
            }),
        };
        let once = briefed(spec.clone());
        assert_eq!(
            once.task,
            "Write both.\n\nYour part is a.rs. Agents of your model do the other parts at the \
             same time (b.rs), so leave their files to them."
        );
        assert_eq!(briefed(once.clone()).task, once.task);
        let alone = TaskSpec { way: None, ..spec };
        assert_eq!(briefed(alone).task, "Write both.");
    }

    #[test]
    fn each_other_part_reads_the_task_without_the_first_parts_words() {
        let parts = vec![paths(&["a.rs"]), paths(&["b.rs"])];
        let mut spec = TaskSpec {
            id: TaskId::numbered(1),
            title: "Both".to_owned(),
            task: first_part_brief("Write both.", &parts),
            kind: argonauts_core::runs::events::task::TaskKind::Build,
            owned_paths: paths(&["a.rs", "b.rs"]),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            worker: None,
            reviewers: Vec::new(),
            limit: Default::default(),
            predictions: None,
            item: Some("both".to_owned()),
            way: None,
        };
        assert_eq!(whole_task(&spec, &parts), "Write both.");
        let second = part_spec(
            &spec,
            whole_task(&spec, &parts),
            &TaskId::numbered(2),
            2,
            &parts[1],
        );
        assert_eq!(
            second.task,
            "Write both.\n\nYour part is b.rs. Agents of your model do the other parts at the \
             same time, so leave every other file to them."
        );
        assert_eq!(
            (second.title.as_str(), second.item.as_deref()),
            ("B", Some("both-2"))
        );
        spec.task = "Changed by hand.".to_owned();
        assert_eq!(whole_task(&spec, &parts), "Changed by hand.");
    }

    #[test]
    fn a_check_sends_work_back_to_the_build_before_it() {
        let build = |name: &str| TaskStep::Build {
            step: name.to_owned(),
            does: None,
        };
        let check = TaskStep::Check {
            step: "Review".to_owned(),
            role: CrewRole::Reviewer,
            send_back_to: None,
        };
        let steps = [build("Build"), build("Polish"), check];
        assert_eq!(last_build(&steps, 2), 1);
    }
}
