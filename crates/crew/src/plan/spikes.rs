//! Spikes: the phase lead proposes; code checks each proposal against `experiments.limit_usd` and
//! `time_box_min`. A fitting one becomes spike cards, one per option, in sandboxed worktrees,
//! ending with a `SpikeReport`. Spike worktrees are never integrated.

use std::path::PathBuf;

use futures::future::join_all;

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardBudget, CardEvent, CardKind, CardSpec, CardState};
use argonauts_core::events::plan::PlanEvent;
use argonauts_core::events::session::InputPartKind;
use argonauts_core::names::{RoleName, SessionKey, WorkerRef};
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
use argonauts_core::settings::protocol::SpikeSettings;
use argonauts_core::steps::plan::{
    ProposeSpike, ProposeSpikeInput, SpikeProposal, SpikeReport, SpikeReportStep,
};
use argonauts_core::steps::{StepName, last_json_object, output_schema};
use argonauts_core::worker::Access;

use crate::card::worktree;
use crate::engine::host::{Ask, Host, SessionPlan, StandIns};
use crate::error::{EngineError, EngineResult};
use crate::lead;
use crate::plan::context::{crew_for, part, warn};
use crate::plan::map;
use crate::prompts::plan::{PROPOSE_SPIKE, SPIKE_REPORT};
use crate::state::RunState;
use crate::workflow::StepCall;

/// Whether one proposal fits on its own: spikes on, at least one option, each option within
/// the time box, and the whole proposal within the run's spike budget.
pub fn fits(proposal: &SpikeProposal, settings: &SpikeSettings) -> bool {
    fits_within(proposal, settings, 0.0)
}

fn fits_within(proposal: &SpikeProposal, settings: &SpikeSettings, spent_usd: f64) -> bool {
    let estimates_sane = proposal.est_minutes.is_finite()
        && proposal.est_cost_usd.is_finite()
        && proposal.est_minutes > 0.0
        && proposal.est_cost_usd >= 0.0;
    settings.enabled
        && !proposal.options.is_empty()
        && estimates_sane
        && proposal.est_minutes <= settings.time_box_min
        && spent_usd + proposal.est_cost_usd <= settings.budget_usd
}

/// Which proposals fit, in order, each counting against what the earlier fitting ones took
/// from the run's spike budget.
pub fn within_budget(proposals: &[SpikeProposal], settings: &SpikeSettings) -> Vec<bool> {
    let mut spent = 0.0;
    proposals
        .iter()
        .map(|proposal| {
            let fits = fits_within(proposal, settings, spent);
            if fits {
                spent += proposal.est_cost_usd;
            }
            fits
        })
        .collect()
}

/// Dollar limits for the spike cards, one per option in card order. A proposal's estimate
/// covers all its options, so each option gets an even share, and no card gets more than the
/// earlier cards left of the run's spike budget.
fn card_limits_usd(proposals: &[SpikeProposal], budget_usd: f64) -> Vec<f64> {
    let mut left = budget_usd.max(0.0);
    let mut limits = Vec::new();
    for proposal in proposals {
        let share = proposal.est_cost_usd / proposal.options.len().max(1) as f64;
        for _ in &proposal.options {
            let limit = share.min(left).max(0.0);
            left -= limit;
            limits.push(limit);
        }
    }
    limits
}

/// Proposes spikes once per run when topics are still disputed, runs every fitting one as
/// spike cards (one per option), and returns every spike report the run has.
pub async fn run(host: &Host, call: &StepCall) -> EngineResult<Vec<SpikeReport>> {
    let settings = host.settings().experiments.clone();
    let (proposed, disputed) = host.read(|state| {
        (
            state.plan.spikes.clone(),
            state
                .plan
                .disputed()
                .into_iter()
                .map(map::to_step_topic)
                .collect::<Vec<_>>(),
        )
    });
    if settings.enabled && !disputed.is_empty() {
        if proposed.is_empty() {
            propose(host, call, &settings, disputed).await?;
        }
        build(host, call, &settings).await?;
    }
    Ok(host.read(|state| {
        state
            .plan
            .spike_reports
            .iter()
            .map(|result| result.report.clone())
            .collect()
    }))
}

/// The phase lead proposes; code checks each proposal against the budget and records it.
async fn propose(
    host: &Host,
    call: &StepCall,
    settings: &SpikeSettings,
    disputed: Vec<argonauts_core::steps::plan::MapTopic>,
) -> EngineResult<()> {
    let lead = host
        .read(|state| lead::phase_lead(state, host.settings()))
        .unwrap_or_else(|| host.settings().main_agent.clone());
    let input = ProposeSpikeInput {
        disputed,
        budget_usd: settings.budget_usd,
        time_box_min: settings.time_box_min,
    };
    let mut ask = Ask::new(
        Some(call.at.clone()),
        RoleName::PhaseLead,
        lead,
        PROPOSE_SPIKE,
        input,
    );
    ask.access = Access::NoTools;
    ask.blind = true;
    let proposals = match host.ask::<ProposeSpike>(ask).await {
        Ok(answered) => answered.output.proposals,
        Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
        Err(error) => {
            return warn(
                host,
                None,
                format!("No experiments: the proposal failed: {error}"),
            );
        }
    };
    for (proposal, fits_budget) in proposals.iter().zip(within_budget(&proposals, settings)) {
        host.record(
            None,
            None,
            RunEventKind::Plan(PlanEvent::SpikeProposed {
                proposal: proposal.clone(),
                fits_budget,
            }),
        )?;
    }
    Ok(())
}

/// One spike card per option of every fitting proposal, numbered in proposal order so a
/// re-entered step finds the cards it already made. Worktrees are made one at a time, since
/// they share the repository's refs; the spikes then run side by side.
async fn build(host: &Host, call: &StepCall, settings: &SpikeSettings) -> EngineResult<()> {
    let proposals: Vec<SpikeProposal> = host.read(|state| {
        state
            .plan
            .spikes
            .iter()
            .filter(|record| record.fits_budget)
            .map(|record| record.proposal.clone())
            .collect()
    });
    let builders: Vec<WorkerRef> = crew_for(host.settings(), RoleName::Spike);
    if builders.is_empty() {
        return warn(
            host,
            None,
            "No experiment worker is available; the disputes stay open".to_owned(),
        );
    }
    let mut limits = card_limits_usd(&proposals, settings.budget_usd).into_iter();
    let mut ready = Vec::new();
    let mut n = 0u32;
    for proposal in &proposals {
        for (index, option) in proposal.options.iter().enumerate() {
            n += 1;
            let card = CardId::new(format!("spike-{n:03}"))
                .map_err(|error| EngineError::Refused(error.to_string()))?;
            let worker = builders[index % builders.len()].clone();
            // One option's experiment carries that option's share of the estimate, which is
            // also its card's limit.
            let experiment = SpikeProposal {
                options: vec![option.clone()],
                est_cost_usd: limits.next().unwrap_or(0.0),
                ..proposal.clone()
            };
            match prepare(host, settings, &card, &worker, &experiment).await {
                Ok(Some(workdir)) => ready.push((card, worker, experiment, workdir)),
                Ok(None) => {}
                Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
                Err(error) => warn(
                    host,
                    Some(&card),
                    format!("{card} couldn't start; its dispute stays open: {error}"),
                )?,
            }
        }
    }
    let runs = ready
        .into_iter()
        .map(|(card, worker, experiment, workdir)| async move {
            let outcome = spike(host, call, &card, worker, &experiment, workdir).await;
            (card, outcome)
        });
    for (card, outcome) in join_all(runs).await {
        match outcome {
            Ok(()) => {}
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(error) => warn(
                host,
                Some(&card),
                format!("{card} failed; its dispute stays open: {error}"),
            )?,
        }
    }
    Ok(())
}

/// Creates the spike card and its worktree, or finds them. `None` when it already reported.
async fn prepare(
    host: &Host,
    settings: &SpikeSettings,
    card: &CardId,
    worker: &WorkerRef,
    experiment: &SpikeProposal,
) -> EngineResult<Option<PathBuf>> {
    let (created, finished) = host.read(|state| {
        (
            state.cards.get(card).is_some(),
            state.plan.spike_reports.iter().any(|r| &r.card == card),
        )
    });
    if finished {
        return Ok(None);
    }
    let Some(option) = experiment.options.first() else {
        return Ok(None);
    };
    if !created {
        let spec = CardSpec {
            id: card.clone(),
            title: format!("Experiment: {}: {}", experiment.topic, option.label),
            task: format!(
                "{}\n\nBuild: {}\nMeasure: {}\nDecides by: {}",
                experiment.question, option.build, experiment.measure, experiment.decides_by
            ),
            kind: CardKind::Spike {
                topic: experiment.topic.clone(),
                option: option.label.clone(),
            },
            owned_paths: Vec::new(),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            owner: Some(worker.clone()),
            reviewers: Vec::new(),
            budget: CardBudget {
                limit: Some(BudgetAmount {
                    unit: BudgetUnit::Usd,
                    amount: experiment.est_cost_usd,
                }),
                max_minutes: Some(settings.time_box_min),
                ..CardBudget::default()
            },
            predictions: None,
            item: Some(argonauts_core::labels::slug(&option.label, 3))
                .filter(|item| !item.is_empty()),
        };
        host.record(
            Some(card),
            None,
            RunEventKind::Card(CardEvent::Created(Box::new(spec))),
        )?;
        host.record(
            Some(card),
            None,
            RunEventKind::Card(CardEvent::State {
                from: None,
                to: CardState::Running,
                reason: "experiment started".to_owned(),
            }),
        )?;
    }
    Ok(Some(worktree::ensure(host, card).await?.path))
}

/// Runs one option's spike session, keeps its report, and closes the card unmerged.
async fn spike(
    host: &Host,
    call: &StepCall,
    card: &CardId,
    worker: WorkerRef,
    experiment: &SpikeProposal,
    workdir: PathBuf,
) -> EngineResult<()> {
    let plan = SessionPlan {
        at: Some(call.at.clone()),
        role: RoleName::Spike,
        worker,
        card: Some(card.clone()),
        access: Access::Spike,
        step: Some(StepName::SpikeReport),
        workdir,
        parts: vec![
            part(InputPartKind::Prompt, "spike", SPIKE_REPORT.to_owned()),
            part(
                InputPartKind::StepInput,
                "experiment",
                serde_json::to_string_pretty(experiment)?,
            ),
        ],
        json_schema: Some(output_schema::<SpikeReportStep>()),
        resume: None,
        slot: 0,
        label: None,
    };
    // A vendor error has the spike go again, or to another agent; when nobody can run it, the
    // dispute stays open, as without a report.
    let (session, end) = match host.run_session(plan, StandIns::Allowed).await {
        Ok(ran) => (ran.key, Some(ran.end)),
        Err(EngineError::Stalled(stall)) => {
            warn(host, Some(card), stall.line.clone())?;
            let session = host.read(|state| newest_spike_session(state, card));
            let Some(session) = session else {
                return Ok(());
            };
            (session, None)
        }
        Err(err) => return Err(err),
    };
    worktree::commit_after_end(host, card, &session).await?;
    let report = end.and_then(|end| {
        end.structured
            .clone()
            .or_else(|| end.result_text.as_deref().and_then(last_json_object))
            .and_then(|value| serde_json::from_value::<SpikeReport>(value).ok())
    });
    match report {
        Some(report) => {
            host.record(
                Some(card),
                Some(&session),
                RunEventKind::Plan(PlanEvent::SpikeFinished {
                    card: card.clone(),
                    report,
                }),
            )?;
        }
        None => warn(
            host,
            Some(card),
            format!("{card} gave no experiment report; the dispute stays open"),
        )?,
    }
    // Spike worktrees are never integrated; only the report is kept.
    host.record(
        Some(card),
        Some(&session),
        RunEventKind::Card(CardEvent::State {
            from: Some(CardState::Running),
            to: CardState::Cancelled,
            reason: "experiment done; experiments are never merged into the run".to_owned(),
        }),
    )?;
    Ok(())
}

/// The spike card's newest session.
fn newest_spike_session(state: &RunState, card: &CardId) -> Option<SessionKey> {
    state
        .sessions
        .by_key
        .values()
        .filter(|session| session.card.as_ref() == Some(card))
        .max_by_key(|session| session.since)
        .map(|session| session.key.clone())
}

#[cfg(test)]
mod tests {
    use argonauts_core::steps::plan::SpikeOption;

    use super::*;

    fn proposal(minutes: f64, cost: f64) -> SpikeProposal {
        SpikeProposal {
            topic: "counters".to_owned(),
            question: "Redis or memory?".to_owned(),
            options: vec![
                SpikeOption {
                    label: "Redis".to_owned(),
                    build: "a counter in Redis".to_owned(),
                },
                SpikeOption {
                    label: "memory".to_owned(),
                    build: "a counter in a map".to_owned(),
                },
            ],
            measure: "p50 latency".to_owned(),
            decides_by: "lower p50 wins".to_owned(),
            est_minutes: minutes,
            est_cost_usd: cost,
        }
    }

    #[test]
    fn a_proposal_fits_the_time_box_and_budget() {
        let settings = SpikeSettings::default();
        assert!(fits(&proposal(10.0, 1.0), &settings));
        assert!(fits(&proposal(15.0, 2.0), &settings));
        assert!(!fits(&proposal(16.0, 1.0), &settings));
        assert!(!fits(&proposal(10.0, 2.5), &settings));
    }

    #[test]
    fn nonsense_estimates_or_no_options_never_fit() {
        let settings = SpikeSettings::default();
        assert!(!fits(&proposal(f64::NAN, 1.0), &settings));
        assert!(!fits(&proposal(10.0, -1.0), &settings));
        assert!(!fits(&proposal(0.0, 1.0), &settings));
        let mut empty = proposal(10.0, 1.0);
        empty.options.clear();
        assert!(!fits(&empty, &settings));
    }

    #[test]
    fn spikes_off_means_nothing_fits() {
        let settings = SpikeSettings {
            enabled: false,
            ..SpikeSettings::default()
        };
        assert!(!fits(&proposal(1.0, 0.1), &settings));
    }

    #[test]
    fn spike_card_limits_add_up_to_no_more_than_the_budget() {
        let settings = SpikeSettings {
            budget_usd: 2.5,
            ..SpikeSettings::default()
        };
        let mut three = proposal(10.0, 2.0);
        three.options.push(SpikeOption {
            label: "sqlite".to_owned(),
            build: "a counter in sqlite".to_owned(),
        });
        let proposals = [three, proposal(10.0, 1.5), proposal(5.0, 0.5)];
        let fitting: Vec<SpikeProposal> = proposals
            .iter()
            .zip(within_budget(&proposals, &settings))
            .filter(|(_, fits)| *fits)
            .map(|(proposal, _)| proposal.clone())
            .collect();
        let limits = card_limits_usd(&fitting, settings.budget_usd);
        // Three options sharing $2, then two sharing $0.50.
        let expected = [2.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0, 0.25, 0.25];
        assert_eq!(limits.len(), expected.len());
        for (limit, want) in limits.iter().zip(expected) {
            assert!((limit - want).abs() < 1e-9, "{limits:?}");
        }
        assert!(limits.iter().sum::<f64>() <= settings.budget_usd + 1e-9);

        // A smaller budget than the proposals were checked against still caps the cards.
        let limits = card_limits_usd(&fitting, 1.0);
        assert!(limits.iter().sum::<f64>() <= 1.0 + 1e-9, "{limits:?}");
        assert_eq!(limits.last(), Some(&0.0));
    }

    #[test]
    fn the_budget_is_shared_by_every_spike_in_the_run() {
        let settings = SpikeSettings::default();
        let proposals = [proposal(10.0, 1.5), proposal(10.0, 1.0), proposal(5.0, 0.5)];
        assert_eq!(
            within_budget(&proposals, &settings),
            vec![true, false, true]
        );
    }
}
