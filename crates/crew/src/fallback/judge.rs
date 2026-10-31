//! `pick = "judge"`: when several fallbacks did the same task at once and more than one passed
//! its checks, the judge picks the best. The tries reach it numbered and unnamed, each as
//! its diff. A judge that can't be asked, or whose answer names no try, leaves the first
//! that passed standing, and a note says so.

use argonauts_core::agents::session_runner::Access;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::session::InputPartKind;
use argonauts_core::workflows::steps::StepName;
use argonauts_core::workflows::steps::fallback::{PickTry, PickTryInput};
use argonauts_core::{Note, NoteLevel, TaskId};

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::{Ask, Host};
use crate::fallback::rules;
use crate::plan::context::crew_for;
use crate::prompts;
use crate::roles::{self, display_name};
use crate::task::support::part;
use crate::task::worktree::TaskTree;

/// The most of one try's diff the judge reads. A longer one is cut, and says so.
const DIFF_CHARS: usize = 60_000;

/// A try that passed its checks and review, in the order it passed.
pub struct Passed<'a> {
    pub worker: &'a ModelRef,
    pub tree: &'a TaskTree,
}

/// The try whose work the task keeps.
pub async fn pick(host: &Host, task: &TaskId, passed: &[Passed<'_>]) -> EngineResult<ModelRef> {
    let first = passed[0].worker.clone();
    let made: Vec<&ModelRef> = passed.iter().map(|a_try| a_try.worker).collect();
    let Some(judge) = apart_from(host, &made) else {
        say(
            host,
            task,
            &first,
            "no judge apart from them could be asked",
        )?;
        return Ok(first);
    };
    let record = rules::record(host, task)?;
    let input = PickTryInput {
        task_id: task.to_string(),
        title: record.spec.title.clone(),
        task: record.spec.task.clone(),
        tries: u32::try_from(passed.len()).unwrap_or(u32::MAX),
    };
    let mut ask = Ask::new(
        host.read(rules::current_step),
        RoleName::Judge,
        judge.clone(),
        prompts::fallback::PICK,
        input,
    );
    ask.task = Some(task.clone());
    ask.access = Access::NoTools;
    ask.blind = true;
    ask.independent_of = passed.iter().map(|a_try| a_try.worker.clone()).collect();
    ask.slot = slot(host, task);
    for (at, a_try) in passed.iter().enumerate() {
        let label = format!("try {}", at + 1);
        ask.parts.push(part(
            InputPartKind::Fallback,
            label,
            diff_of(host, a_try.tree).await,
            None,
        ));
    }
    let answer = match host.ask::<PickTry>(ask).await {
        Ok(answered) => answered.output,
        Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
        Err(err) => {
            let why = format!(
                "the judge, {}, couldn't answer ({err})",
                display_name(&judge)
            );
            say(host, task, &first, &why)?;
            return Ok(first);
        }
    };
    let picked = usize::try_from(answer.pick)
        .ok()
        .and_then(|number| number.checked_sub(1))
        .and_then(|at| passed.get(at));
    let Some(picked) = picked else {
        say(host, task, &first, "the judge's answer named none of them")?;
        return Ok(first);
    };
    let text = format!(
        "{task} · the judge, {}, picked {}'s try out of {} that passed · {}",
        display_name(&judge),
        display_name(picked.worker),
        passed.len(),
        answer.why.trim()
    );
    note(host, task, NoteLevel::Info, text)?;
    Ok(picked.worker.clone())
}

/// A judge that made none of the results it picks among, the work of `made`: one from another
/// provider than all of them when there is one, since a judge may favour its own provider's
/// work.
pub(crate) fn apart_from(host: &Host, made: &[&ModelRef]) -> Option<ModelRef> {
    let providers: Vec<String> = made
        .iter()
        .map(|worker| roles::provider(host, worker))
        .collect();
    let pool: Vec<ModelRef> = crew_for(host.settings(), RoleName::Judge)
        .into_iter()
        .filter(|worker| !made.contains(&worker) && rules::agent_usable(host, &worker.agent_id()))
        .collect();
    pool.iter()
        .find(|worker| !providers.contains(&roles::provider(host, worker)))
        .or(pool.first())
        .cloned()
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

/// How many times the judge was asked about this task before, which tells this ask apart.
fn slot(host: &Host, task: &TaskId) -> u32 {
    let earlier = host.read(|state| {
        state
            .sessions
            .by_key
            .values()
            .filter(|session| {
                session.task.as_ref() == Some(task)
                    && session.started.step == Some(StepName::PickTry)
            })
            .count()
    });
    u32::try_from(earlier).unwrap_or(u32::MAX)
}

fn say(host: &Host, task: &TaskId, first: &ModelRef, why: &str) -> EngineResult<()> {
    let text = format!(
        "{task} · {}'s try passed first and is kept, since {why}",
        display_name(first)
    );
    note(host, task, NoteLevel::Warn, text)
}

fn note(host: &Host, task: &TaskId, level: NoteLevel, text: String) -> EngineResult<()> {
    host.record(Some(task), None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}
