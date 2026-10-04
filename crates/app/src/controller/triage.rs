//! Reading a chat turn's reply. The triage estimate is an answer, a change, or unclear, and one
//! that fails the schema counts as unclear. `/ask` wants only an answer. A recipe whose `match`
//! fits the change decides solo or crew under `triage.use_recipes`, unless the request's own
//! words chose, once code checked them.

use argonauts_core::events::run::{RunMode, TriageDecision};
use argonauts_core::settings::Settings;
use argonauts_core::steps::intake::{
    ChangeEstimate, ChatAnswer, Instruction, IntakeReply, ReplyKind, TriageEstimate,
};
use argonauts_core::steps::{StepError, StepOutcome, decode, last_json_object};
use argonauts_learning::Store;

use crate::controller::instructions::Applied;
use crate::learning::recipe;

/// What the request said about how to run it, and what code made of it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Requested {
    /// As the estimate listed them, for a card's answer to check again.
    pub how_to_run: Vec<Instruction>,
    pub applied: Applied,
}

/// Checks the instructions a reply lists against your message and the agents here.
pub(crate) type CheckWords<'a> = &'a dyn Fn(&[Instruction]) -> Applied;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Reading {
    Answer {
        markdown: String,
    },
    Change {
        markdown: String,
        estimate: ChangeEstimate,
        triage: TriageDecision,
    },
    Unclear {
        markdown: String,
        estimate: Option<ChangeEstimate>,
        /// What triage would pick, said on the `StartWork` card.
        would_pick: Option<RunMode>,
    },
}

/// A step that ended without output at all is not a reading, so it comes back as the error.
#[cfg(test)]
pub(crate) fn read_reply(
    outcome: &Result<StepOutcome, StepError>,
    settings: &Settings,
) -> Result<Reading, StepError> {
    read_reply_with(outcome, settings, None, &|_| Applied::default()).map(|(reading, _)| reading)
}

/// [`read_reply`] with the repo's recipes, from `store`'s main checkout, and the request's own
/// words on how to run it, checked by `check`.
pub(crate) fn read_reply_with(
    outcome: &Result<StepOutcome, StepError>,
    settings: &Settings,
    store: Option<&Store>,
    check: CheckWords<'_>,
) -> Result<(Reading, Requested), StepError> {
    let reply = match outcome {
        Ok(outcome) => decode::<TriageEstimate>(outcome),
        Err(err) => Err(err.clone()),
    };
    match reply {
        Ok(reply) => Ok(reading(reply, settings, store, check)),
        Err(StepError::SchemaMismatch { raw, errors }) => match answer_text(raw.as_deref()) {
            // Nothing came back at all, so there is no answer to show and nothing to offer.
            markdown if markdown.is_empty() => Err(StepError::SchemaMismatch { raw, errors }),
            markdown => Ok((
                Reading::Unclear {
                    markdown,
                    estimate: None,
                    would_pick: None,
                },
                Requested::default(),
            )),
        },
        Err(err) => Err(err),
    }
}

fn reading(
    reply: IntakeReply,
    settings: &Settings,
    store: Option<&Store>,
    check: CheckWords<'_>,
) -> (Reading, Requested) {
    let markdown = reply.answer_markdown;
    // With `triage.read_instructions` off, the request runs as if it said nothing of how.
    let how_to_run = if settings.triage.read_instructions {
        reply.how_to_run
    } else {
        Vec::new()
    };
    let applied = if how_to_run.is_empty() {
        Applied::default()
    } else {
        check(&how_to_run)
    };
    let asked = applied.asked;
    let decide = |estimate: &ChangeEstimate| {
        let matched = store.and_then(|store| {
            recipe::matched(store, settings, &estimate.task_type, &estimate.risk_labels)
        });
        argonauts_crew::triage::decide_with(
            estimate,
            settings,
            matched.as_ref().map(|r| &r.loaded),
            asked,
        )
    };
    let reading = match (reply.kind, reply.estimate) {
        (ReplyKind::Answer, _) => Reading::Answer { markdown },
        (ReplyKind::Change, Some(estimate)) => {
            let triage = decide(&estimate);
            Reading::Change {
                markdown,
                estimate,
                triage,
            }
        }
        // A change with no estimate gives the rules nothing to read, so you pick.
        (ReplyKind::Change | ReplyKind::Unclear, estimate) => {
            let would_pick = estimate.as_ref().map(|estimate| decide(estimate).mode);
            Reading::Unclear {
                markdown,
                estimate,
                would_pick,
            }
        }
    };
    let requested = Requested {
        how_to_run,
        applied,
    };
    (reading, requested)
}

/// `/ask`'s `ChatAnswer`, read the same way.
pub(crate) fn read_answer(outcome: &Result<StepOutcome, StepError>) -> Result<Reading, StepError> {
    let answer = match outcome {
        Ok(outcome) => decode::<ChatAnswer>(outcome),
        Err(err) => Err(err.clone()),
    };
    match answer {
        Ok(answer) => Ok(Reading::Answer {
            markdown: answer.answer_markdown,
        }),
        // An answer is all `/ask` wants, so text that missed the schema still is one.
        Err(StepError::SchemaMismatch { raw, errors }) => match answer_text(raw.as_deref()) {
            markdown if markdown.is_empty() => Err(StepError::SchemaMismatch { raw, errors }),
            markdown => Ok(Reading::Answer { markdown }),
        },
        Err(err) => Err(err),
    }
}

/// What to show from a reply that failed the schema: its `answer_markdown` if the JSON has
/// one, else whatever text came back.
pub(crate) fn answer_text(raw: Option<&str>) -> String {
    let Some(raw) = raw else {
        return String::new();
    };
    let from_json = last_json_object(raw).and_then(|value| {
        value
            .get("answer_markdown")
            .and_then(|answer| answer.as_str())
            .map(str::to_owned)
    });
    from_json.unwrap_or_else(|| raw.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::session::EndReason;
    use argonauts_testkit::worker::session_end;
    use serde_json::{Value, json};

    fn outcome(value: Value) -> Result<StepOutcome, StepError> {
        Ok(StepOutcome {
            value: value.clone(),
            end: session_end(EndReason::Completed, None, Some(value)),
            attempts: 1,
        })
    }

    fn estimate() -> Value {
        json!({
            "task_type": "api-feature",
            "summary": "Add rate limiting",
            "files_likely": ["src/server.rs"],
            "file_count": 9,
            "risk_labels": ["api"],
            "tests_exist": true,
            "unknowns": []
        })
    }

    #[test]
    fn schema_failure_counts_as_unclear() {
        let settings = Settings::default();
        let failed = Err(StepError::SchemaMismatch {
            raw: Some(r#"{"kind": "maybe", "answer_markdown": "Use a middleware."}"#.to_owned()),
            errors: vec!["/kind: not one of the allowed values".to_owned()],
        });
        assert_eq!(
            read_reply(&failed, &settings),
            Ok(Reading::Unclear {
                markdown: "Use a middleware.".to_owned(),
                estimate: None,
                would_pick: None,
            })
        );
        // Output that passed the runner's schema but still doesn't decode counts the same.
        let odd = outcome(json!({"kind": "answer"}));
        assert!(matches!(
            read_reply(&odd, &settings),
            Ok(Reading::Unclear {
                would_pick: None,
                ..
            })
        ));
        let plain = Err(StepError::SchemaMismatch {
            raw: Some("Just text, no JSON.\n".to_owned()),
            errors: Vec::new(),
        });
        assert_eq!(
            read_reply(&plain, &settings),
            Ok(Reading::Unclear {
                markdown: "Just text, no JSON.".to_owned(),
                estimate: None,
                would_pick: None,
            })
        );
    }

    #[test]
    fn each_kind_reads_as_its_branch() {
        let settings = Settings::default();
        let answer = outcome(json!({"kind": "answer", "answer_markdown": "Hi.", "estimate": null}));
        assert_eq!(
            read_reply(&answer, &settings),
            Ok(Reading::Answer {
                markdown: "Hi.".to_owned()
            })
        );
        let change = outcome(json!({
            "kind": "change", "answer_markdown": "Plan.", "estimate": estimate()
        }));
        let Ok(Reading::Change { triage, .. }) = read_reply(&change, &settings) else {
            panic!("a change");
        };
        let decided: ChangeEstimate = serde_json::from_value(estimate()).unwrap();
        assert_eq!(triage, argonauts_crew::triage::decide(&decided, &settings));
        let unclear = outcome(json!({
            "kind": "unclear", "answer_markdown": "Could be.", "estimate": estimate()
        }));
        let Ok(Reading::Unclear { would_pick, .. }) = read_reply(&unclear, &settings) else {
            panic!("unclear");
        };
        assert_eq!(would_pick, Some(triage.mode));
        let bare_change = outcome(json!({
            "kind": "change", "answer_markdown": "Plan.", "estimate": null
        }));
        assert!(matches!(
            read_reply(&bare_change, &settings),
            Ok(Reading::Unclear {
                would_pick: None,
                ..
            })
        ));
    }

    #[test]
    fn a_reply_with_no_text_at_all_is_no_reading() {
        let silent = Err(StepError::SchemaMismatch {
            raw: None,
            errors: vec!["the answer held no JSON object".to_owned()],
        });
        assert!(read_reply(&silent, &Settings::default()).is_err());
    }

    #[test]
    fn a_session_that_ended_early_is_no_reading() {
        let end = session_end(EndReason::Stopped, None, None);
        let stopped = Err(StepError::Session(Box::new(end)));
        assert!(read_reply(&stopped, &Settings::default()).is_err());
    }
}
