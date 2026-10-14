//! The `draft` step: blind drafts in fresh read-only sessions, sealed as they arrive, and in a
//! round after critique, each drafting agent revising its own plan in its own session. A plan you
//! sent back at the plan approval starts a new planning round of blind drafts, with your note. A
//! drafting agent that's out may have several fallbacks draft in its place at once, and then a
//! judge picks its one plan among theirs (`together`).

use futures::future::join_all;

use argonauts_core::RunId;
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::names::{ModelRef, PlanLabel, RoleName};
use argonauts_core::session_runner::Access;
use argonauts_core::sha256_hex;
use argonauts_core::steps::plan::{DraftInput, Revise, ReviseInput};

use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::plan::context::{self, part, warn, workers_for};
use crate::plan::{drafts, map, together};
use crate::prompts::plan::{DRAFT, REVISE};
use crate::state::plan::{self, SealedDraft};
use crate::status::Viewer;
use crate::task::at_start;
use crate::workflow::{StepCall, StepDone};

pub async fn run(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    context::phase_started(host)?;
    let (sent_back, critiqued) = host.read(|state| {
        let first = state.plan.first_round;
        let critiqued = state
            .plan
            .critiques
            .iter()
            .map(|c| c.round)
            .filter(|round| *round > 0 && *round >= first)
            .max();
        (plan::sent_back(state).is_some(), critiqued)
    });
    match critiqued {
        Some(round) if !sent_back => revise(host, call, round).await,
        _ => blind(host, call, sent_back).await,
    }
}

/// Your send-back note as a prompt part, for the steps that plan again after it.
pub fn guidance_part(note: Option<&str>, source: &str) -> Option<InputPart> {
    let note = note?.trim();
    let text = if note.is_empty() {
        "You sent the last plan back at the plan approval without a note. Plan again from the \
         goal."
            .to_owned()
    } else {
        format!("You sent the last plan back at the plan approval with this note:\n\n{note}")
    };
    Some(InputPart {
        kind: InputPartKind::Message,
        label: "your note".to_owned(),
        approx_tokens: u32::try_from(text.len() / 4).unwrap_or(u32::MAX),
        text,
        source: Some(source.to_owned()),
    })
}

/// Labels 1 to N in an order drawn from the run id, so a judge can't tell a plan's author from
/// its label, and a re-entered step gives every drafting agent the same label again.
pub fn labels(drafting_agents: &[ModelRef], run: &RunId) -> Vec<(ModelRef, PlanLabel)> {
    let mut order: Vec<(String, &ModelRef)> = drafting_agents
        .iter()
        .map(|worker| (sha256_hex(format!("{run}:{worker}").as_bytes()), worker))
        .collect();
    order.sort();
    order
        .into_iter()
        .enumerate()
        .map(|(n, (_, worker))| {
            (
                worker.clone(),
                (n + 1).min(PlanLabel::MAX as usize) as PlanLabel,
            )
        })
        .collect()
}

/// After a send back every drafting agent drafts again in a new round; otherwise only those
/// without a draft in this planning round do.
async fn blind(host: &Host, call: &StepCall, sent_back: bool) -> EngineResult<StepDone> {
    let settings = host.settings();
    let drafting_agents = workers_for(call, settings, RoleName::Draft, settings.drafting.agents);
    if drafting_agents.is_empty() {
        return Err(EngineError::Refused(
            "no drafting agent is available".to_owned(),
        ));
    }
    let (sealed, round, note): (Vec<SealedDraft>, u8, Option<String>) = host.read(|state| {
        let sealed = if sent_back {
            Vec::new()
        } else {
            state.plan.latest().into_iter().cloned().collect()
        };
        (
            sealed,
            state.plan.blind_round(sent_back),
            plan::replan_note(state),
        )
    });
    let (goal, decisions) =
        host.read(|state| (context::goal_brief(state), context::decision_lines(state)));
    // The planner hears what already fails before it writes a rule that would forbid the fix.
    at_start::ensure(host).await?;
    let mut notes = context::notes(host);
    notes.extend(host.read(at_start::note));
    let crew = context::task_roles_lines(host)?;
    // By label, since a fallback seals a drafting agent's plan under its own name.
    let missing: Vec<(ModelRef, PlanLabel)> = labels(&drafting_agents, host.run_id())
        .into_iter()
        .filter(|(_, label)| !sealed.iter().any(|d| d.label == *label))
        .collect();
    // A later `draft` step with nothing critiqued, such as with critique off: nothing to revise.
    let mapped = host.read(|state| state.plan.latest_map().is_some());
    if missing.is_empty() && mapped {
        return Ok(StepDone::skipped("No critique to revise from"));
    }
    together::hold_jobs(host, &call.at, &drafting_agents);
    let asks = missing.into_iter().map(|(worker, label)| {
        let viewer = Viewer::Session {
            role: RoleName::Draft,
            task: None,
            label: Some(label),
        };
        let input = DraftInput {
            goal: goal.clone(),
            notes: notes.clone(),
            decisions: decisions.clone(),
            status: host.status_text(&viewer),
            crew: crew.clone(),
        };
        let mut ask = Ask::new(
            Some(call.at.clone()),
            RoleName::Draft,
            worker.clone(),
            crate::task::prompt::replaced(host, &settings.prompts.drafting, DRAFT),
            input,
        );
        ask.access = Access::ReadOnly;
        ask.blind = true;
        ask.label = Some(label);
        ask.parts
            .extend(guidance_part(note.as_deref(), "plan approval"));
        async move { (worker, label, together::drafts(host, ask).await) }
    });
    let mut new = 0;
    let mut failed = Vec::new();
    for (worker, label, written) in join_all(asks).await {
        let answer = match written {
            Ok(written) => together::pick(host, &call.at, written).await,
            Err(error) => Err(error),
        };
        match answer {
            Ok(answered) => {
                drafts::seal(
                    host,
                    label,
                    &answered.worker,
                    round,
                    &answered.output,
                    Some(&answered.session),
                )?;
                new += 1;
            }
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(error) => {
                warn(
                    host,
                    None,
                    format!(
                        "{}'s draft failed and is left out: {error}",
                        crate::roles::display_name(&worker)
                    ),
                )?;
                failed.push(worker);
            }
        }
    }
    let total = sealed.len() + new;
    if total == 0 {
        return Err(EngineError::Refused(
            "no drafting agent produced a plan".to_owned(),
        ));
    }
    let mut summary = format!("{total} draft{} sealed", if total == 1 { "" } else { "s" });
    if !failed.is_empty() {
        summary.push_str(&format!(", {} failed", failed.len()));
    }
    if total < 2 {
        summary.push_str("; the fast path takes it from here");
    }
    Ok(StepDone::done(summary))
}

/// Each plan of the critiqued round, revised by its drafting agent in its own session with the
/// points addressed to its label. A failed revision keeps the original draft.
async fn revise(host: &Host, call: &StepCall, critiqued: u8) -> EngineResult<StepDone> {
    let (targets, points, disputed) = host.read(|state| {
        let plan = &state.plan;
        (
            plan.latest()
                .into_iter()
                .filter(|d| d.round == critiqued)
                .cloned()
                .collect::<Vec<_>>(),
            plan.critiques_of_round(critiqued)
                .flat_map(|c| c.points.clone())
                .collect::<Vec<_>>(),
            plan.disputed()
                .into_iter()
                .map(map::to_step_topic)
                .collect::<Vec<_>>(),
        )
    });
    if targets.is_empty() {
        return Ok(StepDone::done("Every critiqued plan is revised"));
    }
    let already_failing = at_start::part(host).await?;
    let asks = targets.iter().map(|target| {
        let input = ReviseInput {
            label: target.label,
            critique: points
                .iter()
                .filter(|p| p.plan == target.label)
                .cloned()
                .collect(),
            disputed: disputed.clone(),
        };
        let mut ask = Ask::new(
            Some(call.at.clone()),
            RoleName::Draft,
            target.worker.clone(),
            REVISE,
            input,
        );
        ask.access = Access::ReadOnly;
        ask.label = Some(target.label);
        ask.resume = target.session.clone();
        let already_failing = already_failing.clone();
        async move {
            // Without its own session to resume, the drafting agent gets its sealed plan back,
            // and what it heard when it drafted.
            if ask.resume.is_none() {
                let text = drafts::read_text(target)?;
                ask.parts
                    .push(part(InputPartKind::StepInput, "your plan", text));
                ask.parts.extend(already_failing);
            }
            host.ask::<Revise>(ask).await
        }
    });
    let mut revised = 0;
    for (target, answer) in targets.iter().zip(join_all(asks).await) {
        match answer {
            Ok(answered) => {
                drafts::seal(
                    host,
                    target.label,
                    &answered.worker,
                    critiqued.saturating_add(1),
                    &answered.output,
                    Some(&answered.session),
                )?;
                revised += 1;
            }
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(error) => warn(
                host,
                None,
                format!(
                    "{}'s revision failed, so Plan {} keeps its draft: {error}",
                    target.worker, target.label
                ),
            )?,
        }
    }
    Ok(StepDone::done(format!(
        "{revised} of {} plans revised",
        targets.len()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_a_stable_permutation_of_one_to_n() {
        let drafting_agents: Vec<ModelRef> = ["claude-code", "codex", "antigravity-cli"]
            .iter()
            .map(|w| w.parse().unwrap())
            .collect();
        let run = RunId::new("run-01").unwrap();
        let first = labels(&drafting_agents, &run);
        assert_eq!(first, labels(&drafting_agents, &run));
        let mut numbers: Vec<PlanLabel> = first.iter().map(|(_, l)| *l).collect();
        numbers.sort_unstable();
        assert_eq!(numbers, [1, 2, 3]);
        let reversed: Vec<ModelRef> = drafting_agents.iter().rev().cloned().collect();
        let mut again = labels(&reversed, &run);
        let mut first = first;
        again.sort();
        first.sort();
        assert_eq!(again, first);
    }
}
