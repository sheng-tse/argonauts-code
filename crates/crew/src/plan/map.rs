//! The quote check: a mapping agent's quote must appear word for word in its draft once runs of
//! whitespace are collapsed. Failing positions are marked, the mapping agent runs once more, and
//! topics still failing are flagged at the plan approval.

use argonauts_core::events::RunEventKind;
use argonauts_core::events::plan::{MappedPosition, MappedTopic, PlanEvent};
use argonauts_core::events::session::InputPartKind;
use argonauts_core::names::{RoleName, SessionKey};
use argonauts_core::session_runner::Access;
use argonauts_core::steps::plan::{LabeledPlan, Map, MapInput, MapOutput, MapPosition, MapTopic};
use argonauts_core::{Note, NoteLevel};

use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::plan::context::{part, workers_for};
use crate::plan::drafts;
use crate::prompts::plan::MAP;
use crate::workflow::{StepCall, StepDone};

/// Whether `quote` appears in `draft` after collapsing runs of whitespace in both.
pub fn quote_found(quote: &str, draft: &str) -> bool {
    let quote = collapse(quote);
    // An empty quote proves nothing about the draft.
    !quote.is_empty() && collapse(draft).contains(&quote)
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The mapping agent's topics with every quote checked against its plan's sealed text. A position
/// naming a plan that isn't there fails too.
pub fn check_quotes(output: &MapOutput, plans: &[LabeledPlan]) -> Vec<MappedTopic> {
    output
        .topics
        .iter()
        .map(|topic| MappedTopic {
            name: topic.name.clone(),
            disputed: topic.disputed,
            positions: topic
                .positions
                .iter()
                .map(|position| MappedPosition {
                    label: position.label,
                    summary: position.summary.clone(),
                    quote: position.quote.clone(),
                    quote_verified: plans
                        .iter()
                        .find(|plan| plan.label == position.label)
                        .is_some_and(|plan| quote_found(&position.quote, &plan.text)),
                })
                .collect(),
        })
        .collect()
}

/// One line per failing quote, for the mapping agent's second run.
pub fn failures(topics: &[MappedTopic]) -> Vec<String> {
    topics
        .iter()
        .flat_map(|topic| {
            topic
                .positions
                .iter()
                .filter(|position| !position.quote_verified)
                .map(move |position| {
                    format!(
                        "Topic \"{}\", Plan {}: the quote \"{}\" is not in the plan word for word.",
                        topic.name, position.label, position.quote
                    )
                })
        })
        .collect()
}

/// Back to the step shape, for critics and the experiment proposer.
pub fn to_step_topic(topic: &MappedTopic) -> MapTopic {
    MapTopic {
        name: topic.name.clone(),
        disputed: topic.disputed,
        positions: topic
            .positions
            .iter()
            .map(|position| MapPosition {
                label: position.label,
                summary: position.summary.clone(),
                quote: position.quote.clone(),
            })
            .collect(),
    }
}

/// `map`: a fresh mapping agent over the newest sealed drafts under their labels. Failing quotes
/// get the mapping agent once more with each failure named; what still fails is recorded
/// unverified.
pub async fn run(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    let (latest, round, first, mapped) = host.read(|state| {
        let round = state.plan.current_round();
        (
            state.plan.latest().into_iter().cloned().collect::<Vec<_>>(),
            round,
            state.plan.blind_round(false),
            state.plan.maps.iter().find(|m| m.round == round).cloned(),
        )
    });
    if let Some(map) = mapped {
        return Ok(StepDone::done(summary(&map.topics)));
    }
    if latest.len() < 2 {
        return Ok(StepDone::skipped("Fewer than two plans to map"));
    }
    let settings = host.settings();
    if round > first && !settings.mapping.remap_after_revision {
        return Ok(StepDone::skipped("Remapping after revision is off"));
    }
    let Some(mapping_agent) = workers_for(call, settings, RoleName::Map, Some(1))
        .into_iter()
        .next()
    else {
        return Ok(StepDone::skipped("No mapping agent is available"));
    };
    let plans = drafts::read_by(
        host.run_id(),
        &format!("map {mapping_agent}"),
        &drafts::labeled_for_agents(host, &latest.iter().collect::<Vec<_>>())?,
    );
    let mut mapped: Option<(Vec<MappedTopic>, SessionKey)> = None;
    let mut feedback: Vec<String> = Vec::new();
    for slot in 0..2u32 {
        let mut ask = Ask::new(
            Some(call.at.clone()),
            RoleName::Map,
            mapping_agent.clone(),
            MAP,
            MapInput {
                plans: plans.clone(),
            },
        );
        ask.access = Access::NoTools;
        ask.blind = true;
        ask.slot = slot;
        if !feedback.is_empty() {
            ask.parts.push(part(
                InputPartKind::Message,
                "quote check",
                feedback.join("\n"),
            ));
        }
        match host.ask::<Map>(ask).await {
            Ok(answered) => {
                let topics = check_quotes(&answered.output, &plans);
                feedback = failures(&topics);
                mapped = Some((topics, answered.session));
                if feedback.is_empty() || !settings.mapping.quote_check {
                    break;
                }
            }
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(error) if mapped.is_none() && slot == 0 => {
                feedback = vec![format!("Your last answer failed: {error}")];
            }
            Err(error) if mapped.is_none() => return Err(error),
            Err(error) => {
                host.record(
                    None,
                    None,
                    RunEventKind::Note(Note {
                        level: NoteLevel::Warn,
                        text: format!(
                            "The mapping agent's second run failed, so the first map stands: {error}"
                        ),
                    }),
                )?;
            }
        }
    }
    let Some((topics, session)) = mapped else {
        return Err(EngineError::Refused(
            "the mapping agent gave no map".to_owned(),
        ));
    };
    let summary = summary(&topics);
    host.record(
        None,
        Some(&session),
        RunEventKind::Plan(PlanEvent::Mapped {
            round,
            worker: mapping_agent,
            topics,
        }),
    )?;
    Ok(StepDone::done(summary))
}

fn summary(topics: &[MappedTopic]) -> String {
    let disputed = topics.iter().filter(|t| t.disputed).count();
    let unverified = topics
        .iter()
        .filter(|t| t.positions.iter().any(|p| !p.quote_verified))
        .count();
    let mut summary = format!("{} topics, {disputed} disputed", topics.len());
    if unverified > 0 {
        summary.push_str(&format!(", {unverified} with unverified quotes"));
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_match_across_whitespace_runs() {
        let draft = "Put the limiter\n   after auth,\tso anonymous\n\ncalls are cheap.";
        assert!(quote_found("after auth, so anonymous calls", draft));
        assert!(quote_found("  the limiter after\nauth ", draft));
    }

    #[test]
    fn a_changed_word_or_an_empty_quote_fails() {
        let draft = "Put the limiter after auth.";
        assert!(!quote_found("before auth", draft));
        assert!(!quote_found("the limiter after authentication", draft));
        assert!(!quote_found("   ", draft));
        assert!(!quote_found("", draft));
    }

    #[test]
    fn quotes_are_case_and_punctuation_exact() {
        let draft = "Use Redis for counters.";
        assert!(!quote_found("use redis", draft));
        assert!(!quote_found("Use Redis, for", draft));
    }

    #[test]
    fn check_quotes_marks_each_position() {
        let plans = vec![
            LabeledPlan {
                label: 1,
                text: "Keep counters in Redis.".to_owned(),
            },
            LabeledPlan {
                label: 2,
                text: "Keep counters in memory.".to_owned(),
            },
        ];
        let position = |label, quote: &str| MapPosition {
            label,
            summary: quote.to_owned(),
            quote: quote.to_owned(),
        };
        let output = MapOutput {
            topics: vec![MapTopic {
                name: "counters".to_owned(),
                disputed: true,
                positions: vec![
                    position(1, "counters in Redis"),
                    position(2, "counters in Redis"),
                    position(3, "counters"),
                ],
            }],
        };
        let topics = check_quotes(&output, &plans);
        let verified: Vec<bool> = topics[0]
            .positions
            .iter()
            .map(|p| p.quote_verified)
            .collect();
        assert_eq!(verified, vec![true, false, false]);
        assert_eq!(failures(&topics).len(), 2);
        assert_eq!(to_step_topic(&topics[0]).positions.len(), 3);
    }
}
