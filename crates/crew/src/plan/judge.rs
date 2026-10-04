//! The `judge` step in planning: spikes first when a proposal fits, the safety scan, scoring by
//! the step's `method` param (else `judging.method`) with judges who never see authors, the
//! aggregate in code, and decision records for the winner.

use std::collections::BTreeMap;

use futures::future::join_all;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::plan::{JudgingResult, PlanEvent, PlanTally};
use argonauts_core::names::{PlanLabel, RoleName, WorkerRef};
use argonauts_core::settings::protocol::JudgingMethod;
use argonauts_core::steps::plan::{Judge, JudgeInput, LabeledPlan, SpikeReport};
use argonauts_core::worker::Access;
use argonauts_core::workflow::{Step, StepWorkers};
use argonauts_core::{Note, NoteLevel};

use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::plan::context::{self, crew_for, families};
use crate::plan::judging::{self, Authors, Score};
use crate::plan::{decisions, drafts, scan, spikes};
use crate::prompts::plan::JUDGE;
use crate::state::plan::SealedDraft;
use crate::workflow::{StepCall, StepDone};

pub async fn run(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let (latest, judged, fast) = host.read(|state| {
        (
            state.plan.latest().into_iter().cloned().collect::<Vec<_>>(),
            state.plan.judged.clone(),
            state.plan.fast_path.is_some(),
        )
    });
    if fast {
        return Ok(StepDone::skipped("The fast path merged the drafts"));
    }
    if let Some(judged) = judged {
        return Ok(StepDone::done(summary(&judged)));
    }
    if latest.len() < 2 {
        return Ok(StepDone::skipped("Fewer than two plans to judge"));
    }
    let reports = spikes::run(host, call).await?;
    let plans = drafts::labeled(&latest.iter().collect::<Vec<_>>())?;
    let flags = scan_plans(host, &plans)?;
    let family = families(host);
    let authors: Authors = latest
        .iter()
        .map(|d| (d.label, (d.worker.clone(), family(&d.worker))))
        .collect();
    let mut settings = host.settings().judging.clone();
    if let Some(method) = method(&call.step).map_err(EngineError::Refused)? {
        settings.method = method;
    }
    let result = if settings.method == JudgingMethod::Human {
        JudgingResult {
            method: settings.method,
            plans: authors
                .iter()
                .map(|(label, (author, _))| PlanTally {
                    label: *label,
                    author: author.clone(),
                    mean: 0.0,
                    spread: 0.0,
                    counted: 0,
                    dropped: 0,
                })
                .collect(),
            winner: None,
            margin: 0.0,
            near_tie: false,
        }
    } else {
        let judges = judges(host, call, &latest, settings.method);
        let own = scores_own_plans(settings.method, &judges);
        let panels = panels(&judges, &plans, &authors, &family, own);
        score(host, call, &panels, &authors, &flags, &reports).await?;
        let scores = recorded_scores(host);
        match settings.method {
            JudgingMethod::Vote => judging::vote(&scores, &authors, &settings),
            _ => judging::aggregate(&scores, &authors, &settings),
        }
    };
    host.record(
        None,
        None,
        RunEventKind::Plan(PlanEvent::Judged(result.clone())),
    )?;
    if let Some(winner) = result.winner
        && let Some(chosen) = latest.iter().find(|d| d.label == winner)
    {
        decisions::record_for(host, chosen).await?;
    }
    let summary = summary(&result);
    context::phase_finished(host, &summary)?;
    Ok(StepDone::done(summary))
}

/// The param a `judge` step names its method with, such as `method: vote`.
pub const METHOD: &str = "method";

/// The method the step names, if it names one. A workflow that says how it judges is judged
/// that way whatever `judging.method` says.
pub fn method(step: &Step) -> Result<Option<JudgingMethod>, String> {
    let Some(value) = step.params.0.get(METHOD) else {
        return Ok(None);
    };
    serde_json::from_value(value.clone()).map(Some).map_err(|_| {
        format!("`{METHOD}: {value}` isn't a judging method; use cross, single, vote, coordinator, or human")
    })
}

fn summary(result: &JudgingResult) -> String {
    match (result.method, result.winner) {
        (JudgingMethod::Human, _) => "You pick the plan at the plan checkpoint".to_owned(),
        (_, Some(winner)) => format!("Plan {winner} won by {:.1}", result.margin),
        (_, None) => "A near tie; you pick at the plan checkpoint".to_owned(),
    }
}

/// The safety scan: plans holding text addressed to judges are flagged, and a note says so.
fn scan_plans(host: &Host, plans: &[LabeledPlan]) -> EngineResult<BTreeMap<PlanLabel, bool>> {
    let mut flags = BTreeMap::new();
    for plan in plans {
        let found = scan::addressed_to_judges(&plan.text);
        if !found.is_empty() {
            host.record(
                None,
                None,
                RunEventKind::Note(Note {
                    level: NoteLevel::Warn,
                    text: format!(
                        "Plan {} speaks to its judges ({}); it can't be auto-approved",
                        plan.label,
                        found.join(", ")
                    ),
                }),
            )?;
        }
        flags.insert(plan.label, !found.is_empty());
    }
    Ok(flags)
}

/// Who judges, by method. Cross: every drafter, plus a reserved seat from a family that wrote
/// no draft when set, plus judges from other families for any plan left with fewer than two.
fn judges(
    host: &Host,
    call: &StepCall,
    latest: &[SealedDraft],
    method: JudgingMethod,
) -> Vec<WorkerRef> {
    if let StepWorkers::Named(named) = &call.step.workers
        && !named.is_empty()
    {
        return named.clone();
    }
    let settings = host.settings();
    let family = families(host);
    let mut drafters: Vec<WorkerRef> = Vec::new();
    for draft in latest {
        if !drafters.contains(&draft.worker) {
            drafters.push(draft.worker.clone());
        }
    }
    let drafting_families: Vec<String> = drafters.iter().map(&family).collect();
    let pool: Vec<WorkerRef> = crew_for(settings, RoleName::Judge);
    let outsiders: Vec<WorkerRef> = pool
        .iter()
        .filter(|w| !drafters.contains(w) && !drafting_families.contains(&family(w)))
        .cloned()
        .collect();
    match method {
        JudgingMethod::Single => pool
            .iter()
            .find(|w| !drafters.contains(w))
            .or(outsiders.first())
            .or(drafters.first())
            .cloned()
            .into_iter()
            .collect(),
        JudgingMethod::Coordinator => crew_for(settings, RoleName::Coordinator)
            .into_iter()
            .next()
            .or_else(|| Some(settings.default_agent.clone()))
            .into_iter()
            .collect(),
        JudgingMethod::Cross | JudgingMethod::Vote | JudgingMethod::Human => {
            let mut judges = drafters.clone();
            if settings.judging.reserve_non_drafting_seat
                && let Some(seat) = outsiders.first()
            {
                judges.push(seat.clone());
            }
            if method == JudgingMethod::Cross {
                for draft in latest {
                    let author_family = family(&draft.worker);
                    let eligible = |judges: &[WorkerRef]| {
                        judges
                            .iter()
                            .filter(|j| **j != draft.worker && family(j) != author_family)
                            .count()
                    };
                    while eligible(&judges) < 2 {
                        let Some(extra) = pool
                            .iter()
                            .find(|w| !judges.contains(w) && family(w) != author_family)
                        else {
                            break;
                        };
                        judges.push(extra.clone());
                    }
                }
            }
            judges
        }
    }
}

/// Whether judges also score their own family's plans. With two judges the choice stands only
/// if both rank the same plan first, and two drafters shown only each other's plan couldn't
/// rank anything. Those scores never enter a mean. A voter never votes for its own plan, so
/// `vote` never shows one.
fn scores_own_plans(method: JudgingMethod, judges: &[WorkerRef]) -> bool {
    method != JudgingMethod::Vote && judges.len() == 2
}

/// A judge, its family, and the plans it scores.
type Panel = (WorkerRef, String, Vec<LabeledPlan>);

/// The plans each judge scores: never its own or its own family's, unless `own`.
fn panels(
    judges: &[WorkerRef],
    plans: &[LabeledPlan],
    authors: &Authors,
    family: &dyn Fn(&WorkerRef) -> String,
    own: bool,
) -> Vec<Panel> {
    judges
        .iter()
        .map(|judge| {
            let judge_family = family(judge);
            let shown = plans
                .iter()
                .filter(|plan| {
                    authors
                        .get(&plan.label)
                        .is_some_and(|(author, f)| own || (author != judge && *f != judge_family))
                })
                .cloned()
                .collect();
            (judge.clone(), judge_family, shown)
        })
        .collect()
}

/// Each judge scores every plan it may see, in both orders when `both_orders`. A score already
/// recorded for this round is not asked again. A stand-in judges in a judge's place only from
/// outside the companies of the plans it's shown, and its scores go under its own name.
async fn score(
    host: &Host,
    call: &StepCall,
    panels: &[Panel],
    authors: &Authors,
    flags: &BTreeMap<PlanLabel, bool>,
    reports: &[SpikeReport],
) -> EngineResult<()> {
    let goal = host.read(context::goal_brief);
    let orders: &[u8] = if host.settings().judging.both_orders {
        &[0, 1]
    } else {
        &[0]
    };
    // Each by the judge asked, so a stand-in's scores count for the seat it took.
    let done: Vec<(WorkerRef, u8)> = host.read(|state| {
        let round = state.plan.current_round();
        state
            .plan
            .scores
            .iter()
            .filter(|s| s.round == round)
            .map(|s| {
                let asked = s
                    .session
                    .as_ref()
                    .and_then(|key| state.sessions.get(key))
                    .map_or(&s.judge, |session| session.asked_worker());
                (asked.clone(), s.order)
            })
            .collect()
    });
    let family = families(host);
    let mut asks = Vec::new();
    for (judge, _, shown) in panels {
        let mut shown = shown.clone();
        if shown.is_empty() {
            continue;
        }
        for order in orders {
            if done.contains(&(judge.clone(), *order)) {
                continue;
            }
            if *order == 1 {
                shown.reverse();
            }
            let input = JudgeInput {
                goal: goal.clone(),
                rubric: judging::rubric(),
                plans: shown.clone(),
                spikes: reports.to_vec(),
            };
            let mut ask = Ask::new(
                Some(call.at.clone()),
                RoleName::Judge,
                judge.clone(),
                JUDGE,
                input,
            );
            ask.access = Access::NoTools;
            ask.blind = true;
            ask.slot = u32::from(*order);
            let labels: Vec<PlanLabel> = shown.iter().map(|p| p.label).collect();
            ask.independent_of = labels
                .iter()
                .filter_map(|label| authors.get(label).map(|(author, _)| author.clone()))
                .collect();
            asks.push(async move {
                let answer = host.ask::<Judge>(ask).await;
                (judge, *order, labels, answer)
            });
        }
    }
    for (judge, order, labels, answer) in join_all(asks).await {
        let answered = match answer {
            Ok(answered) => answered,
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(error) => {
                host.record(
                    None,
                    None,
                    RunEventKind::Note(Note {
                        level: NoteLevel::Warn,
                        text: format!("{judge}'s scores are dropped: {error}"),
                    }),
                )?;
                continue;
            }
        };
        let mut seen = Vec::new();
        for plan_score in answered.output.scores {
            if !labels.contains(&plan_score.plan) || seen.contains(&plan_score.plan) {
                continue;
            }
            seen.push(plan_score.plan);
            let flagged =
                plan_score.addressed_to_judges || flags.get(&plan_score.plan) == Some(&true);
            host.record(
                None,
                Some(&answered.session),
                RunEventKind::Plan(PlanEvent::Scored {
                    judge: answered.worker.clone(),
                    judge_family: family(&answered.worker),
                    plan: plan_score.plan,
                    order,
                    scores: plan_score.criteria,
                    rationale: plan_score.rationale,
                    flagged,
                }),
            )?;
        }
    }
    Ok(())
}

fn recorded_scores(host: &Host) -> Vec<Score> {
    host.read(|state| {
        let round = state.plan.current_round();
        state
            .plan
            .scores
            .iter()
            .filter(|s| s.round == round)
            .map(|s| Score {
                judge: s.judge.clone(),
                judge_family: s.judge_family.clone(),
                plan: s.plan,
                order: s.order,
                criteria: s.scores.clone(),
            })
            .collect()
    })
}
