//! `pick = "judge"`: when several fallbacks did the same task at once and more than one passed
//! its checks, the judge picks the best. The attempts reach it numbered and unnamed, each as
//! its diff. A judge that can't be asked, or whose answer names no attempt, leaves the first
//! that passed standing, and a note says so.

use argonauts_core::events::RunEventKind;
use argonauts_core::events::session::InputPartKind;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::steps::StepName;
use argonauts_core::steps::handoff::{PickAttempt, PickAttemptInput};
use argonauts_core::worker::Access;
use argonauts_core::{CardId, Note, NoteLevel};

use crate::card::support::part;
use crate::card::worktree::CardTree;
use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::handoff::rules;
use crate::plan::context::crew_for;
use crate::prompts;
use crate::roles::{self, display_name};

/// The most of one attempt's diff the judge reads. A longer one is cut, and says so.
const DIFF_CHARS: usize = 60_000;

/// An attempt that passed its checks and review, in the order it passed.
pub struct Passed<'a> {
    pub worker: &'a WorkerRef,
    pub tree: &'a CardTree,
}

/// The attempt whose work the task keeps.
pub async fn pick(host: &Host, card: &CardId, passed: &[Passed<'_>]) -> EngineResult<WorkerRef> {
    let first = passed[0].worker.clone();
    let made: Vec<&WorkerRef> = passed.iter().map(|attempt| attempt.worker).collect();
    let Some(judge) = apart_from(host, &made) else {
        say(
            host,
            card,
            &first,
            "no judge apart from them could be asked",
        )?;
        return Ok(first);
    };
    let record = rules::record(host, card)?;
    let input = PickAttemptInput {
        card: card.to_string(),
        title: record.spec.title.clone(),
        task: record.spec.task.clone(),
        attempts: u32::try_from(passed.len()).unwrap_or(u32::MAX),
    };
    let mut ask = Ask::new(
        host.read(rules::current_step),
        RoleName::Judge,
        judge.clone(),
        prompts::handoff::PICK,
        input,
    );
    ask.card = Some(card.clone());
    ask.access = Access::NoTools;
    ask.blind = true;
    ask.independent_of = passed
        .iter()
        .map(|attempt| attempt.worker.clone())
        .collect();
    ask.slot = slot(host, card);
    for (at, attempt) in passed.iter().enumerate() {
        let label = format!("attempt {}", at + 1);
        ask.parts.push(part(
            InputPartKind::Handoff,
            label,
            diff_of(host, attempt.tree).await,
            None,
        ));
    }
    let answer = match host.ask::<PickAttempt>(ask).await {
        Ok(answered) => answered.output,
        Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
        Err(err) => {
            let why = format!(
                "the judge, {}, couldn't answer ({err})",
                display_name(&judge)
            );
            say(host, card, &first, &why)?;
            return Ok(first);
        }
    };
    let picked = usize::try_from(answer.pick)
        .ok()
        .and_then(|number| number.checked_sub(1))
        .and_then(|at| passed.get(at));
    let Some(picked) = picked else {
        say(host, card, &first, "the judge's answer named none of them")?;
        return Ok(first);
    };
    let text = format!(
        "{card} · the judge, {}, picked {}'s attempt out of {} that passed · {}",
        display_name(&judge),
        display_name(picked.worker),
        passed.len(),
        answer.why.trim()
    );
    note(host, card, NoteLevel::Info, text)?;
    Ok(picked.worker.clone())
}

/// A judge that made none of the results it picks among, the work of `made`: one from another
/// provider than all of them when there is one, since a judge may favour its own provider's
/// work.
pub(crate) fn apart_from(host: &Host, made: &[&WorkerRef]) -> Option<WorkerRef> {
    let providers: Vec<String> = made
        .iter()
        .map(|worker| roles::family(host, worker))
        .collect();
    let pool: Vec<WorkerRef> = crew_for(host.settings(), RoleName::Judge)
        .into_iter()
        .filter(|worker| !made.contains(&worker) && rules::vendor_usable(host, &worker.vendor()))
        .collect();
    pool.iter()
        .find(|worker| !providers.contains(&roles::family(host, worker)))
        .or(pool.first())
        .cloned()
}

/// What the attempt changed, as a reviewer would read it.
async fn diff_of(host: &Host, tree: &CardTree) -> String {
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
fn slot(host: &Host, card: &CardId) -> u32 {
    let earlier = host.read(|state| {
        state
            .sessions
            .by_key
            .values()
            .filter(|session| {
                session.card.as_ref() == Some(card)
                    && session.started.step == Some(StepName::PickAttempt)
            })
            .count()
    });
    u32::try_from(earlier).unwrap_or(u32::MAX)
}

fn say(host: &Host, card: &CardId, first: &WorkerRef, why: &str) -> EngineResult<()> {
    let text = format!(
        "{card} · {}'s attempt passed first and is kept, since {why}",
        display_name(first)
    );
    note(host, card, NoteLevel::Warn, text)
}

fn note(host: &Host, card: &CardId, level: NoteLevel, text: String) -> EngineResult<()> {
    host.record(Some(card), None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}
