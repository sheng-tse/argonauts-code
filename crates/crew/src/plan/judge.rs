//! The `judge` step in planning: experiments first when a proposal fits, the safety scan, scoring
//! by the step's `method` param (else `judging.method`) with judges who never see authors, the
//! aggregate in code, and decision records for the winner.

use std::collections::BTreeMap;

use futures::future::join_all;

use argonauts_core::RunId;
use argonauts_core::agents::session_runner::Access;
use argonauts_core::base::names::{ModelRef, PlanLabel, RoleName};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::plan::{JudgingResult, PlanEvent, PlanTally};
use argonauts_core::settings::protocol::JudgingMethod;
use argonauts_core::workflows::steps::plan::{ExperimentReport, Judge, JudgeInput, LabeledPlan};
use argonauts_core::workflows::workflow::{Step, StepModels};
use argonauts_core::{Note, NoteLevel};

use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::{Ask, Host};
use crate::plan::context::{self, crew_for, providers};
use crate::plan::judging::{self, Authors, Score};
use crate::plan::{decisions, drafts, experiments, scan};
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
    let reports = experiments::run(host, call).await?;
    let plans = drafts::labeled_for_agents(host, &latest.iter().collect::<Vec<_>>())?;
    let flags = scan_plans(host, &plans)?;
    let provider = providers(host);
    let authors: Authors = latest
        .iter()
        .map(|d| (d.label, (d.worker.clone(), provider(&d.worker))))
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
        let panels = panels(&judges, &plans, &authors, &provider, own);
        score(host, call, &panels, &authors, &flags, &reports).await?;
        let scores = recorded_scores(host);
        let criteria = rubric_of(host).len();
        match settings.method {
            JudgingMethod::Vote => judging::vote_on(&scores, &authors, &settings, criteria),
            _ => judging::aggregate_on(&scores, &authors, &settings, criteria),
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
        format!("`{METHOD}: {value}` isn't a judging method; use cross, single, vote, orchestrator, or human")
    })
}

fn summary(result: &JudgingResult) -> String {
    match (result.method, result.winner) {
        (JudgingMethod::Human, _) => "You pick the plan at the plan approval".to_owned(),
        (_, Some(winner)) => format!("Plan {winner} won by {:.1}", result.margin),
        (_, None) => "A near tie; you pick at the plan approval".to_owned(),
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

/// Who judges, by method. Cross: every drafting agent, plus a reserved judge from a provider that
/// wrote no draft when set, plus judges from other providers for any plan left with fewer than two.
fn judges(
    host: &Host,
    call: &StepCall,
    latest: &[SealedDraft],
    method: JudgingMethod,
) -> Vec<ModelRef> {
    if let StepModels::Named(named) = &call.step.models
        && !named.is_empty()
    {
        return named.clone();
    }
    let settings = host.settings();
    let provider = providers(host);
    let mut drafting_agents: Vec<ModelRef> = Vec::new();
    for draft in latest {
        if !drafting_agents.contains(&draft.worker) {
            drafting_agents.push(draft.worker.clone());
        }
    }
    let drafting_providers: Vec<String> = drafting_agents.iter().map(&provider).collect();
    let pool: Vec<ModelRef> = crew_for(settings, RoleName::Judge);
    let outsiders: Vec<ModelRef> = pool
        .iter()
        .filter(|w| !drafting_agents.contains(w) && !drafting_providers.contains(&provider(w)))
        .cloned()
        .collect();
    match method {
        JudgingMethod::Single => pool
            .iter()
            .find(|w| !drafting_agents.contains(w))
            .or(outsiders.first())
            .or(drafting_agents.first())
            .cloned()
            .into_iter()
            .collect(),
        JudgingMethod::Orchestrator => crew_for(settings, RoleName::Orchestrator)
            .into_iter()
            .next()
            .or_else(|| Some(settings.crew.main.clone()))
            .into_iter()
            .collect(),
        JudgingMethod::Cross | JudgingMethod::Vote | JudgingMethod::Human => {
            let mut judges = drafting_agents.clone();
            if settings.judging.reserve_non_drafting_judge
                && let Some(outsider) = outsiders.first()
            {
                judges.push(outsider.clone());
            }
            if method == JudgingMethod::Cross {
                for draft in latest {
                    let author_provider = provider(&draft.worker);
                    let eligible = |judges: &[ModelRef]| {
                        judges
                            .iter()
                            .filter(|j| **j != draft.worker && provider(j) != author_provider)
                            .count()
                    };
                    while eligible(&judges) < 2 {
                        let Some(extra) = pool
                            .iter()
                            .find(|w| !judges.contains(w) && provider(w) != author_provider)
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

/// Whether judges also score their own provider's plans. With two judges the choice stands only
/// if both rank the same plan first, and two drafting agents shown only each other's plan couldn't
/// rank anything. Those scores never enter a mean. A voter never votes for its own plan, so
/// `vote` never shows one.
fn scores_own_plans(method: JudgingMethod, judges: &[ModelRef]) -> bool {
    method != JudgingMethod::Vote && judges.len() == 2
}

/// A judge, its provider, and the plans it scores.
type Panel = (ModelRef, String, Vec<LabeledPlan>);

/// The plans each judge scores: never its own or its own provider's, unless `own`.
fn panels(
    judges: &[ModelRef],
    plans: &[LabeledPlan],
    authors: &Authors,
    provider: &dyn Fn(&ModelRef) -> String,
    own: bool,
) -> Vec<Panel> {
    judges
        .iter()
        .map(|judge| {
            let judge_provider = provider(judge);
            let shown = plans
                .iter()
                .filter(|plan| {
                    authors
                        .get(&plan.label)
                        .is_some_and(|(author, f)| own || (author != judge && *f != judge_provider))
                })
                .cloned()
                .collect();
            (judge.clone(), judge_provider, shown)
        })
        .collect()
}

/// Each judge scores every plan it may see, in both orders when `both_orders`. A score already
/// recorded for this round is not asked again. A fallback judges in a judge's place only from
/// outside the providers of the plans it's shown, and its scores go under its own name.
async fn score(
    host: &Host,
    call: &StepCall,
    panels: &[Panel],
    authors: &Authors,
    flags: &BTreeMap<PlanLabel, bool>,
    reports: &[ExperimentReport],
) -> EngineResult<()> {
    let goal = host.read(context::goal_brief);
    let orders: &[u8] = if host.settings().judging.both_orders {
        &[0, 1]
    } else {
        &[0]
    };
    // Each by the judge asked, so a fallback's scores count for the judge whose place it took.
    let done: Vec<(ModelRef, u8)> = host.read(|state| {
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
    let provider = providers(host);
    let mut asks = Vec::new();
    for (judge, _, shown) in panels {
        if shown.is_empty() {
            continue;
        }
        let drawn = order_for(host.run_id(), judge, shown);
        for order in orders {
            if done.contains(&(judge.clone(), *order)) {
                continue;
            }
            let mut plans = drawn.clone();
            if *order == 1 {
                plans.reverse();
            }
            let input = JudgeInput {
                goal: goal.clone(),
                rubric: rubric_of(host),
                plans,
                experiments: reports.to_vec(),
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
                    judge_provider: provider(&answered.worker),
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

/// The plans `judge` reads, in an order drawn for it as a judge.
fn order_for(run: &RunId, judge: &ModelRef, shown: &[LabeledPlan]) -> Vec<LabeledPlan> {
    drafts::read_by(run, &format!("judge {judge}"), shown)
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
                judge_provider: s.judge_provider.clone(),
                plan: s.plan,
                order: s.order,
                criteria: s.scores.clone(),
            })
            .collect()
    })
}

/// The rubric the judges score by: your `prompts/plan/rubric.md` when you wrote one, else the
/// built-in.
fn rubric_of(host: &Host) -> Vec<argonauts_core::workflows::steps::plan::RubricCriterion> {
    judging::rubric_in(crate::prompts::files::text(
        host,
        crate::prompts::plan::RUBRIC,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn plans() -> Vec<LabeledPlan> {
        (1..=4)
            .map(|label| LabeledPlan {
                label,
                text: format!("Plan {label}'s approach."),
            })
            .collect()
    }

    #[test]
    fn each_judge_reads_the_plans_in_an_order_drawn_for_it() {
        let run = RunId::new("run-01").unwrap();
        let shown = plans();
        let judges: Vec<ModelRef> = [
            "codex/gpt-5.6",
            "antigravity-cli/gemini-3.8-flash",
            "claude-code/opus",
            "opencode/ollama/qwen3-coder:30b",
        ]
        .iter()
        .map(|judge| judge.parse().unwrap())
        .collect();
        let orders: BTreeSet<Vec<PlanLabel>> = judges
            .iter()
            .map(|judge| {
                let drawn = order_for(&run, judge, &shown);
                assert_eq!(drawn.len(), shown.len());
                assert_eq!(
                    order_for(&run, judge, &shown),
                    drawn,
                    "the same again on resume"
                );
                drawn.iter().map(|plan| plan.label).collect()
            })
            .collect();
        assert!(
            orders.len() > 1,
            "judges read the plans in different orders"
        );
        let other_run = RunId::new("run-02").unwrap();
        assert!(
            judges
                .iter()
                .any(|judge| order_for(&other_run, judge, &shown) != order_for(&run, judge, &shown)),
            "another run, other orders"
        );
    }
}
