//! The `merge` step: main merged in if it moved, the final check, the merge checkpoint with the
//! debt rule, and landing by your answer.

use futures::future::join_all;

use argonauts_core::PhaseStatus;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::CardState;
use argonauts_core::events::checkpoint::{
    AnswerAction, AnswerOption, CheckpointDecided, CheckpointKind, CheckpointPacket, FreeText,
};
use argonauts_core::events::run::{Party, PhaseChange, RunOutcome, RunPhase};
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::handoff::QualityDebt;
use argonauts_core::names::{CheckpointId, VendorId};
use argonauts_core::policy::EvidenceSubject;
use argonauts_core::settings::protocol::MergeMode;
use argonauts_core::{CardId, Note, NoteLevel};

use crate::card::pipeline;
use crate::checkpoints::{self, CheckpointRequest, options};
use crate::engine::commands;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::handoff::merge_return::{self, Rerun};
use crate::handoff::{merge_rule, rules, upgrade};
use crate::integrate::final_check::{self, FinalCheck};
use crate::integrate::land::{self, Landing};
use crate::integrate::merge_packet::{self, MergeEvidence};
use crate::retro;
use crate::review::Verdict;
use crate::state::integration::IntegrationState;
use crate::workflow::{Flow, StepCall, StepDone};

pub async fn merge_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    // Re-entered after a crash that came after landing: finish the way it already ended.
    if let Some(outcome) = host.read(|state| ended(&state.integration)) {
        return finish(host, outcome, outcome_summary(outcome));
    }
    // Cards the checkpoint sent back before a restart get back in first.
    run_all(host, call, &in_flight(host)).await?;
    // Why the checkpoint opens again. It goes into the evidence, so a new one opens.
    let mut notices: Vec<String> = Vec::new();
    let mut seen: Vec<CheckpointId> = Vec::new();
    loop {
        let answer = match answered(host, call, &seen).await? {
            Some((decided, approved)) => Answer::Given(Box::new((decided, approved))),
            None => Answer::Pending(checked(host).await?),
        };
        enter(host, RunPhase::Merge)?;
        match host.settings().merge.mode {
            MergeMode::Leave => {
                return finish(host, RunOutcome::BranchLeft, "branch kept by merge.mode");
            }
            MergeMode::PullRequest => {
                land::pull_request_by_rule(host).await?;
                return finish(
                    host,
                    RunOutcome::PullRequest,
                    "pull request opened by merge.mode",
                );
            }
            MergeMode::Ask => {}
        }
        let returned = merge_return::decide(host, &call.at).await?;
        if !returned.is_empty() {
            notices.extend(rerun_returned(host, call, &returned).await?);
            continue;
        }
        let blocked = merge_rule::blocking_debt(host);
        let (decided, approved) = match answer {
            Answer::Given(given) => *given,
            Answer::Pending(check) => match ask(host, call, &check, &blocked, &notices).await? {
                Ok(asked) => asked,
                // Something changed under the card, such as a card you reverted, so it opens
                // again on what's true now.
                Err(why) => {
                    notices.push(why);
                    continue;
                }
            },
        };
        if seen.contains(&decided.id) {
            return Err(EngineError::Refused(
                "the merge checkpoint could not open again".to_owned(),
            ));
        }
        seen.push(decided.id.clone());

        // Every way back to the checkpoint adds a line, so its evidence is never the same as a
        // decision already acted on.
        match decided.action {
            AnswerAction::RequestChanges => {
                let cards = requested(host, &decided);
                let note = decided
                    .text
                    .clone()
                    .unwrap_or_else(|| "changes requested at the merge checkpoint".to_owned());
                let sent = rerun(host, call, &cards, &note).await?;
                notices.extend(sent_back_notices("changes requested", &cards, &sent));
            }
            AnswerAction::Wait => {
                wait_for_originals(host, &blocked).await?;
                notices.push(format!(
                    "waited for the originals of {}",
                    card_list(&blocked)
                ));
            }
            AnswerAction::UpgradeReview => {
                let Some(reviewer) = merge_rule::stronger_candidate(host, &blocked) else {
                    notices.push("no model at least as strong is free to review".to_owned());
                    continue;
                };
                let mut asked = Vec::new();
                for (card, _) in &blocked {
                    let (verdict, _) = upgrade::review(host, card, &reviewer).await?;
                    if verdict != Verdict::Approved {
                        asked.push(card.clone());
                    }
                }
                let note = "the upgrade review asked for changes";
                let sent = rerun(host, call, &asked, note).await?;
                notices.push(format!("{reviewer} reviewed {}", card_list(&blocked)));
                notices.extend(sent_back_notices(note, &asked, &sent));
            }
            AnswerAction::Merge | AnswerAction::Approve
                if !blocked.is_empty() && !merge_rule::may_merge_with_debt(host, &decided.by) =>
            {
                notices.push(format!(
                    "merging with quality debt on {} needs you, since \
                     approvals.merge.rules.allow_debt is off",
                    card_list(&blocked)
                ));
            }
            _ => match land::act(host, &decided, &approved).await? {
                Landing::Done(outcome) => {
                    return finish(host, outcome, outcome_summary(outcome));
                }
                Landing::Again(why) => notices.push(why),
                Landing::Later => return merge_later(host).await,
            },
        }
    }
}

/// You chose to merge later: the run pauses at the merge, waiting to merge, and resuming it
/// acts on the same approval and tries again.
async fn merge_later(host: &Host) -> EngineResult<StepDone> {
    let text = format!(
        "The run waits to merge. Once those files are out of the way, /resume {} merges it.",
        host.run_id()
    );
    host.record(
        None,
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text,
        }),
    )?;
    if commands::begin_pause_by(host, false, Party::User)? {
        commands::finish_pause(host).await?;
    }
    Err(EngineError::Interrupted)
}

/// Your answer, already given, or the final check the checkpoint opens with.
enum Answer {
    Given(Box<(CheckpointDecided, EvidenceSubject)>),
    Pending(FinalCheck),
}

/// An answer given in this round that hasn't been acted on here, while the branch is still
/// where its checkpoint showed it. A run woken by your answer acts on it without running the
/// suite or building the packet again, which could change the evidence and drop the answer.
async fn answered(
    host: &Host,
    call: &StepCall,
    seen: &[CheckpointId],
) -> EngineResult<Option<(CheckpointDecided, EvidenceSubject)>> {
    let found = checkpoints::decided_in_round(host, &call.at, CheckpointKind::Merge, &title(host))?;
    let Some((decided, Some(shown))) = found else {
        return Ok(None);
    };
    if seen.contains(&decided.id) {
        return Ok(None);
    }
    let now = merge_packet::position(host).await?.subject;
    Ok((now == shown).then_some((decided, shown)))
}

/// The final check, run again only when the branch or the target moved since the last one.
/// The run's notes and decisions are committed first, so the check and the checkpoint cover
/// them.
pub(crate) async fn checked(host: &Host) -> EngineResult<FinalCheck> {
    retro::notes_before_merge(host).await?;
    if let Some(check) = final_check::reusable(host).await? {
        return Ok(check);
    }
    phase(host, RunPhase::FinalCheck, PhaseStatus::Started, None)?;
    let check = final_check::run(host).await?;
    let (status, summary) = if check.passed {
        (PhaseStatus::Finished, "final check passed")
    } else {
        (PhaseStatus::Failed, "final check failed")
    };
    phase(host, RunPhase::FinalCheck, status, Some(summary))?;
    Ok(check)
}

/// Builds the evidence and waits for the checkpoint's decision, or why it was withdrawn.
async fn ask(
    host: &Host,
    call: &StepCall,
    check: &FinalCheck,
    blocked: &[(CardId, QualityDebt)],
    notices: &[String],
) -> EngineResult<Result<(CheckpointDecided, EvidenceSubject), String>> {
    let evidence = merge_packet::build(host, check).await?;
    let Some(approved) = evidence.subject().cloned() else {
        return Err(EngineError::Refused(
            "the merge has nothing to approve".to_owned(),
        ));
    };
    let request = request(host, call, evidence, blocked, notices);
    let decided = checkpoints::open(host, request)?
        .decided_or_withdrawn(host)
        .await?;
    Ok(decided.map(|decided| (decided, approved)))
}

/// The question the card asks, such as "Merge this run into main?". The screen shows it, so
/// it names the target and leaves the run's id to the detail views.
fn title(host: &Host) -> String {
    match final_check::target(host) {
        Some(target) => format!("Merge this run into {target}?"),
        None => "Merge this run?".to_owned(),
    }
}

fn request(
    host: &Host,
    call: &StepCall,
    evidence: MergeEvidence,
    blocked: &[(CardId, QualityDebt)],
    notices: &[String],
) -> CheckpointRequest {
    let authors = evidence
        .packet
        .rows
        .iter()
        .filter_map(|row| row.owner.clone())
        .collect();
    let mut agent_outputs = evidence.agent_outputs;
    agent_outputs.extend(notices.iter().cloned());
    let options = merge_options(host, blocked);
    CheckpointRequest {
        at: Some(call.at.clone()),
        kind: CheckpointKind::Merge,
        title: title(host),
        card: None,
        packet: CheckpointPacket::Merge(evidence.packet),
        facts: evidence.facts,
        agent_outputs,
        documents: evidence.documents,
        options,
        free_text: FreeText::Guidance,
        authors,
    }
}

/// The merge card offers wait, a stronger review, and merge anyway while debt blocks.
fn merge_options(host: &Host, blocked: &[(CardId, QualityDebt)]) -> Option<Vec<AnswerOption>> {
    if blocked.is_empty() {
        return None;
    }
    let stronger = merge_rule::stronger_candidate(host, blocked);
    Some(options::merge_with_debt(stronger.as_ref()))
}

/// "Request changes on some cards": the integrated cards in the answer, or every one when it
/// names none.
fn requested(host: &Host, decided: &CheckpointDecided) -> Vec<CardId> {
    let integrated: Vec<CardId> = host.read(|state| {
        state
            .cards
            .by_id
            .values()
            .filter(|record| {
                record.state == CardState::Integrated && merge_packet::in_the_merge(record)
            })
            .map(|record| record.spec.id.clone())
            .collect()
    });
    if decided.cards.is_empty() {
        return integrated;
    }
    decided
        .cards
        .iter()
        .filter(|card| integrated.contains(card))
        .cloned()
        .collect()
}

/// Sends the integrated ones of `cards` back with `note` and runs them to integration again.
/// The rest stay as they are. Returns the cards sent back.
async fn rerun(
    host: &Host,
    call: &StepCall,
    cards: &[CardId],
    note: &str,
) -> EngineResult<Vec<CardId>> {
    let integrated: Vec<CardId> = host.read(|state| {
        cards
            .iter()
            .filter(|card| {
                state
                    .cards
                    .get(card)
                    .is_some_and(|record| record.state == CardState::Integrated)
            })
            .cloned()
            .collect()
    });
    let runs = integrated
        .iter()
        .map(|card| pipeline::send_back(host, card, &call.at, note));
    for result in join_all(runs).await {
        result?;
    }
    Ok(integrated)
}

/// Runs the cards whose original came back to integration again. Returns what happened, for
/// the merge card.
async fn rerun_returned(
    host: &Host,
    call: &StepCall,
    returned: &[(CardId, Rerun)],
) -> EngineResult<Vec<String>> {
    let runs = returned.iter().map(|(card, rerun)| async move {
        match rerun {
            Rerun::Changes => pipeline::send_back(host, card, &call.at, CHANGES_ASKED).await,
            Rerun::NewOwner { why } => pipeline::hand_back(host, card, &call.at, why).await,
            Rerun::TakeOver => pipeline::take_over_back(host, card, &call.at).await,
        }
    });
    for result in join_all(runs).await {
        result?;
    }
    Ok(returned
        .iter()
        .map(|(card, rerun)| match rerun {
            Rerun::Changes => format!("{CHANGES_ASKED} on {card}"),
            Rerun::NewOwner { why } => format!("{card}: {why}"),
            Rerun::TakeOver => format!("you took {card} over"),
        })
        .collect())
}

const CHANGES_ASKED: &str = "the original's review asked for changes";

/// Cards in the merge between `Running` and `Integrating`, or taken over from it. Build ends
/// with none, so these were sent back from the checkpoint before a restart.
fn in_flight(host: &Host) -> Vec<CardId> {
    host.read(|state| {
        state
            .cards
            .by_id
            .values()
            .filter(|record| {
                merge_packet::in_the_merge(record)
                    && matches!(
                        record.state,
                        CardState::Running
                            | CardState::Gating
                            | CardState::InReview
                            | CardState::Integrating
                            | CardState::TakenOver
                    )
            })
            .map(|record| record.spec.id.clone())
            .collect()
    })
}

async fn run_all(host: &Host, call: &StepCall, cards: &[CardId]) -> EngineResult<()> {
    let runs = cards
        .iter()
        .map(|card| pipeline::run_card_back(host, card, &call.at));
    for result in join_all(runs).await {
        result?;
    }
    Ok(())
}

fn sent_back_notices(what: &str, asked: &[CardId], sent: &[CardId]) -> Vec<String> {
    let kept: Vec<CardId> = asked
        .iter()
        .filter(|card| !sent.contains(card))
        .cloned()
        .collect();
    let mut notices = Vec::new();
    if !sent.is_empty() {
        notices.push(format!("{what} on {}", ids(sent)));
    }
    if !kept.is_empty() {
        notices.push(format!(
            "{what} on {}, which isn't in the integration branch, so it stays as it is",
            ids(&kept)
        ));
    }
    notices
}

/// Waits until every blocked card's original can work again.
async fn wait_for_originals(host: &Host, blocked: &[(CardId, QualityDebt)]) -> EngineResult<()> {
    let vendors: Vec<VendorId> = blocked
        .iter()
        .map(|(_, debt)| debt.needs.vendor())
        .collect();
    let mut events = host.services().health.subscribe();
    loop {
        if vendors
            .iter()
            .all(|vendor| rules::vendor_usable(host, vendor))
        {
            return Ok(());
        }
        tokio::select! {
            () = host.interrupted().cancelled() => return Err(EngineError::Interrupted),
            event = events.recv() => {
                if let Err(tokio::sync::broadcast::error::RecvError::Closed) = event {
                    return Err(EngineError::Refused(
                        "vendor health stopped reporting while the merge waited".to_owned(),
                    ));
                }
            }
        }
    }
}

/// Records the move into `phase` unless the log already has the run there, as it does when
/// a parked run wakes.
fn enter(host: &Host, phase_now: RunPhase) -> EngineResult<()> {
    if host.read(|state| state.run.phase) == Some(phase_now) {
        return Ok(());
    }
    phase(host, phase_now, PhaseStatus::Started, None)
}

fn phase(
    host: &Host,
    phase: RunPhase,
    status: PhaseStatus,
    summary: Option<&str>,
) -> EngineResult<()> {
    host.record(
        None,
        None,
        RunEventKind::Phase(PhaseChange {
            phase,
            status,
            summary: summary.map(str::to_owned),
        }),
    )?;
    Ok(())
}

fn finish(host: &Host, outcome: RunOutcome, summary: &str) -> EngineResult<StepDone> {
    phase(host, RunPhase::Merge, PhaseStatus::Finished, Some(summary))?;
    Ok(StepDone {
        end: StepEnd::Done,
        summary: Some(summary.to_owned()),
        flow: Flow::Finish(outcome),
    })
}

/// How the run already landed, if it did.
pub fn ended(integration: &IntegrationState) -> Option<RunOutcome> {
    if integration.landed.is_some() {
        Some(RunOutcome::Merged)
    } else if integration.pull_request.is_some() {
        Some(RunOutcome::PullRequest)
    } else if integration.abandoned.is_some() {
        Some(RunOutcome::Abandoned)
    } else {
        None
    }
}

fn outcome_summary(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Merged => "merged",
        RunOutcome::PullRequest => "pull request",
        RunOutcome::BranchLeft => "branch kept",
        RunOutcome::Abandoned => "abandoned, branches kept",
        RunOutcome::Stopped => "stopped",
        RunOutcome::Failed => "failed",
    }
}

fn card_list(blocked: &[(CardId, QualityDebt)]) -> String {
    let cards: Vec<CardId> = blocked.iter().map(|(card, _)| card.clone()).collect();
    ids(&cards)
}

fn ids(cards: &[CardId]) -> String {
    cards
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::integration::Landed;

    #[test]
    fn a_run_that_landed_ends_the_same_way_again() {
        let mut integration = IntegrationState::default();
        assert_eq!(ended(&integration), None);
        integration.abandoned = Some(Vec::new());
        assert_eq!(ended(&integration), Some(RunOutcome::Abandoned));
        integration.landed = Some(Landed {
            target: "main".to_owned(),
            sha: "h1".to_owned(),
            fast_forward: true,
        });
        assert_eq!(ended(&integration), Some(RunOutcome::Merged));
    }
}
