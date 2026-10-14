//! Several fallbacks in one drafting agent's place (`together` of 2 or more in the fallback table
//! the drafting agents follow). When the drafting agent is out as its draft starts, that many
//! fallbacks draft at once in its place. When it stops midway and one fallback takes its place, the
//! rest join once that one has answered. The drafting agent's place still gives one plan: a judge
//! that wrote none of the drafts, from another provider when there is one, reads them numbered and
//! unnamed and picks the best. A plan passes no checks and holds no findings to fold, so the judge
//! picks whatever `pick` says. A judge that can't be asked, or whose answer scores no draft, leaves
//! the first draft standing, and a note says so.

use futures::future::join_all;

use argonauts_core::events::RunEventKind;
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::session_runner::Access;
use argonauts_core::steps::Step;
use argonauts_core::steps::plan::{
    Draft, DraftInput, Judge, JudgeInput, LabeledPlan, PlanDraft, RubricCriterion,
};
use argonauts_core::{Note, NoteLevel};

use crate::engine::host::{Answered, Ask, Host, round_window};
use crate::engine::job_fallback::{self, Job};
use crate::error::{EngineError, EngineResult};
use crate::fallback::{judge, rules};
use crate::plan::{context, drafts};
use crate::prompts::plan::PICK_DRAFT;
use crate::roles::display_name;
use crate::workflow::StepAt;

/// What the judge scores each draft on.
const BEST: &str = "10 means the plan would reach the goal, all of it and nothing else, in the \
                    simplest way to build and to test.";

/// A drafting agent's job in the step at `at`, which whoever drafts in its place must fit.
fn draft_job(at: Option<&StepAt>, blind: bool) -> Job {
    Job {
        at: at.cloned(),
        role: RoleName::Draft,
        step: Some(Draft::NAME),
        task: None,
        independent_of: Vec::new(),
        isolated: blind,
    }
}

/// Gives each of `drafting_agents` its own job before any is asked, so none becomes the fallback of
/// another too and drafts twice.
pub(super) fn hold_jobs(host: &Host, at: &StepAt, drafting_agents: &[ModelRef]) {
    let job = draft_job(Some(at), true);
    for drafting_agent in drafting_agents {
        job_fallback::hold(host, &job, drafting_agent);
    }
}

/// Every draft written for the drafting agent `ask` is for: the drafting agent's own, the one
/// fallback's that took its place, or one from each of several fallbacks that drafted at once.
pub(super) async fn drafts(
    host: &Host,
    ask: Ask<DraftInput>,
) -> EngineResult<Vec<Answered<PlanDraft>>> {
    let drafting_agent = ask.worker.clone();
    let applied = rules::applied(host, RoleName::Draft, &drafting_agent.agent_id());
    let together = usize::try_from(applied.together).unwrap_or(usize::MAX);
    if together < 2 {
        return Ok(vec![host.ask::<Draft>(ask).await?]);
    }
    let job = draft_job(ask.at.as_ref(), ask.blind);
    let mut written = Vec::new();
    let mut by = if job_fallback::is_out(host, &drafting_agent).await {
        job_fallback::together(host, &job, &drafting_agent, &[], together)
    } else {
        Vec::new()
    };
    if by.len() < 2 {
        // Its own draft, or that of the one fallback that takes its place, which the rest join.
        let own = host.ask::<Draft>(ask.clone()).await?;
        if own.worker == drafting_agent {
            return Ok(vec![own]);
        }
        by = job_fallback::together(host, &job, &drafting_agent, &[&own.worker], together - 1);
        say_who_drafts(
            host,
            ask.at.as_ref(),
            &drafting_agent,
            &by,
            Some(&own.worker),
        )?;
        written.push(own);
    } else {
        say_who_drafts(host, ask.at.as_ref(), &drafting_agent, &by, None)?;
    }
    let asked = join_all(by.iter().map(|taker| {
        // Under its own name, so a step entered again finds its draft by that name.
        let mut ask = ask.clone();
        ask.worker = taker.clone();
        host.ask::<Draft>(ask)
    }))
    .await;
    let mut failed = None;
    for (taker, answer) in by.iter().zip(asked) {
        match answer {
            Ok(answered) => written.push(answered),
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(error) => {
                let name = display_name(taker);
                context::warn(host, None, format!("{name}'s draft failed: {error}"))?;
                failed.get_or_insert(error);
            }
        }
    }
    match failed {
        Some(error) if written.is_empty() => Err(error),
        _ => Ok(written),
    }
}

/// The one plan of a drafting agent's place: its only draft, or the judge's pick among those its
/// fallbacks wrote. The picks of one step are asked one after another, since their order is what
/// tells them apart when the step is entered again.
pub(super) async fn pick(
    host: &Host,
    at: &StepAt,
    mut written: Vec<Answered<PlanDraft>>,
) -> EngineResult<Answered<PlanDraft>> {
    if written.len() < 2 {
        return written
            .pop()
            .ok_or_else(|| EngineError::Refused("no draft was written".to_owned()));
    }
    let authors: Vec<&ModelRef> = written.iter().map(|draft| &draft.worker).collect();
    let Some(judge) = judge::apart_from(host, &authors) else {
        return keep_first(host, written, "no judge apart from them could be asked");
    };
    let plans = written
        .iter()
        .zip(1..)
        .map(|(draft, label)| LabeledPlan {
            label,
            text: drafts::render(&draft.output),
        })
        .collect();
    let input = JudgeInput {
        goal: host.read(context::goal_brief),
        rubric: vec![RubricCriterion {
            name: "the best plan".to_owned(),
            description: BEST.to_owned(),
        }],
        plans,
        experiments: Vec::new(),
    };
    let mut ask = Ask::new(
        Some(at.clone()),
        RoleName::Judge,
        judge.clone(),
        PICK_DRAFT,
        input,
    );
    ask.access = Access::NoTools;
    ask.blind = true;
    ask.independent_of = authors.into_iter().cloned().collect();
    ask.slot = picks_done(host, at, &judge);
    let scores = match host.ask::<Judge>(ask).await {
        Ok(answered) => answered.output.scores,
        Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
        Err(error) => {
            let why = format!(
                "the judge, {}, couldn't answer ({error})",
                display_name(&judge)
            );
            return keep_first(host, written, &why);
        }
    };
    let best = scores
        .iter()
        .filter_map(|score| {
            let value = *score.criteria.first()?;
            let at = usize::from(score.plan).checked_sub(1)?;
            (value.is_finite() && at < written.len()).then_some((at, value, &score.rationale))
        })
        // The first of two with the same score, as the first to arrive.
        .reduce(|best, next| if next.1 > best.1 { next } else { best });
    let Some((at, _, rationale)) = best else {
        return keep_first(host, written, "the judge's answer scored none of them");
    };
    let text = format!(
        "The judge, {}, picked {}'s draft out of {} · {}",
        display_name(&judge),
        display_name(&written[at].worker),
        written.len(),
        rationale.trim()
    );
    note(host, NoteLevel::Info, text)?;
    Ok(written.swap_remove(at))
}

/// How many of `judge`'s picks in this step's round ended for good, which is the place of the
/// next one among them: a pick that broke off with the run is asked again in its own session.
fn picks_done(host: &Host, at: &StepAt, judge: &ModelRef) -> u32 {
    let done = host.read(|state| {
        let (from, until) = round_window(state, Some(at));
        state
            .sessions
            .by_key
            .values()
            .filter(|session| {
                session.since >= from
                    && until.is_none_or(|until| session.since < until)
                    && session.started.workflow_step.as_ref() == Some(&at.step)
                    && session.started.role == RoleName::Judge
                    && session.started.step == Some(Judge::NAME)
                    && session.asked_worker() == judge
                    && !session.resumable()
            })
            .count()
    });
    u32::try_from(done).unwrap_or(u32::MAX)
}

fn keep_first(
    host: &Host,
    mut written: Vec<Answered<PlanDraft>>,
    why: &str,
) -> EngineResult<Answered<PlanDraft>> {
    let first = written.swap_remove(0);
    let text = format!(
        "{}'s draft came first and is kept, since {why}",
        display_name(&first.worker)
    );
    note(host, NoteLevel::Warn, text)?;
    Ok(first)
}

/// `Sonnet 5.5 on Claude Code and GPT 5.6 on Codex both draft the plan in place of Opus 5.5
/// on Claude Code`, and that the judge picks. `more` are about to be asked beside `already`,
/// who has the job. Says nothing when one of `more` was asked in this round before, since it
/// was said then.
fn say_who_drafts(
    host: &Host,
    at: Option<&StepAt>,
    drafting_agent: &ModelRef,
    more: &[ModelRef],
    already: Option<&ModelRef>,
) -> EngineResult<()> {
    if more.iter().any(|taker| asked_before(host, at, taker)) {
        return Ok(());
    }
    let names: Vec<String> = already.into_iter().chain(more).map(display_name).collect();
    let who = match names.as_slice() {
        [] | [_] => return Ok(()),
        [one, other] => format!("{one} and {other} both draft"),
        [rest @ .., last] => format!("{} and {last} all draft", rest.join(", ")),
    };
    let text = format!(
        "{who} the plan in place of {} · the judge picks the best · /fallback changes this",
        display_name(drafting_agent)
    );
    note(host, NoteLevel::Info, text)
}

/// Whether `taker` has a draft session of its own in this step's round.
fn asked_before(host: &Host, at: Option<&StepAt>, taker: &ModelRef) -> bool {
    host.read(|state| {
        let (from, until) = round_window(state, at);
        let step = at.map(|at| &at.step);
        state.sessions.by_key.values().any(|session| {
            session.since >= from
                && until.is_none_or(|until| session.since < until)
                && session.started.workflow_step.as_ref() == step
                && session.started.role == RoleName::Draft
                && session.started.step == Some(Draft::NAME)
                && session.asked_worker() == taker
        })
    })
}

fn note(host: &Host, level: NoteLevel, text: String) -> EngineResult<()> {
    host.record(None, None, RunEventKind::Note(Note { level, text }))?;
    Ok(())
}
