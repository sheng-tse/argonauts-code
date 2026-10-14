//! One session's exchange: wait for a slot, send the request, stream the reply into events,
//! wait out rate limits the time limit allows, and classify the end. Stop and wrap-up drop
//! the request, which closes the connection.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::events::session::{
    EndReason, SessionEnd, SessionEvent, SessionInput, SessionStarted,
};
use argonauts_core::limits::{
    CostEstimate, HealthBoard, HealthSignal, PlanUsageBoard, PlanUsageReading, PlanUsageSource,
    PlanUsageStatus,
};
use argonauts_core::names::{AgentId, ModelRef, SessionKey};
use argonauts_core::registry::ModelRegistry;
use argonauts_core::session_runner::{RunnerEvent, SessionControl};
use argonauts_core::settings::agents::EndpointApi;
use argonauts_core::{AgentEvent, ErrorCategory, LimitDimension, MessageStage, RetryReason, Spend};
use futures::StreamExt;
use reqwest::header::{CONTENT_TYPE, HeaderMap, RETRY_AFTER};
use serde_json::Value;
use time::OffsetDateTime;
use tokio::sync::{Semaphore, mpsc};
use tokio::time::Instant;

use crate::errors::{self, Failure, Secrets};
use crate::raw_log::RawLog;
use crate::sse::{MAX_EVENT_BYTES, SseDecoder, SseLine};
use crate::stream::{Cutoff, Reply, ReplyStream, StreamFailure};
use crate::structured;

/// Rate-limit waits before giving up, even with time left, so a server that keeps answering
/// `Retry-After: 0` can't hold a session forever.
const MAX_RATE_LIMIT_WAITS: u32 = 5;

/// Error bodies are short; anything longer is cut here.
const ERROR_BODY_LIMIT: usize = 64 * 1024;

/// A reply that isn't streamed is one event, so it's held to the same size.
const MAX_BODY_BYTES: usize = MAX_EVENT_BYTES;

/// What the session reports to, shared by every step of the exchange.
pub(crate) struct Session {
    pub key: SessionKey,
    pub agent_id: AgentId,
    /// The worker and model prices are looked up by.
    pub pricing: ModelRef,
    pub plan_usage: Arc<dyn PlanUsageBoard>,
    pub health: Arc<dyn HealthBoard>,
    pub registry: Arc<dyn ModelRegistry>,
    pub events: mpsc::Sender<RunnerEvent>,
    /// Taken out of every error and plan usage detail the endpoint sends back.
    pub secrets: Secrets,
}

/// Everything the request needs, checked before the session starts.
pub(crate) struct Exchange {
    pub client: reqwest::Client,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Value,
    pub api: EndpointApi,
    pub model: String,
    pub schema: Option<Value>,
    pub slots: Arc<Semaphore>,
    pub limits: Limits,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Limits {
    /// How long one request may take, counted from when it's sent.
    pub timeout: Option<Duration>,
    /// When the session's minutes run out. Only this bounds the wait for a slot and the
    /// waits out rate limits, so time spent queued never counts against a request.
    pub limit: Option<Instant>,
}

impl Limits {
    /// A request sent now ends at its timeout, or when the limit runs out if that's sooner.
    fn request(&self) -> Option<Deadline> {
        let timeout = self.timeout.and_then(|limit| {
            Instant::now()
                .checked_add(limit)
                .map(|at| Deadline::Timeout { at, limit })
        });
        let limit = self.limit.map(Deadline::Limit);
        match (timeout, limit) {
            (Some(timeout), Some(limit)) if limit.at() <= timeout.at() => Some(limit),
            (Some(timeout), _) => Some(timeout),
            (None, limit) => limit,
        }
    }

    fn queue(&self) -> Option<Deadline> {
        self.limit.map(Deadline::Limit)
    }

    fn allows_wait(&self, wait: Duration) -> bool {
        self.limit.is_none_or(|limit| {
            Instant::now()
                .checked_add(wait)
                .is_some_and(|end| end <= limit)
        })
    }
}

/// The limit a wait ran into.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Deadline {
    Limit(Instant),
    Timeout { at: Instant, limit: Duration },
}

impl Deadline {
    fn at(self) -> Instant {
        match self {
            Deadline::Limit(at) | Deadline::Timeout { at, .. } => at,
        }
    }

    /// Only a request that outlives its own timeout is the endpoint's fault. Running out of
    /// the session's minutes is a limit end, which never counts against its health.
    fn missed(self) -> EndReason {
        match self {
            Deadline::Limit(_) => EndReason::LimitHit {
                dimension: LimitDimension::Minutes,
            },
            Deadline::Timeout { limit, .. } => EndReason::CliError {
                category: ErrorCategory::Timeout,
                message: format!("no whole reply within {}s", limit.as_secs()),
            },
        }
    }
}

struct Outcome {
    reason: EndReason,
    reply: Reply,
    structured: Option<Value>,
    /// What the reply got wrong against the schema.
    mismatch: Option<String>,
}

impl Outcome {
    fn ended(reason: EndReason, reply: Reply) -> Self {
        Self {
            reason,
            reply,
            structured: None,
            mismatch: None,
        }
    }

    /// A reply the endpoint said was done: cut off, or whole and checked against `schema`.
    fn completed(schema: Option<&Value>, reply: Reply) -> Self {
        if let Some(cutoff) = &reply.cutoff {
            return Self::ended(cut_off(cutoff), reply);
        }
        let (structured, mismatch) =
            match schema.map(|schema| structured::read(schema, &reply.text)) {
                Some(Ok(value)) => (Some(value), None),
                Some(Err(detail)) => (None, Some(detail)),
                None => (None, None),
            };
        Self {
            reason: EndReason::Completed,
            reply,
            structured,
            mismatch,
        }
    }
}

enum Waited<T> {
    Done(T),
    Halted(EndReason),
}

async fn within<F: Future>(deadline: Option<Deadline>, future: F) -> Result<F::Output, Deadline> {
    match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline.at(), future)
            .await
            .map_err(|_| deadline),
        None => Ok(future.await),
    }
}

/// Runs the session to its end. `plan` is the checked request, or why there can't be one.
/// The events sender drops when this returns, before the caller sends the end.
pub(crate) async fn run(
    session: Session,
    started: SessionStarted,
    input: SessionInput,
    plan: Result<Exchange, EndReason>,
    mut raw: RawLog,
    mut control: mpsc::Receiver<SessionControl>,
    clock: Instant,
) -> SessionEnd {
    session
        .emit(RunnerEvent::Session(SessionEvent::Started(Box::new(
            started,
        ))))
        .await;
    session
        .emit(RunnerEvent::Session(SessionEvent::Input(Box::new(input))))
        .await;
    let outcome = match &plan {
        Ok(exchange) => session.drive(exchange, &mut raw, &mut control).await,
        Err(reason) => Outcome::ended(reason.clone(), Reply::default()),
    };
    raw.flush().await;
    session.report_raw(&mut raw).await;
    session.finish(outcome, &raw, clock).await
}

impl Session {
    async fn emit(&self, event: RunnerEvent) {
        // A reader that has gone away stops the session through `control`, not here.
        let _ = self.events.send(event).await;
    }

    async fn emit_agent(&self, event: AgentEvent) {
        self.emit(RunnerEvent::Agent(event)).await;
    }

    /// Runs `future` while answering `control`: stop, wrap-up, and a dropped sender end the
    /// session; a message waits, since steering only reaches the next turn.
    async fn wait<F: Future>(
        &self,
        future: F,
        control: &mut mpsc::Receiver<SessionControl>,
    ) -> Waited<F::Output> {
        tokio::pin!(future);
        loop {
            tokio::select! {
                biased;
                message = control.recv() => match message {
                    // A reply is one request with nothing in the background, so stopping
                    // the turn alone is stopping.
                    None
                    | Some(
                        SessionControl::Stop | SessionControl::StopTurn | SessionControl::WrapUp,
                    ) => {
                        return Waited::Halted(EndReason::Stopped);
                    }
                    // No tool is ever running, so "after the tool call" is now.
                    Some(SessionControl::StopAfterTool) => return Waited::Halted(EndReason::Paused),
                    Some(SessionControl::Deliver(message)) => {
                        self.emit(RunnerEvent::Session(SessionEvent::Queued(message))).await;
                    }
                    // An endpoint's reply is one request, which nothing can join.
                    Some(SessionControl::Send { uuid, .. }) => {
                        self.emit_agent(AgentEvent::MessageState {
                            uuid,
                            state: MessageStage::NotSent,
                        })
                        .await;
                    }
                    Some(
                        SessionControl::SendNow { .. }
                        | SessionControl::TakeBack { .. }
                        | SessionControl::Screen(_)
                        | SessionControl::AskContext(_)
                        | SessionControl::WatchContext(_)
                        | SessionControl::StopSubagent { .. },
                    ) => {}
                },
                output = &mut future => return Waited::Done(output),
            }
        }
    }

    async fn drive(
        &self,
        exchange: &Exchange,
        raw: &mut RawLog,
        control: &mut mpsc::Receiver<SessionControl>,
    ) -> Outcome {
        let slot = within(
            exchange.limits.queue(),
            exchange.slots.clone().acquire_owned(),
        );
        let _slot = match self.wait(slot, control).await {
            Waited::Done(Ok(Ok(permit))) => permit,
            Waited::Done(Ok(Err(closed))) => {
                let message = format!("the endpoint's request slots closed: {closed}");
                return Outcome::ended(EndReason::StartFailed { message }, Reply::default());
            }
            Waited::Done(Err(missed)) => return Outcome::ended(missed.missed(), Reply::default()),
            Waited::Halted(reason) => return Outcome::ended(reason, Reply::default()),
        };
        let mut waits = 0;
        let mut reply = ReplyStream::new(exchange.api);
        loop {
            let exchanged = within(
                exchange.limits.request(),
                self.exchange_once(exchange, raw, &mut reply),
            );
            let after = match self.wait(exchanged, control).await {
                Waited::Done(Ok(Ok(()))) => {
                    return Outcome::completed(exchange.schema.as_ref(), reply.finish());
                }
                Waited::Done(Ok(Err(Failure::Retry { after }))) => after,
                Waited::Done(Ok(Err(Failure::End(reason)))) => {
                    return Outcome::ended(reason, reply.finish());
                }
                Waited::Done(Err(missed)) => {
                    return Outcome::ended(missed.missed(), reply.finish());
                }
                Waited::Halted(reason) => return Outcome::ended(reason, reply.finish()),
            };
            if waits >= MAX_RATE_LIMIT_WAITS || !exchange.limits.allows_wait(after) {
                return Outcome::ended(EndReason::RateLimited, reply.finish());
            }
            waits += 1;
            self.health.observe(HealthSignal::Retry {
                agent_id: self.agent_id.clone(),
                model: self.pricing.model.clone(),
                session: self.key.clone(),
                reason: RetryReason::RateLimit,
            });
            self.emit_agent(AgentEvent::Retry {
                reason: RetryReason::RateLimit,
                delay_ms: Some(u64::try_from(after.as_millis()).unwrap_or(u64::MAX)),
            })
            .await;
            if let Waited::Halted(reason) = self.wait(tokio::time::sleep(after), control).await {
                return Outcome::ended(reason, reply.finish());
            }
            reply = reply.next_try();
        }
    }

    async fn exchange_once(
        &self,
        exchange: &Exchange,
        raw: &mut RawLog,
        reply: &mut ReplyStream,
    ) -> Result<(), Failure> {
        let transport = |err: reqwest::Error| {
            Failure::End(transport_failure(err, &exchange.url, &self.secrets))
        };
        let mut response = exchange
            .client
            .post(&exchange.url)
            .headers(exchange.headers.clone())
            .json(&exchange.body)
            .send()
            .await
            .map_err(transport)?;
        let status = response.status().as_u16();
        let header = |name| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let retry_after = header(RETRY_AFTER).and_then(|value| errors::parse_retry_after(&value));
        let streamed = header(CONTENT_TYPE).is_some_and(|kind| kind.contains("text/event-stream"));
        if !response.status().is_success() {
            let body = read_capped(&mut response, ERROR_BODY_LIMIT)
                .await
                .map_err(transport)?;
            let body = String::from_utf8_lossy(&body.bytes);
            raw.write_body(&body).await;
            self.report_raw(raw).await;
            let failure =
                errors::classify(status, retry_after, &body, &exchange.model, &self.secrets);
            self.note_plan_usage(&failure, retry_after, &body);
            return Err(failure);
        }
        if !streamed {
            let body = read_capped(&mut response, MAX_BODY_BYTES)
                .await
                .map_err(transport)?;
            if body.cut {
                let mib = MAX_BODY_BYTES / (1024 * 1024);
                return Err(misbehaved(format!("the reply is longer than {mib} MiB")));
            }
            let body = String::from_utf8_lossy(&body.bytes);
            raw.write_body(&body).await;
            self.report_raw(raw).await;
            let events = reply
                .push_body(&body)
                .map_err(|failure| self.stream_failure(failure, &exchange.model))?;
            self.relay(reply, events).await;
            return Ok(());
        }
        let mut decoder = SseDecoder::default();
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            for line in decoder.push(&chunk.map_err(transport)?) {
                self.take(line, raw, reply, &exchange.model).await?;
            }
            overflowed(&decoder)?;
        }
        for line in decoder.finish() {
            self.take(line, raw, reply, &exchange.model).await?;
        }
        overflowed(&decoder)?;
        if !reply.finished() {
            return Err(Failure::End(EndReason::CliError {
                category: ErrorCategory::Network,
                message: "the stream closed before the reply finished".to_owned(),
            }));
        }
        Ok(())
    }

    async fn take(
        &self,
        line: SseLine,
        raw: &mut RawLog,
        reply: &mut ReplyStream,
        model: &str,
    ) -> Result<(), Failure> {
        // The blank line that ends each event is left out, so the log has no empty lines. A
        // replay puts it back after every line, since these streams send one `data:` line an
        // event.
        if let Some(text) = line.raw.as_deref().filter(|text| !text.is_empty()) {
            raw.write(text).await;
            self.report_raw(raw).await;
        }
        if let Some(data) = line.event {
            let events = reply
                .push(&data)
                .map_err(|failure| self.stream_failure(failure, model))?;
            self.relay(reply, events).await;
        }
        Ok(())
    }

    /// Emits `events`, after an `Init` when the reply names a model not yet announced.
    async fn relay(&self, reply: &mut ReplyStream, events: Vec<AgentEvent>) {
        if let Some(model) = reply.announce() {
            self.emit_agent(AgentEvent::Init {
                session_id: None,
                model: Some(model),
                tools: Vec::new(),
                plugins: Vec::new(),
                capabilities: Vec::new(),
                version: None,
            })
            .await;
        }
        for event in events {
            self.emit_agent(event).await;
        }
    }

    fn stream_failure(&self, failure: StreamFailure, model: &str) -> Failure {
        let classified =
            errors::classify(failure.status, None, &failure.body, model, &self.secrets);
        self.note_plan_usage(&classified, None, &failure.body);
        classified
    }

    /// Rate limits and a spent plan or credits go on the plan usage board, with the body as sent
    /// but for credentials.
    fn note_plan_usage(&self, failure: &Failure, retry_after: Option<Duration>, body: &str) {
        let limited = match failure {
            Failure::Retry { .. } => true,
            Failure::End(reason) => matches!(
                reason,
                EndReason::RateLimited | EndReason::UsageLimit { .. } | EndReason::Billing { .. }
            ),
        };
        if !limited {
            return;
        }
        let now = OffsetDateTime::now_utc();
        let mut detail =
            serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.to_owned()));
        self.secrets.redact_json(&mut detail);
        self.plan_usage.observe(PlanUsageReading {
            agent_id: self.agent_id.clone(),
            status: PlanUsageStatus::Rejected,
            used: None,
            resets_at: retry_after.and_then(|after| errors::reset_time(now, after)),
            window: None,
            source: PlanUsageSource::Http,
            observed_at: now,
            detail: Some(detail),
            wrap_up: false,
        });
    }

    async fn report_raw(&self, raw: &mut RawLog) {
        if let Some(message) = raw.take_error() {
            self.emit_agent(AgentEvent::Error {
                category: ErrorCategory::Unknown,
                message,
                retryable: false,
            })
            .await;
        }
    }

    /// The last events, the health signal, and the end.
    async fn finish(&self, outcome: Outcome, raw: &RawLog, clock: Instant) -> SessionEnd {
        let Outcome {
            reason,
            reply,
            structured,
            mismatch,
        } = outcome;
        let (spend, cost) = self.spend(&reply);
        if reason == EndReason::Completed {
            self.emit_agent(AgentEvent::Message {
                text: reply.text.clone(),
                parent: None,
            })
            .await;
        }
        if reply.usage_seen {
            self.emit_agent(AgentEvent::Usage {
                input_tokens: reply.input_tokens,
                output_tokens: reply.output_tokens,
                cached_tokens: reply.cached_tokens,
                cost_usd: cost,
                cumulative: true,
            })
            .await;
        }
        let category = errors::category(&reason);
        match &reason {
            EndReason::Completed => {
                self.health.observe(HealthSignal::Success {
                    agent_id: self.agent_id.clone(),
                    model: self.pricing.model.clone(),
                    session: self.key.clone(),
                });
                if let Some(detail) = &mismatch {
                    // The step runner decides whether to ask again.
                    self.emit_agent(AgentEvent::Error {
                        category: ErrorCategory::InvalidRequest,
                        message: format!("the reply didn't match the output schema: {detail}"),
                        retryable: false,
                    })
                    .await;
                }
                self.emit_agent(AgentEvent::Result {
                    text: Some(reply.text.clone()),
                    structured: structured.clone(),
                    is_error: mismatch.is_some(),
                })
                .await;
            }
            EndReason::Stopped | EndReason::Paused => {}
            other => {
                let retryable =
                    errors::is_health_failure(category) || matches!(other, EndReason::RateLimited);
                if errors::is_health_failure(category) {
                    self.health.observe(HealthSignal::Error {
                        agent_id: self.agent_id.clone(),
                        model: self.pricing.model.clone(),
                        session: self.key.clone(),
                        category,
                    });
                }
                self.emit_agent(AgentEvent::Error {
                    category,
                    message: describe(other),
                    retryable,
                })
                .await;
            }
        }
        let text = (!reply.text.is_empty()).then(|| reply.text.clone());
        SessionEnd {
            cli_session: None,
            exit_code: None,
            signal: None,
            duration_ms: u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX),
            reason: reason.clone(),
            spend,
            tool_calls: 0,
            result_text: (reason == EndReason::Completed).then(|| reply.text.clone()),
            structured,
            last_message: text,
            raw_log: Some(raw.path().to_owned()),
        }
    }

    /// The endpoint's own cost when it says, else the registry's price. The cost is `None`
    /// when neither knows.
    fn spend(&self, reply: &Reply) -> (Spend, Option<f64>) {
        let mut spend = Spend {
            input_tokens: reply.input_tokens,
            output_tokens: reply.output_tokens,
            cached_tokens: reply.cached_tokens,
            ..Spend::default()
        };
        let cost = match reply.cost_usd {
            Some(cost) => Some(cost),
            None => {
                match self
                    .registry
                    .estimate(&self.pricing, reply.input_tokens, reply.output_tokens)
                {
                    CostEstimate::Usd(cost) => {
                        spend.cost_estimated = true;
                        Some(cost)
                    }
                    CostEstimate::Free => Some(0.0),
                    CostEstimate::PlanUsage(_) | CostEstimate::Unknown => None,
                }
            }
        };
        let tokens_unknown = !reply.usage_seen && !reply.text.is_empty();
        let price_unknown = cost.is_none() && reply.input_tokens + reply.output_tokens > 0;
        spend.cost_usd = cost.unwrap_or(0.0);
        spend.cost_partial = tokens_unknown || price_unknown;
        (spend, cost)
    }
}

/// A body read up to a limit, and whether there was more.
struct Capped {
    bytes: Vec<u8>,
    cut: bool,
}

/// Reads no more than `limit` bytes, so a long body can't take all memory. Dropping the
/// response afterwards closes the connection on the rest.
async fn read_capped(
    response: &mut reqwest::Response,
    limit: usize,
) -> Result<Capped, reqwest::Error> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let room = limit - bytes.len();
        if chunk.len() > room {
            bytes.extend_from_slice(&chunk[..room]);
            return Ok(Capped { bytes, cut: true });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Capped { bytes, cut: false })
}

fn overflowed(decoder: &SseDecoder) -> Result<(), Failure> {
    match decoder.overflow() {
        Some(overflow) => Err(misbehaved(overflow.to_string())),
        None => Ok(()),
    }
}

fn misbehaved(message: String) -> Failure {
    Failure::End(EndReason::CliError {
        category: ErrorCategory::Server,
        message,
    })
}

/// reqwest's own text names the full URL, which may carry a user, password, or key in its query,
/// so the URL is dropped from it and shown cleaned.
fn transport_failure(err: reqwest::Error, url: &str, secrets: &Secrets) -> EndReason {
    let category = if err.is_timeout() {
        ErrorCategory::Timeout
    } else {
        ErrorCategory::Network
    };
    let what = if err.is_connect() {
        format!("couldn't reach {}", errors::shown_url(url))
    } else {
        "the connection failed".to_owned()
    };
    let message = format!("{what}: {}", err.without_url());
    EndReason::CliError {
        category,
        message: secrets.redact(&message),
    }
}

/// A cut-off reply ends the session with why, rather than as a whole reply the schema check
/// or the next step would misread. Its text stays in the end's last message.
fn cut_off(cutoff: &Cutoff) -> EndReason {
    let (category, message) = match cutoff {
        Cutoff::TokenLimit => (
            ErrorCategory::ContextFull,
            "the reply was cut off at the model's output token limit".to_owned(),
        ),
        // The category a content filter gets when it refuses the prompt with a 400.
        Cutoff::ContentFilter => (
            ErrorCategory::InvalidRequest,
            "the endpoint's content filter cut the reply off".to_owned(),
        ),
        Cutoff::Other(Some(reason)) => (
            ErrorCategory::Unknown,
            format!("the endpoint left the reply incomplete: {reason}"),
        ),
        Cutoff::Other(None) => (
            ErrorCategory::Unknown,
            "the endpoint left the reply incomplete".to_owned(),
        ),
    };
    EndReason::CliError { category, message }
}

fn describe(reason: &EndReason) -> String {
    match reason {
        EndReason::Auth { message }
        | EndReason::Billing { message }
        | EndReason::CliError { message, .. }
        | EndReason::StartFailed { message }
        | EndReason::Crashed { message } => message.clone(),
        EndReason::UsageLimit { .. } => "the endpoint's usage limit is reached".to_owned(),
        EndReason::RateLimited => "rate limited for longer than the session can wait".to_owned(),
        EndReason::ContextFull => "the prompt is longer than the model's context".to_owned(),
        EndReason::ModelUnavailable { model } => format!("the endpoint has no model {model}"),
        EndReason::LimitHit { .. } => "the session's time ran out".to_owned(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;
    use argonauts_testkit::services::{MemoryHealthBoard, MemoryPlanUsageBoard, StaticRegistry};
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const MINUTE: Duration = Duration::from_secs(60);

    /// Reads one request, headers and then the body its length names.
    async fn read_request(socket: &mut tokio::net::TcpStream) {
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let read = socket.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&request);
            let whole = text.find("\r\n\r\n").is_some_and(|end| {
                let length = text[..end]
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                request.len() >= end + 4 + length
            });
            if whole || read == 0 {
                return;
            }
        }
    }

    /// Answers one request with `payloads` as an event stream.
    async fn serve(payloads: Vec<Value>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let body: String = payloads
            .iter()
            .map(|payload| format!("data: {payload}\n\n"))
            .collect();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
        });
        url
    }

    /// A server that answers one request with a 500 and `body`, and gives up quietly once the
    /// reader goes away.
    fn serve_error(body: Vec<u8>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.read(&mut [0; 4096]);
            let head = format!(
                "HTTP/1.1 500 Internal Server Error\r\ncontent-length: {}\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        });
        url
    }

    /// Runs one exchange against `payloads` and returns its agent events and end.
    async fn exchange(
        api: EndpointApi,
        schema: Option<Value>,
        payloads: Vec<Value>,
    ) -> (Vec<AgentEvent>, SessionEnd) {
        let (events, mut received) = mpsc::channel(64);
        let session = Session {
            key: SessionKey::numbered(1),
            agent_id: AgentId("fake".to_owned()),
            pricing: ModelRef::new("fake", Some("m".to_owned())),
            plan_usage: Arc::new(MemoryPlanUsageBoard::default()),
            health: Arc::new(MemoryHealthBoard::default()),
            registry: Arc::new(StaticRegistry::default()),
            events,
            secrets: Secrets::default(),
        };
        let exchange = Exchange {
            client: reqwest::Client::new(),
            url: serve(payloads).await,
            headers: HeaderMap::new(),
            body: json!({"model": "m"}),
            api,
            model: "m".to_owned(),
            schema,
            slots: Arc::new(Semaphore::new(1)),
            limits: Limits {
                timeout: None,
                limit: None,
            },
        };
        let dir = tempfile::tempdir().unwrap();
        let mut raw = RawLog::open(&dir.path().join("raw.log"), Vec::new())
            .await
            .unwrap();
        let (_control, mut control) = mpsc::channel(1);
        let outcome = session.drive(&exchange, &mut raw, &mut control).await;
        let end = session.finish(outcome, &raw, Instant::now()).await;
        drop(session);
        let mut agent = Vec::new();
        while let Some(event) = received.recv().await {
            if let RunnerEvent::Agent(event) = event {
                agent.push(event);
            }
        }
        (agent, end)
    }

    fn chunk(content: &str, finish: Option<&str>) -> Value {
        json!({"model": "agent/routed",
               "choices": [{"index": 0, "delta": {"content": content}, "finish_reason": finish}]})
    }

    fn error_events(events: &[AgentEvent]) -> Vec<(ErrorCategory, &str)> {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Error {
                    category, message, ..
                } => Some((*category, message.as_str())),
                _ => None,
            })
            .collect()
    }

    fn results(events: &[AgentEvent]) -> Vec<bool> {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Result { is_error, .. } => Some(*is_error),
                _ => None,
            })
            .collect()
    }

    fn verdict_schema() -> Value {
        json!({"type": "object", "required": ["verdict"],
               "properties": {"verdict": {"type": "string"}}})
    }

    #[tokio::test]
    async fn the_served_model_is_announced_before_its_text() {
        let payloads = vec![chunk("Hi ", None), chunk("there", Some("stop"))];
        let (events, end) = exchange(EndpointApi::ChatCompletions, None, payloads).await;
        assert_eq!(
            events[0],
            AgentEvent::Init {
                session_id: None,
                model: Some("agent/routed".to_owned()),
                tools: Vec::new(),
                plugins: Vec::new(),
                capabilities: Vec::new(),
                version: None,
            }
        );
        let inits = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::Init { .. }))
            .count();
        assert_eq!(inits, 1);
        assert_eq!(end.reason, EndReason::Completed);
        assert_eq!(end.result_text.as_deref(), Some("Hi there"));
    }

    #[tokio::test]
    async fn a_schema_mismatch_says_what_failed() {
        let payloads = vec![chunk(r#"{"verdict": 3}"#, Some("stop"))];
        let (events, end) = exchange(
            EndpointApi::ChatCompletions,
            Some(verdict_schema()),
            payloads,
        )
        .await;
        let [(category, message)] = error_events(&events)[..] else {
            panic!("expected one error in {events:?}");
        };
        assert_eq!(category, ErrorCategory::InvalidRequest);
        assert!(message.contains("/verdict"), "{message}");
        assert!(message.contains("string"), "{message}");
        assert_eq!(results(&events), [true]);
        assert_eq!(end.reason, EndReason::Completed);
        assert_eq!(end.structured, None);

        let payloads = vec![chunk(r#"{"verdict": "ok"}"#, Some("stop"))];
        let (events, end) = exchange(
            EndpointApi::ChatCompletions,
            Some(verdict_schema()),
            payloads,
        )
        .await;
        assert!(error_events(&events).is_empty());
        assert_eq!(results(&events), [false]);
        assert_eq!(end.structured, Some(json!({"verdict": "ok"})));
    }

    #[tokio::test]
    async fn cut_off_replies_end_with_why() {
        let cut_at_length = vec![chunk(r#"{"verdict": "o"#, Some("length"))];
        let filtered = vec![chunk("I can", Some("content_filter"))];
        let incomplete = |reason: &str| {
            vec![
                json!({"type": "response.output_text.delta", "delta": "{\"verd"}),
                json!({"type": "response.incomplete", "response": {"model": "m",
                    "status": "incomplete", "incomplete_details": {"reason": reason}, "output": []}}),
            ]
        };
        let cases = [
            (
                EndpointApi::ChatCompletions,
                cut_at_length,
                ErrorCategory::ContextFull,
            ),
            (
                EndpointApi::ChatCompletions,
                filtered,
                ErrorCategory::InvalidRequest,
            ),
            (
                EndpointApi::Responses,
                incomplete("max_output_tokens"),
                ErrorCategory::ContextFull,
            ),
            (
                EndpointApi::Responses,
                incomplete("content_filter"),
                ErrorCategory::InvalidRequest,
            ),
            (
                EndpointApi::Responses,
                incomplete("server_shutdown"),
                ErrorCategory::Unknown,
            ),
        ];
        for (api, payloads, category) in cases {
            let (events, end) = exchange(api, Some(verdict_schema()), payloads).await;
            let EndReason::CliError {
                category: ended,
                message,
            } = &end.reason
            else {
                panic!("a cut-off reply ended as {:?}", end.reason);
            };
            assert_eq!(*ended, category, "{message}");
            assert!(message.contains("cut") || message.contains("incomplete"));
            assert_eq!(error_events(&events), [(category, message.as_str())]);
            assert!(results(&events).is_empty());
            assert_eq!(end.result_text, None);
            assert_eq!(end.structured, None);
            assert!(end.last_message.is_some());
        }
    }

    #[test]
    fn each_request_gets_its_whole_timeout_unless_the_limit_ends_first() {
        let timeout = Some(5 * MINUTE);
        let far = Limits {
            timeout,
            limit: Some(Instant::now() + 60 * MINUTE),
        };
        let Some(Deadline::Timeout { at, limit }) = far.request() else {
            panic!("the timeout comes first");
        };
        assert_eq!(limit, 5 * MINUTE);
        assert!(at > Instant::now() + 4 * MINUTE);

        let near = Instant::now() + MINUTE;
        let close = Limits {
            timeout,
            limit: Some(near),
        };
        assert_eq!(close.request(), Some(Deadline::Limit(near)));
        assert_eq!(close.queue(), Some(Deadline::Limit(near)));
        assert!(matches!(
            close.request().unwrap().missed(),
            EndReason::LimitHit {
                dimension: LimitDimension::Minutes
            }
        ));

        let timeout_only = Limits {
            timeout,
            limit: None,
        };
        assert_eq!(
            timeout_only.queue(),
            None,
            "the timeout never bounds a queue"
        );
        assert!(timeout_only.allows_wait(10 * MINUTE));
        assert!(!close.allows_wait(2 * MINUTE));
        assert!(!close.allows_wait(Duration::MAX));
    }

    #[tokio::test]
    async fn transport_errors_hide_the_url_credentials() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let url =
            format!("http://agent:hunter22@127.0.0.1:{port}/v1/chat/completions?key=q-secret");
        let err = reqwest::Client::new().post(&url).send().await.unwrap_err();
        let secrets = Secrets::new(vec!["hunter22".into()]);
        let EndReason::CliError { message, .. } = transport_failure(err, &url, &secrets) else {
            panic!("expected an agent error");
        };
        assert!(!message.contains("hunter22"), "{message}");
        assert!(!message.contains("q-secret"), "{message}");
        assert!(message.contains(&format!("127.0.0.1:{port}/v1/chat/completions")));
    }

    #[test]
    fn cut_off_replies_end_as_errors_that_are_not_retried() {
        for (cutoff, category) in [
            (Cutoff::TokenLimit, ErrorCategory::ContextFull),
            (Cutoff::ContentFilter, ErrorCategory::InvalidRequest),
            (
                Cutoff::Other(Some("odd".to_owned())),
                ErrorCategory::Unknown,
            ),
            (Cutoff::Other(None), ErrorCategory::Unknown),
        ] {
            let reason = cut_off(&cutoff);
            assert!(
                matches!(&reason, EndReason::CliError { category: c, .. } if *c == category),
                "{reason:?}"
            );
            assert!(!errors::is_health_failure(errors::category(&reason)));
        }
    }

    #[tokio::test]
    async fn a_body_is_read_only_up_to_its_limit() {
        let mut response = reqwest::get(serve_error(vec![b'x'; 1024 * 1024]))
            .await
            .unwrap();
        let capped = read_capped(&mut response, 1000).await.unwrap();
        assert_eq!((capped.bytes.len(), capped.cut), (1000, true));

        let mut response = reqwest::get(serve_error(b"short".to_vec())).await.unwrap();
        let capped = read_capped(&mut response, 5).await.unwrap();
        assert_eq!(
            (capped.bytes.as_slice(), capped.cut),
            (&b"short"[..], false)
        );
    }
}
