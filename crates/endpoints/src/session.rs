//! Starting a session: check it, record exactly what will be sent, and hand the exchange to a
//! task. A setting that makes the request impossible still gives a session, which ends with
//! the reason, so the user sees it where they see every other ending.

use std::time::Duration;

use argonauts_core::events::session::{
    EndReason, InputPartKind, LaunchRecord, PromptVia, SessionInput, SessionStarted,
};
use argonauts_core::layout::session_files::RAW_STDOUT;
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::Span;
use argonauts_core::settings::agents::{EndpointApi, StructuredMode};
use argonauts_core::worker::{
    Access, SessionControl, SessionHandle, SessionSpec, WorkerError, WorkerKind,
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::EndpointWorker;
use crate::errors::{self, Secrets};
use crate::raw_log::RawLog;
use crate::request::{self, Credentials, Effort, MissingCredential, Options};
use crate::run::{self, Exchange, Limits, Session};
use crate::structured;

/// Events buffered ahead of a slow reader. Deltas are small, so this is a few screens of text.
const EVENT_BUFFER: usize = 256;

/// Refuses any access but `NoTools`. Records `Session(Input)` with `PromptVia::Request` and a
/// launch record naming the endpoint and model, never the key.
pub(crate) async fn start(
    worker: &EndpointWorker,
    spec: SessionSpec,
) -> Result<SessionHandle, WorkerError> {
    if spec.access != Access::NoTools {
        return Err(WorkerError::Refused(format!(
            "the {} endpoint has no tools, so it takes only no-tools sessions, not {:?}",
            worker.id, spec.access
        )));
    }
    if spec.resume.is_some() {
        return Err(WorkerError::Refused(format!(
            "the {} endpoint keeps no session to resume",
            worker.id
        )));
    }
    let settings = &worker.settings;
    let clock = Instant::now();
    let model = spec
        .worker
        .model
        .clone()
        .or_else(|| worker.worker.model.clone());
    let mut parts = spec.prompt.clone();
    if let Some(schema) = &spec.json_schema
        && !parts.iter().any(|part| part.kind == InputPartKind::Schema)
        && let Some(part) = structured::schema_part(settings.structured_output, schema)
    {
        parts.push(part);
    }
    let url = request::url(&worker.id, settings);
    let effort = effort(worker, model.as_deref(), spec.effort.as_deref());
    let credentials = request::credentials(&worker.id, settings, &spec.env);

    tokio::fs::create_dir_all(&spec.session_dir)
        .await
        .map_err(|err| {
            WorkerError::Io(format!(
                "couldn't make the session folder {}: {err}",
                spec.session_dir.display()
            ))
        })?;
    let secrets = credentials
        .as_ref()
        .map(|found| found.secrets.clone())
        .unwrap_or_default();
    let raw = RawLog::open(&spec.session_dir.join(RAW_STDOUT), secrets.clone())
        .await
        .map_err(WorkerError::Io)?;
    let secrets = Secrets::new(secrets);

    let started = SessionStarted {
        worker: spec.worker.clone(),
        vendor: worker.vendor_id(),
        family: worker.family.clone(),
        kind: WorkerKind::Endpoint,
        role: spec.role,
        step: spec.step,
        access: spec.access,
        lifetime: spec.lifetime,
        resumed_from: None,
        follow_key: None,
        workflow_step: None,
    };
    let input = SessionInput {
        launch: launch_record(
            worker,
            &spec,
            url.as_deref().ok(),
            model.as_deref(),
            &effort,
            &secrets,
        ),
        parts,
        queued: Vec::new(),
    };

    let plan = (|| {
        let url = url.map_err(|message| EndReason::StartFailed { message })?;
        let model = model.clone().ok_or_else(|| EndReason::StartFailed {
            message: format!("{} names no model", spec.worker),
        })?;
        let headers = headers(credentials?)?;
        let client = worker
            .client()
            .map_err(|message| EndReason::StartFailed { message })?;
        let prompt = request::split_prompt(&input.parts);
        let body = request::body(
            &prompt,
            Options {
                api: settings.api,
                mode: settings.structured_output,
                model: &model,
                schema: spec.json_schema.as_ref(),
                effort: effort.sent(),
            },
        );
        Ok(Exchange {
            client,
            url,
            headers,
            body,
            api: settings.api,
            model,
            schema: spec.json_schema.clone(),
            slots: worker.slots.clone(),
            limits: limits(clock, &settings.timeout, spec.limits.max_minutes)?,
        })
    })();

    let (events_tx, events) = mpsc::channel(EVENT_BUFFER);
    let session = Session {
        key: spec.key.clone(),
        vendor: worker.vendor_id(),
        pricing: WorkerRef::new(worker.worker.worker.clone(), model),
        quota: worker.quota.clone(),
        health: worker.health.clone(),
        registry: worker.registry.clone(),
        events: events_tx,
        secrets,
    };
    let (control, control_rx) = mpsc::channel::<SessionControl>(16);
    let (ended_tx, ended) = oneshot::channel();
    tokio::spawn(async move {
        // `run` owns the events sender and drops it on return, so the end follows the last event.
        let end = run::run(session, started, input, plan, raw, control_rx, clock).await;
        let _ = ended_tx.send(end);
    });
    Ok(SessionHandle {
        key: spec.key,
        events,
        control,
        ended,
    })
}

/// Names the endpoint, the address, the model, and the variables the key and headers come
/// from. Never a value, so the address goes without its user, password, and query.
fn launch_record(
    worker: &EndpointWorker,
    spec: &SessionSpec,
    url: Option<&str>,
    model: Option<&str>,
    effort: &Effort,
    secrets: &Secrets,
) -> LaunchRecord {
    let settings = &worker.settings;
    let address = url.map_or_else(|| "<no url>".to_owned(), errors::shown_url);
    let mut args = vec![
        "POST".to_owned(),
        secrets.redact(&address),
        format!("model={}", model.unwrap_or("<none>")),
        format!("api={}", api_name(settings.api)),
        "stream=true".to_owned(),
    ];
    args.extend(effort.launch_arg());
    if spec.json_schema.is_some() {
        args.push(format!(
            "structured_output={}",
            mode_name(settings.structured_output)
        ));
    }
    if let Some(variable) = &settings.api_key_env {
        args.push(format!("authorization=${variable}"));
    }
    for (header, variable) in &settings.header_env {
        args.push(format!("{header}=${variable}"));
    }
    LaunchRecord {
        program: worker.id.clone(),
        args,
        cwd: spec.workdir.clone(),
        env_set: Vec::new(),
        env_removed: Vec::new(),
        prompt_via: PromptVia::Request,
        sandbox: Some("none: a model API with no tools".to_owned()),
    }
}

/// The asked level checked against the efforts the registry lists for this endpoint's model.
fn effort(worker: &EndpointWorker, model: Option<&str>, asked: Option<&str>) -> Effort {
    let entry = worker.registry.get(&WorkerRef::new(
        worker.worker.worker.clone(),
        model.map(str::to_owned),
    ));
    let listed = entry.map(|entry| entry.efforts).unwrap_or_default();
    Effort::choose(asked, &listed)
}

fn api_name(api: EndpointApi) -> &'static str {
    match api {
        EndpointApi::ChatCompletions => "chat_completions",
        EndpointApi::Responses => "responses",
    }
}

fn mode_name(mode: StructuredMode) -> &'static str {
    match mode {
        StructuredMode::JsonSchema => "json_schema",
        StructuredMode::JsonObject => "json_object",
        StructuredMode::Prompt => "prompt",
    }
}

impl From<MissingCredential> for EndReason {
    fn from(missing: MissingCredential) -> Self {
        match missing {
            MissingCredential::Key(message) => EndReason::Auth { message },
            MissingCredential::Header(message) => EndReason::StartFailed { message },
        }
    }
}

fn headers(credentials: Credentials) -> Result<HeaderMap, EndReason> {
    let mut map = HeaderMap::new();
    for (name, value) in credentials.headers {
        let bad = |what: &str| EndReason::StartFailed {
            message: format!("the header {name} has an invalid {what}"),
        };
        let header = HeaderName::from_bytes(name.as_bytes()).map_err(|_| bad("name"))?;
        let mut value = HeaderValue::from_str(&value).map_err(|_| bad("value"))?;
        value.set_sensitive(true);
        map.insert(header, value);
    }
    Ok(map)
}

/// The endpoint's timeout, which each request counts from when it's sent, and the moment the
/// session's minutes, counted from `clock`, run out. A budget too far off for the clock to hold
/// is no limit.
fn limits(clock: Instant, timeout: &Span, max_minutes: Option<f64>) -> Result<Limits, EndReason> {
    let timeout = timeout.parse().map_err(|message| EndReason::StartFailed {
        message: format!("the endpoint's timeout: {message}"),
    })?;
    if let Some(minutes) = max_minutes.filter(|m| !m.is_finite() || *m < 0.0) {
        return Err(EndReason::StartFailed {
            message: format!("the session's max_minutes must be 0 or more, not {minutes}"),
        });
    }
    let budget = max_minutes
        .and_then(|minutes| Duration::try_from_secs_f64(minutes * 60.0).ok())
        .and_then(|budget| clock.checked_add(budget));
    Ok(Limits { timeout, budget })
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use argonauts_core::events::VendorEvent;
    use argonauts_core::events::session::{InputPart, SessionEnd, SessionEvent};
    use argonauts_core::limits::{HealthBoard, HealthSignal, QuotaBoard, VendorHealth};
    use argonauts_core::names::VendorId;
    use argonauts_core::registry::ModelEntry;
    use argonauts_core::settings::agents::EndpointSettings;
    use argonauts_core::worker::{Worker, WorkerEvent};
    use argonauts_core::{AgentEvent, BudgetDimension};
    use argonauts_testkit::services::{MemoryHealthBoard, MemoryQuotaBoard, StaticRegistry};
    use argonauts_testkit::worker::{drain, session_spec};
    use argonauts_testkit::{FakeOpenAi, FakeReply};
    use serde_json::json;
    use tokio::sync::broadcast;

    use super::*;

    fn worker_with(entries: &[(&str, &[&str])]) -> EndpointWorker {
        let registry = Arc::new(StaticRegistry::default());
        for (model, efforts) in entries {
            let entry: ModelEntry = serde_json::from_value(json!({
                "id": format!("openrouter/{model}"), "family": "llama", "kind": "endpoint",
                "access": "api-key", "cost": {"unit": "usd"}, "efforts": efforts,
            }))
            .unwrap();
            registry.add(entry);
        }
        EndpointWorker::new(
            "openrouter",
            &EndpointSettings::default(),
            WorkerRef::new("openrouter", Some("reasoner".to_owned())),
            "llama".to_owned(),
            registry,
            Arc::new(MemoryQuotaBoard::default()),
            Arc::new(MemoryHealthBoard::default()),
        )
    }

    #[test]
    fn the_effort_follows_the_models_registry_entry() {
        let worker = worker_with(&[("reasoner", &["low", "medium", "high"]), ("plain", &[])]);
        let max = Some("max");
        assert_eq!(effort(&worker, Some("reasoner"), max).sent(), Some("high"));
        assert_eq!(
            effort(&worker, Some("reasoner"), Some("low")),
            Effort::Sent("low".into())
        );
        assert_eq!(effort(&worker, Some("plain"), max).sent(), None);
        assert_eq!(effort(&worker, Some("unlisted"), max).sent(), None);
        assert_eq!(effort(&worker, Some("reasoner"), None), Effort::Unset);
    }

    #[test]
    fn the_timeout_is_per_request_and_the_budget_from_the_start() {
        let now = Instant::now();
        let span = Span::new;
        let found = limits(now, &span("5m"), Some(1.0)).unwrap();
        assert_eq!(found.timeout, Some(Duration::from_secs(300)));
        assert_eq!(found.budget, Some(now + Duration::from_secs(60)));
        let found = limits(now, &span("none"), None).unwrap();
        assert_eq!((found.timeout, found.budget), (None, None));
        for minutes in [f64::NAN, f64::INFINITY, -1.0] {
            let refused = limits(now, &span("5m"), Some(minutes));
            assert!(
                matches!(&refused, Err(EndReason::StartFailed { message }) if message.contains("max_minutes")),
                "{minutes}: {refused:?}"
            );
        }
        let broken = limits(now, &span("soon"), None);
        assert!(matches!(broken, Err(EndReason::StartFailed { .. })));
        let forever = limits(now, &span("none"), Some(1e17)).unwrap();
        assert!(forever.budget.is_none_or(|at| at > now));
    }

    /// Keeps every health signal, so a test can check that none blamed the endpoint.
    #[derive(Default)]
    struct Signals {
        board: MemoryHealthBoard,
        seen: Mutex<Vec<HealthSignal>>,
    }

    impl Signals {
        fn errors(&self) -> Vec<HealthSignal> {
            let seen = self.seen.lock().unwrap();
            seen.iter()
                .filter(|signal| matches!(signal, HealthSignal::Error { .. }))
                .cloned()
                .collect()
        }
    }

    impl HealthBoard for Signals {
        fn observe(&self, signal: HealthSignal) {
            self.seen.lock().unwrap().push(signal.clone());
            self.board.observe(signal);
        }

        fn state(&self, vendor: &VendorId) -> VendorHealth {
            self.board.state(vendor)
        }

        fn all(&self) -> Vec<VendorHealth> {
            self.board.all()
        }

        fn subscribe(&self) -> broadcast::Receiver<VendorEvent> {
            self.board.subscribe()
        }

        fn test_result(&self, vendor: &VendorId, ok: bool) {
            self.board.test_result(vendor, ok);
        }
    }

    /// An endpoint that sends one request at a time.
    fn one_at_a_time(server: &FakeOpenAi, timeout: &str, health: Arc<Signals>) -> EndpointWorker {
        let settings = EndpointSettings {
            base_url: Some(server.base_url()),
            max_concurrency: 1,
            timeout: Span::new(timeout),
            ..EndpointSettings::default()
        };
        EndpointWorker::new(
            "fake",
            &settings,
            WorkerRef::new("fake", Some("fake-model".to_owned())),
            "llama".to_owned(),
            Arc::new(StaticRegistry::default()),
            Arc::new(MemoryQuotaBoard::default()),
            health,
        )
    }

    async fn session(worker: &EndpointWorker, dir: &Path, max_minutes: Option<f64>) -> SessionEnd {
        let mut spec = session_spec("fake/fake-model", Access::NoTools, dir);
        spec.prompt = vec![InputPart {
            kind: InputPartKind::StepInput,
            label: "step input".to_owned(),
            text: "Review card 3.".to_owned(),
            approx_tokens: 1,
            source: None,
        }];
        spec.limits.max_minutes = max_minutes;
        let handle = start(worker, spec).await.unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(20), drain(handle));
        ended.await.expect("the session ended").1
    }

    fn slow(reply: &str, millis: u64) -> FakeReply {
        FakeReply::Delay(
            Duration::from_millis(millis),
            Box::new(FakeReply::text(reply)),
        )
    }

    #[tokio::test]
    async fn waiting_for_a_slot_leaves_the_request_its_whole_timeout() {
        let server = FakeOpenAi::start().unwrap();
        server.push(slow("first", 1300));
        server.push(slow("second", 1300));
        let health = Arc::new(Signals::default());
        let worker = one_at_a_time(&server, "2s", health.clone());
        let (first, second) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let clock = std::time::Instant::now();
        let (first, second) = tokio::join!(
            session(&worker, first.path(), None),
            session(&worker, second.path(), None),
        );
        // The second reply ends past 2s from the start, so it did wait in the queue.
        assert!(clock.elapsed() >= Duration::from_millis(2600));
        assert_eq!(first.reason, EndReason::Completed);
        assert_eq!(second.reason, EndReason::Completed, "{:?}", second.reason);
        assert_eq!(server.requests().len(), 2);
        assert!(health.errors().is_empty(), "{:?}", health.errors());
    }

    #[tokio::test]
    async fn a_budget_spent_in_the_queue_is_a_budget_end_that_blames_no_one() {
        let server = FakeOpenAi::start().unwrap();
        server.push(slow("first", 3000));
        let health = Arc::new(Signals::default());
        let worker = one_at_a_time(&server, "5m", health.clone());
        let (first, second) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let one_second = Some(1.0 / 60.0);
        let (first, second) = tokio::join!(session(&worker, first.path(), None), async {
            // Starts second, so it's the one queued.
            tokio::time::sleep(Duration::from_millis(200)).await;
            let end = session(&worker, second.path(), one_second).await;
            (end, server.requests().len())
        },);
        let (second, sent) = second;
        assert_eq!(first.reason, EndReason::Completed);
        assert_eq!(
            second.reason,
            EndReason::BudgetHit {
                dimension: BudgetDimension::Minutes
            }
        );
        assert_eq!(sent, 1, "the queued session never sent its request");
        assert!(health.errors().is_empty(), "{:?}", health.errors());
    }

    #[tokio::test]
    async fn a_rate_limit_wait_leaves_the_next_request_its_whole_timeout() {
        let server = FakeOpenAi::start().unwrap();
        server.push(FakeReply::Error {
            status: 429,
            message: "Rate limit reached for requests".into(),
            retry_after: Some(1),
        });
        server.push(slow("after the wait", 600));
        let health = Arc::new(Signals::default());
        let worker = one_at_a_time(&server, "1s", health.clone());
        let dir = tempfile::tempdir().unwrap();
        let clock = std::time::Instant::now();
        let end = session(&worker, dir.path(), None).await;
        // The wait and the retried request together outlast the 1s timeout.
        assert!(clock.elapsed() >= Duration::from_millis(1600));
        assert_eq!(end.reason, EndReason::Completed, "{:?}", end.reason);
        assert_eq!(end.result_text.as_deref(), Some("after the wait"));
        assert_eq!(server.requests().len(), 2);
        assert!(health.errors().is_empty(), "{:?}", health.errors());
    }

    fn shown_text(events: &[WorkerEvent]) -> String {
        let texts = events.iter().filter_map(|event| match event {
            WorkerEvent::Agent(AgentEvent::Error { message, .. }) => Some(message.clone()),
            WorkerEvent::Session(SessionEvent::Input(input)) => {
                Some(serde_json::to_string(&input.launch).unwrap())
            }
            _ => None,
        });
        texts.collect::<Vec<_>>().join("\n")
    }

    /// A proxy that repeats the key in its errors, reached with a password in the address.
    #[tokio::test]
    async fn an_echoed_key_stays_out_of_reasons_events_and_quota() {
        const KEY: &str = "plain-test-key-0042";
        const PASSWORD: &str = "url-password-77";
        for status in [401, 402] {
            let server = FakeOpenAi::start().unwrap();
            server.push(FakeReply::error(
                status,
                &format!("Incorrect API key provided: {KEY}"),
            ));
            let base = server
                .base_url()
                .replacen("://", &format!("://agent:{PASSWORD}@"), 1);
            let settings = EndpointSettings {
                base_url: Some(base),
                api_key_env: Some("ARGONAUTS_TEST_ENDPOINT_KEY".into()),
                ..EndpointSettings::default()
            };
            let quota = Arc::new(MemoryQuotaBoard::default());
            let worker = EndpointWorker::new(
                "fake",
                &settings,
                WorkerRef::new("fake", Some("fake-model".to_owned())),
                "llama".to_owned(),
                Arc::new(StaticRegistry::default()),
                quota.clone(),
                Arc::new(MemoryHealthBoard::default()),
            );
            let dir = tempfile::tempdir().unwrap();
            let mut spec = session_spec("fake/fake-model", Access::NoTools, dir.path());
            spec.env = vec![("ARGONAUTS_TEST_ENDPOINT_KEY".into(), KEY.into())];
            let handle = worker.start(spec).await.unwrap();
            let (events, end) = tokio::time::timeout(Duration::from_secs(20), drain(handle))
                .await
                .expect("the session ended");
            let reason = format!("{:?}", end.reason);
            assert!(
                matches!(
                    end.reason,
                    EndReason::Auth { .. } | EndReason::Billing { .. }
                ),
                "{reason}"
            );
            assert!(reason.contains("[redacted]"), "{reason}");
            let seen = shown_text(&events);
            assert!(seen.contains("[redacted]"), "{seen}");
            for secret in [KEY, PASSWORD] {
                assert!(!reason.contains(secret), "{reason}");
                assert!(!seen.contains(secret), "{seen}");
            }
            if status == 402 {
                let reading = quota
                    .get(&VendorId("fake".into()))
                    .expect("a quota reading");
                let detail = serde_json::to_string(&reading.detail).unwrap();
                assert!(!detail.contains(KEY), "{detail}");
            }
        }
    }
}
