//! The core [`StepRunner`]: checks each session's output against the schema and, on a
//! mismatch, asks the same session once more with the errors. A session runner that can't resume
//! gets the prompt again with the errors.

use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{StepError, StepObserver, StepOutcome, StepRequest, StepRunner, asking};
use crate::Spend;
use crate::events::session::{EndReason, InputPart, InputPartKind, SessionEnd};
use crate::future::BoxFuture;
use crate::session_runner::{SessionControl, SessionRunner, SessionSpec};

/// Holds no state, so one runner can be shared by every run.
#[derive(Debug, Clone, Copy, Default)]
pub struct SchemaStepRunner;

impl StepRunner for SchemaStepRunner {
    fn run(
        &self,
        request: StepRequest,
        mut observer: StepObserver,
    ) -> BoxFuture<'_, Result<StepOutcome, StepError>> {
        Box::pin(async move {
            let StepRequest {
                worker,
                mut spec,
                retries_on_mismatch,
                ask_limit,
                mut control,
                cancel,
                normalize,
            } = request;
            let validator = match spec.json_schema.as_ref().map(jsonschema::validator_for) {
                Some(Ok(validator)) => Some(validator),
                Some(Err(err)) => {
                    return Err(StepError::SchemaMismatch {
                        raw: None,
                        errors: vec![format!("the step's own schema is invalid: {err}")],
                    });
                }
                None => None,
            };
            let first_prompt = spec.prompt.clone();
            let mut tries: u8 = 0;
            let mut mismatches: u8 = 0;
            let mut asks: u8 = 0;
            loop {
                tries = tries.saturating_add(1);
                let end = run_session(
                    worker.as_ref(),
                    spec.clone(),
                    &mut observer,
                    control.as_mut(),
                    &cancel,
                )
                .await?;
                let output = output_of(&end);
                // What's checked and returned; a mismatch reports what the session sent.
                let filled = output
                    .clone()
                    .map(|value| normalize(fill_left_out(value, spec.json_schema.as_ref())));
                let errors = match (&filled, &validator) {
                    (Some(value), Some(validator)) => validator
                        .iter_errors(value)
                        .map(|err| describe_error(&err.instance_path().to_string(), &err))
                        .collect(),
                    (Some(_), None) => Vec::new(),
                    (None, _) => vec!["the answer held no JSON object".to_owned()],
                };
                if let Some(value) = filled
                    && errors.is_empty()
                {
                    return Ok(StepOutcome { value, end, tries });
                }
                // A session that stopped early can't take a retry; its end says why.
                if end.reason != EndReason::Completed || cancel.is_cancelled() {
                    return Err(StepError::Session(Box::new(end)));
                }
                // A turn that ends with a question has no one to answer it, so nothing would
                // ever read it. The session is told to decide, and that counts against its own
                // limit, never against the retries a wrong answer gets.
                let told = asking::reply(&end, u32::from(asks), ask_limit.map(u32::from))
                    .filter(|_| output.is_none());
                let reply = if let Some(decide) = told {
                    asks += 1;
                    decide
                } else {
                    if mismatches >= retries_on_mismatch {
                        return Err(mismatch(&end, output, errors));
                    }
                    mismatches += 1;
                    let resumes = end.cli_session.is_some() && worker.capabilities().resume;
                    retry_part(
                        &errors,
                        if resumes {
                            Retry::SameSession
                        } else {
                            Retry::NewSession
                        },
                    )
                };
                let resumable = end
                    .cli_session
                    .clone()
                    .filter(|_| worker.capabilities().resume);
                match resumable {
                    Some(cli_session) => {
                        spec.resume = Some(cli_session);
                        spec.prompt = vec![reply];
                    }
                    // An endpoint keeps no session, so it gets the whole prompt again.
                    None => {
                        spec.resume = None;
                        spec.prompt = first_prompt.clone();
                        spec.prompt.push(reply);
                    }
                }
            }
        })
    }
}

/// The handle's `control` stays alive until the end, because dropping it stops the session.
async fn run_session(
    worker: &dyn SessionRunner,
    spec: SessionSpec,
    observer: &mut StepObserver,
    mut steering: Option<&mut mpsc::Receiver<SessionControl>>,
    cancel: &CancellationToken,
) -> Result<SessionEnd, StepError> {
    let mut handle = worker.start(spec).await?;
    let mut stop_sent = false;
    loop {
        tokio::select! {
            event = handle.events.recv() => match event {
                Some(event) => observer(&event),
                None => break,
            },
            () = cancel.cancelled(), if !stop_sent => {
                stop_sent = true;
                // A full or closed channel means the session is already on its way out.
                let _ = handle.control.try_send(SessionControl::Stop);
            }
            command = next_command(&mut steering), if !stop_sent => match command {
                Some(command) => {
                    let _ = handle.control.send(command).await;
                }
                None => steering = None,
            },
        }
    }
    Ok(handle.ended.await.unwrap_or_else(|_| SessionEnd {
        cli_session: None,
        exit_code: None,
        signal: None,
        duration_ms: 0,
        reason: EndReason::Crashed {
            message: "the session runner ended the session without saying how".to_owned(),
        },
        spend: Spend::default(),
        tool_calls: 0,
        result_text: None,
        structured: None,
        last_message: None,
        raw_log: None,
    }))
}

/// The next steering command, or never when nobody steers.
async fn next_command(
    steering: &mut Option<&mut mpsc::Receiver<SessionControl>>,
) -> Option<SessionControl> {
    match steering {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

/// The structured output, or else the last JSON object in the final text or last message.
fn output_of(end: &SessionEnd) -> Option<Value> {
    if let Some(structured) = &end.structured
        && structured.is_object()
    {
        return Some(structured.clone());
    }
    [&end.result_text, &end.last_message]
        .into_iter()
        .flatten()
        .find_map(|text| last_json_object(text))
}

/// Lists added to a step's output after replies were recorded. A reply that leaves one out
/// means an empty one, as serde reads it, so recorded replies and the fakes written before
/// still pass the schema.
const MAY_BE_LEFT_OUT: &[&str] = &["how_to_run"];

fn fill_left_out(mut value: Value, schema: Option<&Value>) -> Value {
    let Some(properties) = schema.and_then(|schema| schema.get("properties")) else {
        return value;
    };
    if let Value::Object(map) = &mut value {
        for name in MAY_BE_LEFT_OUT {
            if properties.get(*name).is_some() && !map.contains_key(*name) {
                map.insert((*name).to_owned(), Value::Array(Vec::new()));
            }
        }
    }
    value
}

/// The last complete top-level JSON object in `text`, such as one inside a ```json fence.
pub fn last_json_object(text: &str) -> Option<Value> {
    let mut last = None;
    let mut from = 0;
    while let Some(offset) = text[from..].find('{') {
        let start = from + offset;
        let mut values = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match values.next() {
            Some(Ok(value @ Value::Object(_))) => {
                from = start + values.byte_offset();
                last = Some(value);
            }
            _ => from = start + 1,
        }
    }
    last
}

fn describe_error(path: &str, err: &impl std::fmt::Display) -> String {
    if path.is_empty() {
        err.to_string()
    } else {
        format!("{path}: {err}")
    }
}

fn mismatch(end: &SessionEnd, output: Option<Value>, errors: Vec<String>) -> StepError {
    let raw = output
        .map(|value| value.to_string())
        .or_else(|| end.result_text.clone())
        .or_else(|| end.last_message.clone());
    StepError::SchemaMismatch { raw, errors }
}

/// Whether the retry reaches the session that gave the wrong answer.
#[derive(Debug, Clone, Copy)]
enum Retry {
    SameSession,
    NewSession,
}

fn retry_part(errors: &[String], retry: Retry) -> InputPart {
    let mut text = String::from(match retry {
        Retry::SameSession => {
            "Your last answer didn't match the required JSON schema. Fix these problems and \
             reply with only the corrected JSON object:\n"
        }
        Retry::NewSession => {
            "An earlier answer to this prompt didn't match the required JSON schema. Avoid these \
             problems and reply with only the JSON object:\n"
        }
    });
    for error in errors {
        text.push_str("- ");
        text.push_str(error);
        text.push('\n');
    }
    InputPart {
        kind: InputPartKind::Message,
        label: "schema errors".to_owned(),
        approx_tokens: (text.len() / 4) as u32,
        text,
        source: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use tokio::sync::{mpsc, oneshot};

    use crate::AgentEvent;
    use crate::CommandSpec;
    use crate::names::{AgentId, ModelRef, RoleName, SessionKey};
    use crate::session_runner::{
        Access, RunnerCapabilities, RunnerError, RunnerEvent, RunnerKind, SandboxProfile,
        SessionControl, SessionHandle, SessionLimits, SteerSupport, SubagentVisibility,
    };
    use crate::settings::sessions::{Compaction, NativeFanout, SessionLifetime};
    use crate::steps::intake::{Estimate, IntakeReply, ReplyKind};
    use crate::steps::{decode, output_schema};

    /// Plays one scripted ending per session start and remembers every spec.
    struct Scripted {
        worker: ModelRef,
        endings: Mutex<Vec<SessionEnd>>,
        specs: Mutex<Vec<SessionSpec>>,
        resume: bool,
    }

    impl Scripted {
        fn new(endings: Vec<SessionEnd>) -> Arc<Self> {
            Arc::new(Self {
                worker: ModelRef::new("fake", None),
                endings: Mutex::new(endings),
                specs: Mutex::new(Vec::new()),
                resume: true,
            })
        }

        /// Like an endpoint, which keeps no session to resume.
        fn without_resume(endings: Vec<SessionEnd>) -> Arc<Self> {
            Arc::new(Self {
                worker: ModelRef::new("fake", None),
                endings: Mutex::new(endings),
                specs: Mutex::new(Vec::new()),
                resume: false,
            })
        }
    }

    impl SessionRunner for Scripted {
        fn model_ref(&self) -> &ModelRef {
            &self.worker
        }
        fn kind(&self) -> RunnerKind {
            RunnerKind::Cli
        }
        fn agent_id(&self) -> AgentId {
            self.worker.agent_id()
        }
        fn provider(&self) -> String {
            "fake".to_owned()
        }
        fn capabilities(&self) -> RunnerCapabilities {
            RunnerCapabilities {
                streams_text: true,
                steer: SteerSupport::NextTurnOnly,
                resume: self.resume,
                fork: false,
                structured_output: false,
                subagents: SubagentVisibility::None,
                native_limit_cap: false,
                take_over: false,
                can_build: false,
                status_tool: false,
            }
        }
        fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<SessionHandle, RunnerError>> {
            let key = spec.key.clone();
            self.specs.lock().unwrap().push(spec);
            let next = {
                let mut endings = self.endings.lock().unwrap();
                (!endings.is_empty()).then(|| endings.remove(0))
            };
            Box::pin(async move {
                let Some(end) = next else {
                    return Err(RunnerError::Refused("no more scripted sessions".into()));
                };
                let (events_tx, events) = mpsc::channel(8);
                let (control, _control_rx) = mpsc::channel::<SessionControl>(1);
                let (ended_tx, ended) = oneshot::channel();
                let text = end.result_text.clone().unwrap_or_default();
                events_tx
                    .send(RunnerEvent::Agent(AgentEvent::TextDelta {
                        text,
                        parent: None,
                    }))
                    .await
                    .unwrap();
                drop(events_tx);
                ended_tx.send(end).unwrap();
                Ok(SessionHandle {
                    key,
                    events,
                    control,
                    ended,
                })
            })
        }
        fn take_over_command(&self, _cli_session: &str, _workdir: &Path) -> Option<CommandSpec> {
            None
        }
    }

    fn ending(reason: EndReason, text: Option<&str>, structured: Option<Value>) -> SessionEnd {
        SessionEnd {
            cli_session: Some("cli-1".into()),
            exit_code: Some(0),
            signal: None,
            duration_ms: 5,
            reason,
            spend: Spend::default(),
            tool_calls: 0,
            result_text: text.map(str::to_owned),
            structured,
            last_message: None,
            raw_log: None,
        }
    }

    fn spec() -> SessionSpec {
        SessionSpec {
            run: None,
            task: None,
            key: SessionKey::numbered(1),
            role: RoleName::Main,
            step: Some(super::super::StepName::Estimate),
            model: ModelRef::new("fake", None),
            effort: None,
            access: Access::ReadOnly,
            workdir: PathBuf::from("/repo"),
            session_dir: PathBuf::from("/state/s-001"),
            prompt: vec![InputPart {
                kind: InputPartKind::Prompt,
                label: "prompt".into(),
                text: "hi".into(),
                approx_tokens: 1,
                source: None,
            }],
            json_schema: Some(output_schema::<Estimate>()),
            resume: None,
            fork: false,
            lifetime: SessionLifetime::Task,
            keep_input_open: false,
            limits: SessionLimits::default(),
            goal_sha256: None,
            sandbox: SandboxProfile::default(),
            compaction: Compaction::Inherit,
            native_fanout: NativeFanout::default(),
            isolated: false,
            autopilot: false,
            status_file: None,
            env: Vec::new(),
            asking: None,
            cli_workflow: crate::session_runner::CliWorkflow::default(),
            shell: crate::session_runner::SessionShell::None,
        }
    }

    fn answer() -> Value {
        json!({"kind": "answer", "answer_markdown": "Hello!", "estimate": null})
    }

    async fn run(worker: Arc<Scripted>) -> (Result<StepOutcome, StepError>, usize) {
        let seen = Arc::new(Mutex::new(0usize));
        let counter = seen.clone();
        let observer: StepObserver = Box::new(move |_event| *counter.lock().unwrap() += 1);
        let request = StepRequest::new(worker, spec());
        let result = SchemaStepRunner.run(request, observer).await;
        let seen = *seen.lock().unwrap();
        (result, seen)
    }

    #[tokio::test]
    async fn structured_output_that_matches_is_returned_and_decodes() {
        let worker = Scripted::new(vec![ending(EndReason::Completed, None, Some(answer()))]);
        let (result, seen) = run(worker).await;
        let outcome = result.unwrap();
        assert_eq!(outcome.tries, 1);
        assert_eq!(seen, 1, "every event reaches the observer");
        let reply: IntakeReply = decode::<Estimate>(&outcome).unwrap();
        assert_eq!(reply.kind, ReplyKind::Answer);
    }

    #[tokio::test]
    async fn the_last_json_object_in_the_text_counts_when_there_is_no_structured_output() {
        let text = format!("Sure.\n```json\n{}\n```\n", answer());
        let worker = Scripted::new(vec![ending(EndReason::Completed, Some(&text), None)]);
        let (result, _) = run(worker).await;
        let mut expected = answer();
        expected["how_to_run"] = json!([]);
        assert_eq!(result.unwrap().value, expected);
    }

    /// Replies recorded before the instruction list existed still pass on the first try.
    #[tokio::test]
    async fn a_reply_that_leaves_out_the_instruction_list_passes_with_an_empty_one() {
        let worker = Scripted::new(vec![ending(EndReason::Completed, None, Some(answer()))]);
        let (result, _) = run(worker).await;
        let outcome = result.unwrap();
        assert_eq!(outcome.tries, 1);
        assert_eq!(outcome.value["how_to_run"], json!([]));
        let listed = json!({"kind": "answer", "answer_markdown": "Hi.", "estimate": null,
            "how_to_run": [{"says": "codex reviews", "kind": "review", "who": "codex",
                "model": "", "effort": "", "value": "", "scope": "this_request"}]});
        let worker = Scripted::new(vec![ending(
            EndReason::Completed,
            None,
            Some(listed.clone()),
        )]);
        let (result, _) = run(worker).await;
        assert_eq!(result.unwrap().value, listed);
    }

    #[tokio::test]
    async fn a_mismatch_resumes_the_same_session_once_with_the_errors() {
        let wrong = json!({"kind": "maybe", "answer_markdown": "?", "estimate": null});
        let worker = Scripted::new(vec![
            ending(EndReason::Completed, None, Some(wrong)),
            ending(EndReason::Completed, None, Some(answer())),
        ]);
        let (result, _) = run(worker.clone()).await;
        assert_eq!(result.unwrap().tries, 2);
        let specs = worker.specs.lock().unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[1].resume.as_deref(), Some("cli-1"));
        assert_eq!(specs[1].prompt.len(), 1);
        assert!(
            specs[1].prompt[0].text.contains("kind"),
            "{}",
            specs[1].prompt[0].text
        );
    }

    async fn run_asking(
        worker: Arc<Scripted>,
        ask_limit: Option<u8>,
    ) -> Result<StepOutcome, StepError> {
        let mut request = StepRequest::new(worker, spec());
        request.ask_limit = ask_limit;
        SchemaStepRunner.run(request, Box::new(|_| {})).await
    }

    /// A turn that ends with a question is told to decide and carry on, in the same session,
    /// and that doesn't use up the retry a wrong answer gets.
    #[tokio::test]
    async fn a_turn_that_ends_with_a_question_is_told_to_decide() {
        let wrong = json!({"kind": "maybe", "answer_markdown": "?", "estimate": null});
        let worker = Scripted::new(vec![
            ending(
                EndReason::Completed,
                Some("I found two ways.\n\nShould I proceed?"),
                None,
            ),
            ending(EndReason::Completed, None, Some(wrong)),
            ending(EndReason::Completed, None, Some(answer())),
        ]);
        let outcome = run_asking(worker.clone(), Some(2)).await.unwrap();
        assert_eq!(outcome.tries, 3);
        let specs = worker.specs.lock().unwrap();
        assert_eq!(specs[1].resume.as_deref(), Some("cli-1"));
        assert_eq!(specs[1].prompt.len(), 1);
        assert_eq!(specs[1].prompt[0].label, "decide and carry on");
        assert!(
            specs[1].prompt[0].text.contains("Decide yourself"),
            "{}",
            specs[1].prompt[0].text
        );
        assert_eq!(specs[2].prompt[0].label, "schema errors");
    }

    /// After `ask_limit` it is told to take the option it recommended, once, and one more
    /// question is an answer that doesn't match.
    #[tokio::test]
    async fn past_the_limit_it_takes_its_recommended_option_and_then_fails() {
        let asks = || {
            ending(
                EndReason::Completed,
                Some("Which do you prefer, A or B?"),
                None,
            )
        };
        let worker = Scripted::new(vec![asks(), asks(), asks(), asks()]);
        let result = run_asking(worker.clone(), Some(1)).await;
        assert!(matches!(result, Err(StepError::SchemaMismatch { .. })));
        let specs = worker.specs.lock().unwrap();
        assert_eq!(specs.len(), 4);
        assert!(specs[1].prompt[0].text.contains("Decide yourself"));
        assert!(
            specs[2].prompt[0]
                .text
                .contains("Take the option you recommended"),
            "{}",
            specs[2].prompt[0].text
        );
        // The fourth turn is the one retry any answer without its JSON gets.
        assert_eq!(specs[3].prompt[0].label, "schema errors");
    }

    /// With no limit set nobody replies to a question, as before.
    #[tokio::test]
    async fn without_an_ask_limit_a_question_is_only_a_wrong_answer() {
        let asks = || ending(EndReason::Completed, Some("Should I proceed?"), None);
        let worker = Scripted::new(vec![asks(), asks()]);
        let result = run_asking(worker.clone(), None).await;
        assert!(matches!(result, Err(StepError::SchemaMismatch { .. })));
        let specs = worker.specs.lock().unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[1].prompt[0].label, "schema errors");
    }

    #[tokio::test]
    async fn a_worker_that_cannot_resume_gets_the_prompt_again_with_the_errors() {
        let wrong = json!({"kind": "maybe", "answer_markdown": "?", "estimate": null});
        let mut first = ending(EndReason::Completed, None, Some(wrong));
        first.cli_session = None;
        let worker = Scripted::without_resume(vec![
            first,
            ending(EndReason::Completed, None, Some(answer())),
        ]);
        let (result, _) = run(worker.clone()).await;
        assert_eq!(result.unwrap().tries, 2);
        let specs = worker.specs.lock().unwrap();
        assert_eq!(specs[1].resume, None);
        assert_eq!(specs[1].prompt.len(), 2);
        assert_eq!(specs[1].prompt[0], specs[0].prompt[0]);
        assert!(
            specs[1].prompt[1].text.contains("kind"),
            "{}",
            specs[1].prompt[1].text
        );
    }

    #[tokio::test]
    async fn a_second_mismatch_is_an_error_with_the_raw_output() {
        let wrong = json!({"kind": "maybe"});
        let worker = Scripted::new(vec![
            ending(EndReason::Completed, None, Some(wrong.clone())),
            ending(EndReason::Completed, None, Some(wrong.clone())),
        ]);
        let (result, _) = run(worker).await;
        match result {
            Err(StepError::SchemaMismatch { raw, errors }) => {
                assert_eq!(raw, Some(wrong.to_string()));
                assert!(!errors.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_session_that_stops_early_is_a_session_error() {
        let limit = EndReason::UsageLimit { resets_at: None };
        let worker = Scripted::new(vec![ending(limit.clone(), None, None)]);
        let (result, _) = run(worker).await;
        match result {
            Err(StepError::Session(end)) => assert_eq!(end.reason, limit),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_worker_that_refuses_is_a_worker_error() {
        let worker = Scripted::new(Vec::new());
        let (result, _) = run(worker).await;
        assert!(matches!(
            result,
            Err(StepError::Runner(RunnerError::Refused(_)))
        ));
    }

    /// A session that runs until it's told to stop, and remembers what it was sent.
    struct Waiting {
        worker: ModelRef,
        received: Arc<Mutex<Vec<SessionControl>>>,
    }

    impl SessionRunner for Waiting {
        fn model_ref(&self) -> &ModelRef {
            &self.worker
        }
        fn kind(&self) -> RunnerKind {
            RunnerKind::Cli
        }
        fn agent_id(&self) -> AgentId {
            self.worker.agent_id()
        }
        fn provider(&self) -> String {
            "fake".to_owned()
        }
        fn capabilities(&self) -> RunnerCapabilities {
            Scripted::new(Vec::new()).capabilities()
        }
        fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<SessionHandle, RunnerError>> {
            let received = self.received.clone();
            Box::pin(async move {
                let (events_tx, events) = mpsc::channel(8);
                let (control, mut control_rx) = mpsc::channel::<SessionControl>(4);
                let (ended_tx, ended) = oneshot::channel();
                tokio::spawn(async move {
                    // A closed control channel counts as Stop, as the session runner contract says.
                    while let Some(message) = control_rx.recv().await {
                        let stop = message == SessionControl::Stop;
                        received.lock().unwrap().push(message);
                        if stop {
                            break;
                        }
                    }
                    drop(events_tx);
                    let _ = ended_tx.send(ending(EndReason::Stopped, None, None));
                });
                Ok(SessionHandle {
                    key: spec.key,
                    events,
                    control,
                    ended,
                })
            })
        }
        fn take_over_command(&self, _cli_session: &str, _workdir: &Path) -> Option<CommandSpec> {
            None
        }
    }

    fn waiting() -> Arc<Waiting> {
        Arc::new(Waiting {
            worker: ModelRef::new("fake", None),
            received: Arc::new(Mutex::new(Vec::new())),
        })
    }

    #[tokio::test]
    async fn cancelling_a_step_stops_its_session() {
        let worker = waiting();
        let request = StepRequest::new(worker.clone(), spec());
        let cancel = request.cancel.clone();
        let step = tokio::spawn(async move {
            SchemaStepRunner
                .run(request, Box::new(|_event: &RunnerEvent| {}))
                .await
        });
        cancel.cancel();
        match step.await.unwrap() {
            Err(StepError::Session(end)) => assert_eq!(end.reason, EndReason::Stopped),
            other => panic!("{other:?}"),
        }
        assert_eq!(*worker.received.lock().unwrap(), vec![SessionControl::Stop]);
    }

    #[tokio::test]
    async fn steering_commands_reach_the_running_session() {
        let worker = waiting();
        let (steer, steering) = mpsc::channel(4);
        let mut request = StepRequest::new(worker.clone(), spec());
        request.control = Some(steering);
        let step = tokio::spawn(async move {
            SchemaStepRunner
                .run(request, Box::new(|_event: &RunnerEvent| {}))
                .await
        });
        steer.send(SessionControl::WrapUp).await.unwrap();
        steer.send(SessionControl::Stop).await.unwrap();
        assert!(matches!(step.await.unwrap(), Err(StepError::Session(_))));
        assert_eq!(
            *worker.received.lock().unwrap(),
            vec![SessionControl::WrapUp, SessionControl::Stop]
        );
    }

    #[test]
    fn last_json_object_finds_top_level_objects_only() {
        assert_eq!(last_json_object("no json here"), None);
        assert_eq!(
            last_json_object(r#"{"a":{"b":1}}"#),
            Some(json!({"a": {"b": 1}}))
        );
        assert_eq!(
            last_json_object(r#"first {"a":1} then {"b":[{"c":2}]} done"#),
            Some(json!({"b": [{"c": 2}]}))
        );
        assert_eq!(
            last_json_object(r#"{"a":1} and a broken { one"#),
            Some(json!({"a": 1}))
        );
    }
}
