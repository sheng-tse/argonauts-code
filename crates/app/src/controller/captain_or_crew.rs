//! Reading a conversation turn's reply. The estimate is an answer, a change, or unclear, and one
//! that fails the schema counts as unclear. `/ask` wants only an answer. A learned workflow whose
//! `match` fits the change decides Captain or crew under `captain_or_crew.use_learned`, unless the
//! request's own words chose, once code checked them. A crew workflow you chose convenes for
//! every change; one that is only the default leaves a small change to the main agent, and the
//! app says so.

use argonauts_core::events::run::{CaptainOrCrewDecision, RunMode};
use argonauts_core::log::DOING_THIS_ALONE;
use argonauts_core::settings::{Settings, layer};
use argonauts_core::steps::intake::{
    ChangeEstimate, ConversationAnswer, Estimate, Instruction, IntakeReply, ReplyKind,
};
use argonauts_core::steps::{StepError, StepOutcome, decode, last_json_object};
use argonauts_core::text::{sentence_case, title_words};
use argonauts_learning::Store;

use crate::app_settings::AppSettings;
use crate::controller::instructions::Applied;
use crate::learning::learned_workflow;

/// What the request said about how to run it, and what code made of it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Requested {
    /// As the estimate listed them, for a question's answer to check again.
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
        captain_or_crew: CaptainOrCrewDecision,
    },
    Unclear {
        markdown: String,
        estimate: Option<ChangeEstimate>,
        /// What the Captain-or-crew rules would pick, said on the `StartWork` question.
        would_pick: Option<RunMode>,
    },
}

/// A step that ended without output at all is not a reading, so it comes back as the error.
#[cfg(test)]
pub(crate) fn read_reply(
    outcome: &Result<StepOutcome, StepError>,
    settings: &Settings,
) -> Result<Reading, StepError> {
    read_reply_with(outcome, settings, false, None, &|_| Applied::default())
        .map(|(reading, _)| reading)
}

/// [`read_reply`] with whether you chose the workflow in use, the repo's learned workflows, from
/// `store`'s main checkout, and the request's own words on how to run it, checked by `check`.
pub(crate) fn read_reply_with(
    outcome: &Result<StepOutcome, StepError>,
    settings: &Settings,
    workflow_chosen: bool,
    store: Option<&Store>,
    check: CheckWords<'_>,
) -> Result<(Reading, Requested), StepError> {
    let reply = match outcome {
        Ok(outcome) => decode::<Estimate>(outcome),
        Err(err) => Err(err.clone()),
    };
    match reply {
        Ok(reply) => Ok(reading(reply, settings, workflow_chosen, store, check)),
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
    workflow_chosen: bool,
    store: Option<&Store>,
    check: CheckWords<'_>,
) -> (Reading, Requested) {
    let markdown = reply.answer_markdown;
    // With `captain_or_crew.read_instructions` off, the request runs as if it said nothing of how.
    let how_to_run = if settings.captain_or_crew.read_instructions {
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
        // Your words, Ultracode and a forced mode still win over the workflow you chose.
        let by_rules =
            asked.mode.is_none() && argonauts_crew::captain_or_crew::ultracode(settings).is_none();
        if by_rules && let Some(crew) = convened(settings, workflow_chosen) {
            return crew;
        }
        let matched = store.and_then(|store| {
            learned_workflow::matched(store, settings, &estimate.task_type, &estimate.risk_labels)
        });
        argonauts_crew::captain_or_crew::decide_with(
            estimate,
            settings,
            matched.as_ref().map(|r| &r.loaded),
            asked,
        )
    };
    let reading = match (reply.kind, reply.estimate) {
        (ReplyKind::Answer, _) => Reading::Answer { markdown },
        (ReplyKind::Change, Some(estimate)) => {
            let captain_or_crew = decide(&estimate);
            Reading::Change {
                markdown,
                estimate,
                captain_or_crew,
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

/// The rule a crew workflow you chose fires.
pub(crate) const CHOSEN_WORKFLOW: &str = "chosen_workflow";

/// Whether you chose the workflow in use: with `/council` or another workflow's command, in a
/// settings file, or with a flag. The built-in default is no choice.
pub(crate) fn workflow_chosen(settings: &AppSettings) -> bool {
    let from = settings.layer("workflow");
    from != layer::BUILT_IN && from != layer::WORKFLOW
}

/// A crew workflow you chose convenes for every change, small ones included. A workflow that
/// forces a mode, as `captain` does, keeps it.
fn convened(settings: &Settings, workflow_chosen: bool) -> Option<CaptainOrCrewDecision> {
    let crew_workflow = settings.captain_or_crew.force.is_none();
    (workflow_chosen && crew_workflow).then(|| CaptainOrCrewDecision {
        mode: RunMode::Crew,
        move_to_crew: false,
        rule: CHOSEN_WORKFLOW.to_owned(),
        reason: format!("you chose {}", workflow_name(settings)),
        captain: None,
    })
}

/// The app's own line when the rules leave a change to the main agent while a crew workflow is
/// only the default: why it works alone, and how to have the workflow do it instead. `None` when
/// the workflow convenes, or you or the workflow chose the mode.
pub(crate) fn alone_line(
    settings: &Settings,
    workflow_chosen: bool,
    estimate: &ChangeEstimate,
    decision: &CaptainOrCrewDecision,
) -> Option<String> {
    let rules_picked = !decision.as_you_asked() && settings.captain_or_crew.force.is_none();
    if decision.mode != RunMode::Captain || workflow_chosen || !rules_picked {
        return None;
    }
    let why = match decision.rule.strip_prefix("learned:") {
        Some(learned) => format!(", as the learned workflow {learned} says"),
        None => {
            let listed = u32::try_from(estimate.files_likely.len()).unwrap_or(u32::MAX);
            let files = match estimate.file_count.max(listed) {
                1 => "1 file".to_owned(),
                files => format!("{files} files"),
            };
            let tests = if estimate.tests_exist {
                ", and the repo has tests"
            } else {
                ""
            };
            format!(": about {files}{tests}")
        }
    };
    Some(format!(
        "{DOING_THIS_ALONE}{why} · /crew to have {} do it",
        workflow_name(settings)
    ))
}

/// The workflow in use as a sentence names it: `Council`, `Draft and vote`.
fn workflow_name(settings: &Settings) -> String {
    sentence_case(&title_words(&settings.workflow))
}

/// `/ask`'s `ConversationAnswer`, read the same way.
pub(crate) fn read_answer(outcome: &Result<StepOutcome, StepError>) -> Result<Reading, StepError> {
    let answer = match outcome {
        Ok(outcome) => decode::<ConversationAnswer>(outcome),
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
    use argonauts_core::settings::protocol::ForcedMode;
    use argonauts_testkit::runner::session_end;
    use serde_json::{Value, json};

    fn outcome(value: Value) -> Result<StepOutcome, StepError> {
        Ok(StepOutcome {
            value: value.clone(),
            end: session_end(EndReason::Completed, None, Some(value)),
            tries: 1,
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
        let Ok(Reading::Change {
            captain_or_crew, ..
        }) = read_reply(&change, &settings)
        else {
            panic!("a change");
        };
        let decided: ChangeEstimate = serde_json::from_value(estimate()).unwrap();
        assert_eq!(
            captain_or_crew,
            argonauts_crew::captain_or_crew::decide(&decided, &settings)
        );
        let unclear = outcome(json!({
            "kind": "unclear", "answer_markdown": "Could be.", "estimate": estimate()
        }));
        let Ok(Reading::Unclear { would_pick, .. }) = read_reply(&unclear, &settings) else {
            panic!("unclear");
        };
        assert_eq!(would_pick, Some(captain_or_crew.mode));
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

    fn small_change() -> Result<StepOutcome, StepError> {
        outcome(json!({
            "kind": "change",
            "answer_markdown": "This is a small change, so I'll make it myself.",
            "estimate": {
                "task_type": "bugfix",
                "summary": "One bucket per key",
                "files_likely": ["ratelimit/bucket.py"],
                "file_count": 1,
                "risk_labels": [],
                "tests_exist": true,
                "unknowns": []
            }
        }))
    }

    fn decided(
        reply: &Result<StepOutcome, StepError>,
        settings: &Settings,
        workflow_chosen: bool,
        asked: Option<RunMode>,
    ) -> (ChangeEstimate, CaptainOrCrewDecision) {
        let applied = Applied {
            asked: argonauts_crew::captain_or_crew::Asked {
                mode: asked,
                captain: None,
            },
            ..Applied::default()
        };
        let check = move |_: &[Instruction]| applied.clone();
        let mut asked_reply = reply.clone();
        if asked.is_some()
            && let Ok(outcome) = &mut asked_reply
        {
            outcome.value["how_to_run"] =
                json!([{"says": "do it alone", "kind": "mode", "value": "captain"}]);
        }
        match read_reply_with(&asked_reply, settings, workflow_chosen, None, &check) {
            Ok((
                Reading::Change {
                    estimate,
                    captain_or_crew,
                    ..
                },
                _,
            )) => (estimate, captain_or_crew),
            other => panic!("a change, not {other:?}"),
        }
    }

    /// A crew workflow you chose convenes for a small change too; as only the default, the
    /// rules give it to the main agent.
    #[test]
    fn a_workflow_you_chose_convenes_and_a_default_one_may_not() {
        let settings = Settings::default();
        let (_, chosen) = decided(&small_change(), &settings, true, None);
        assert_eq!(
            (chosen.mode, chosen.rule.as_str(), chosen.reason.as_str()),
            (RunMode::Crew, CHOSEN_WORKFLOW, "you chose Council")
        );
        let (_, default) = decided(&small_change(), &settings, false, None);
        assert_eq!(
            (default.mode, default.rule.as_str()),
            (RunMode::Captain, "captain_max_files")
        );
    }

    /// Your words, a workflow that forces Captain and Ultracode each keep their say over the
    /// workflow you chose.
    #[test]
    fn words_a_forced_mode_and_ultracode_beat_the_chosen_workflow() {
        let settings = Settings::default();
        let (_, asked) = decided(&small_change(), &settings, true, Some(RunMode::Captain));
        assert_eq!(asked.mode, RunMode::Captain, "{asked:?}");
        assert!(asked.as_you_asked(), "{asked:?}");

        let mut captain = Settings {
            workflow: "captain".to_owned(),
            ..Settings::default()
        };
        captain.captain_or_crew.force = Some(ForcedMode::Captain);
        let (_, forced) = decided(&small_change(), &captain, true, None);
        assert_eq!(
            (forced.mode, forced.rule.as_str()),
            (RunMode::Captain, "force")
        );

        let ultracode = Settings {
            ultracode: true,
            ..Settings::default()
        };
        let (_, crew) = decided(&small_change(), &ultracode, true, None);
        assert_eq!(
            (crew.mode, crew.rule.as_str()),
            (RunMode::Crew, "ultracode")
        );
    }

    /// The app's own line says why the main agent works alone and how to send the change to the
    /// workflow, only when the rules picked that and the workflow is only the default.
    #[test]
    fn the_alone_line_says_why_and_offers_the_workflow() {
        let settings = Settings::default();
        let (estimate, default) = decided(&small_change(), &settings, false, None);
        assert_eq!(
            alone_line(&settings, false, &estimate, &default).as_deref(),
            Some(
                "Doing this alone: about 1 file, and the repo has tests · /crew to have Council do it"
            )
        );

        let mut untested = estimate.clone();
        untested.tests_exist = false;
        untested.file_count = 5;
        let mut otherwise = default.clone();
        otherwise.rule = "otherwise".to_owned();
        assert_eq!(
            alone_line(&settings, false, &untested, &otherwise).as_deref(),
            Some("Doing this alone: about 5 files · /crew to have Council do it")
        );

        let mut learned = default.clone();
        learned.rule = "learned:small-fix".to_owned();
        assert_eq!(
            alone_line(&settings, false, &estimate, &learned).as_deref(),
            Some(
                "Doing this alone, as the learned workflow small-fix says · /crew to have Council do it"
            )
        );

        let draft = Settings {
            workflow: "draft-and-vote".to_owned(),
            ..Settings::default()
        };
        assert!(
            alone_line(&draft, false, &estimate, &default)
                .is_some_and(|line| line.ends_with("/crew to have Draft and vote do it"))
        );

        // Chosen, asked for, forced, or a crew run: nothing to say.
        assert_eq!(alone_line(&settings, true, &estimate, &default), None);
        let (_, asked) = decided(&small_change(), &settings, false, Some(RunMode::Captain));
        assert_eq!(alone_line(&settings, false, &estimate, &asked), None);
        let mut captain = Settings::default();
        captain.captain_or_crew.force = Some(ForcedMode::Captain);
        assert_eq!(alone_line(&captain, false, &estimate, &default), None);
        let (_, crew) = decided(&small_change(), &settings, true, None);
        assert_eq!(alone_line(&settings, false, &estimate, &crew), None);
    }

    /// Through the controller: on council you chose, a one-file change starts the crew; on the
    /// default, the main agent takes it and the app's line says so, with `/crew` to send it on.
    #[tokio::test]
    async fn a_small_change_convenes_the_council_you_chose() {
        use argonauts_core::app::AppEvent;
        use argonauts_testkit::FakeSession;

        use crate::controller::test_support::{
            CliOptions, TestDriver, max_wait, on_the_default_workflow,
        };

        let starts = |options: CliOptions| async move {
            let driver = TestDriver::start_with(options).await;
            let value = small_change().expect("a structured reply").value;
            driver.agent.push(FakeSession::structured(value));
            driver.submit("Make the rate limiter per API key.").await;
            let conductor = driver.conductor.clone();
            let deadline = tokio::time::Instant::now() + max_wait();
            while conductor.starts().is_empty() {
                assert!(tokio::time::Instant::now() < deadline, "no run started");
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            let start = conductor.starts().remove(0);
            let seen = driver.exit().await;
            let said: Vec<String> = seen
                .iter()
                .filter_map(|event| match event {
                    AppEvent::Notice { text, .. } if text.starts_with("Doing this alone") => {
                        Some(text.clone())
                    }
                    _ => None,
                })
                .collect();
            (start, said)
        };

        let (chosen, said) = starts(CliOptions::default()).await;
        let decision = chosen
            .captain_or_crew
            .expect("the Captain-or-crew decision");
        assert_eq!(
            (chosen.mode, decision.rule.as_str()),
            (RunMode::Crew, CHOSEN_WORKFLOW)
        );
        assert!(said.is_empty(), "{said:?}");

        let (default, said) = starts(on_the_default_workflow(CliOptions::default())).await;
        assert_eq!(default.mode, RunMode::Captain);
        assert_eq!(
            said,
            [
                "Doing this alone: about 1 file, and the repo has tests · /crew to have Council do it"
            ]
        );
    }

    /// The alone line's `/crew`, typed bare, hands the request the main agent took to the crew.
    #[tokio::test]
    async fn a_bare_crew_hands_the_request_taken_alone_to_the_crew() {
        use argonauts_core::app::UiCommand;
        use argonauts_core::input::SlashCommand;
        use argonauts_testkit::FakeSession;

        use crate::controller::test_support::{
            CliOptions, TestDriver, max_wait, on_the_default_workflow,
        };

        const REQUEST: &str = "Make the rate limiter per API key.";
        let driver = TestDriver::start_with(on_the_default_workflow(CliOptions::default())).await;
        let value = small_change().expect("a structured reply").value;
        driver.agent.push(FakeSession::structured(value));
        driver.submit(REQUEST).await;
        let conductor = driver.conductor.clone();
        let started = |count: usize| {
            let conductor = conductor.clone();
            async move {
                let deadline = tokio::time::Instant::now() + max_wait();
                while conductor.starts().len() < count {
                    assert!(tokio::time::Instant::now() < deadline, "no run started");
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                conductor.starts()
            }
        };
        assert_eq!(started(1).await[0].mode, RunMode::Captain);

        driver
            .send(UiCommand::Slash {
                command: SlashCommand::Crew { task: None },
            })
            .await;
        let starts = started(2).await;
        assert_eq!(
            (starts[1].mode, starts[1].request.as_str()),
            (RunMode::Crew, REQUEST)
        );
        driver.exit().await;
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
