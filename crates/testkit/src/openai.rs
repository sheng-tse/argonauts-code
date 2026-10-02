//! A local OpenAI-compatible server with scripted replies that records every request. It runs
//! on plain threads, so it works with or without a Tokio runtime in the test.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Context as _;
use argonauts_core::settings::agents::EndpointSettings;
use serde_json::{Value, json};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Clone, PartialEq)]
pub enum FakeReply {
    /// A message, streamed word by word when the request asks for a stream.
    Text {
        text: String,
        input_tokens: u64,
        output_tokens: u64,
    },
    /// An HTTP error with an OpenAI-style error body.
    Error {
        status: u16,
        message: String,
        /// In seconds.
        retry_after: Option<u64>,
    },
    Delay(Duration, Box<FakeReply>),
}

impl FakeReply {
    /// A message with small made-up token counts.
    pub fn text(text: &str) -> Self {
        Self::Text {
            text: text.to_owned(),
            input_tokens: 12,
            output_tokens: text.split_whitespace().count() as u64,
        }
    }

    pub fn json(value: &Value) -> Self {
        Self::text(&value.to_string())
    }

    pub fn error(status: u16, message: &str) -> Self {
        Self::Error {
            status,
            message: message.to_owned(),
            retry_after: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordedRequest {
    pub method: String,
    /// Such as `/v1/chat/completions`.
    pub path: String,
    /// Headers, names lowercased.
    pub headers: Vec<(String, String)>,
    /// The JSON body, or `null`.
    pub body: Value,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Default)]
struct State {
    replies: VecDeque<FakeReply>,
    requests: Vec<RecordedRequest>,
    models: Vec<String>,
}

/// Stops on drop.
pub struct FakeOpenAi {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeOpenAi {
    pub fn start() -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").context("binding the fake server")?;
        let addr = listener
            .local_addr()
            .context("reading the fake server's port")?;
        let state = Arc::new(Mutex::new(State {
            models: vec!["fake-model".to_owned()],
            ..State::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let state = state.clone();
            let stop = stop.clone();
            thread::Builder::new()
                .name("argonauts-fake-openai".to_owned())
                .spawn(move || accept(&listener, &state, &stop))
                .context("starting the fake server")?
        };
        Ok(Self {
            addr,
            state,
            stop,
            thread: Some(thread),
        })
    }

    /// The base URL, such as `http://127.0.0.1:49152/v1`.
    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    /// `[endpoints.<id>]` pointed at this server, for the real endpoint worker.
    pub fn endpoint_settings(&self) -> EndpointSettings {
        EndpointSettings {
            base_url: Some(self.base_url()),
            ..EndpointSettings::default()
        }
    }

    /// Queues the answer to the next model request.
    pub fn push(&self, reply: FakeReply) {
        lock(&self.state).replies.push_back(reply);
    }

    /// Sets what `GET /v1/models` lists.
    pub fn set_models(&self, models: &[&str]) {
        lock(&self.state).models = models.iter().map(|m| (*m).to_owned()).collect();
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        lock(&self.state).requests.clone()
    }
}

impl Drop for FakeOpenAi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the flag.
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn accept(listener: &TcpListener, state: &Arc<Mutex<State>>, stop: &AtomicBool) {
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(stream) = stream else { continue };
        let state = state.clone();
        let _ = thread::Builder::new()
            .name("argonauts-fake-openai-conn".to_owned())
            .spawn(move || {
                let _ = serve(stream, &state);
            });
    }
}

fn serve(stream: TcpStream, state: &Mutex<State>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let request = RecordedRequest {
        method,
        path,
        headers,
        body,
    };
    let (reply, models) = {
        let mut state = lock(state);
        state.requests.push(request.clone());
        let reply = if request.method == "POST" {
            state.replies.pop_front()
        } else {
            None
        };
        (reply, state.models.clone())
    };
    let mut out = stream;
    respond(&mut out, &request, reply, &models)
}

fn respond(
    out: &mut TcpStream,
    request: &RecordedRequest,
    reply: Option<FakeReply>,
    models: &[String],
) -> std::io::Result<()> {
    let path = request.path.split('?').next().unwrap_or_default();
    if request.method == "GET" && path.ends_with("/models") {
        let data: Vec<Value> = models
            .iter()
            .map(|id| json!({"id": id, "object": "model", "owned_by": "fake"}))
            .collect();
        return send_json(out, 200, &[], &json!({"object": "list", "data": data}));
    }
    let chat = path.ends_with("/chat/completions");
    let responses = path.ends_with("/responses");
    if !(chat || responses) {
        return send_error(out, 404, &format!("no route {path}"), None);
    }
    let mut reply = reply;
    while let Some(FakeReply::Delay(wait, next)) = reply {
        thread::sleep(wait);
        reply = Some(*next);
    }
    let Some(reply) = reply else {
        return send_error(out, 500, "the fake server has no scripted reply", None);
    };
    let model = request.body["model"].as_str().unwrap_or("fake-model");
    let stream = request.body["stream"].as_bool().unwrap_or(false);
    match reply {
        FakeReply::Error {
            status,
            message,
            retry_after,
        } => send_error(out, status, &message, retry_after),
        FakeReply::Text {
            text,
            input_tokens,
            output_tokens,
        } => {
            let usage = (input_tokens, output_tokens);
            match (chat, stream) {
                (true, false) => send_json(out, 200, &[], &chat_completion(model, &text, usage)),
                (true, true) => send_sse(out, &chat_chunks(model, &text, usage)),
                (false, false) => send_json(out, 200, &[], &response_object(model, &text, usage)),
                (false, true) => send_sse(out, &response_events(model, &text, usage)),
            }
        }
        FakeReply::Delay(..) => send_error(out, 500, "nested delays", None),
    }
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

fn send_json(
    out: &mut TcpStream,
    status: u16,
    extra_headers: &[(&str, String)],
    body: &Value,
) -> std::io::Result<()> {
    let body = body.to_string();
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n",
        status_text(status),
        body.len()
    );
    for (name, value) in extra_headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())?;
    out.write_all(body.as_bytes())?;
    out.flush()
}

fn send_error(
    out: &mut TcpStream,
    status: u16,
    message: &str,
    retry_after: Option<u64>,
) -> std::io::Result<()> {
    let headers: Vec<(&str, String)> = retry_after
        .map(|seconds| vec![("retry-after", seconds.to_string())])
        .unwrap_or_default();
    let kind = match status {
        401 | 403 => "authentication_error",
        402 => "insufficient_quota",
        429 => "rate_limit_exceeded",
        400..=499 => "invalid_request_error",
        _ => "server_error",
    };
    let body = json!({"error": {"message": message, "type": kind, "code": kind}});
    send_json(out, status, &headers, &body)
}

/// Sends server-sent events, each `(event name, data)`; the name is left out when empty.
fn send_sse(out: &mut TcpStream, events: &[(String, String)]) -> std::io::Result<()> {
    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\n\
                connection: close\r\n\r\n";
    out.write_all(head.as_bytes())?;
    for (name, data) in events {
        if !name.is_empty() {
            out.write_all(format!("event: {name}\n").as_bytes())?;
        }
        out.write_all(format!("data: {data}\n\n").as_bytes())?;
        out.flush()?;
    }
    Ok(())
}

/// The text in pieces of about one word each, the way deltas arrive.
fn pieces(text: &str) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        current.push(c);
        if c == ' ' {
            pieces.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

fn chat_completion(model: &str, text: &str, (input, output): (u64, u64)) -> Value {
    json!({
        "id": "chatcmpl-fake", "object": "chat.completion", "created": 0, "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": input, "completion_tokens": output,
                  "total_tokens": input + output},
    })
}

fn chat_chunks(model: &str, text: &str, (input, output): (u64, u64)) -> Vec<(String, String)> {
    let chunk = |delta: Value, finish: Value| {
        json!({"id": "chatcmpl-fake", "object": "chat.completion.chunk", "created": 0,
               "model": model,
               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
        .to_string()
    };
    let mut events = vec![(
        String::new(),
        chunk(json!({"role": "assistant", "content": ""}), Value::Null),
    )];
    for piece in pieces(text) {
        events.push((String::new(), chunk(json!({"content": piece}), Value::Null)));
    }
    events.push((String::new(), chunk(json!({}), json!("stop"))));
    let usage = json!({"id": "chatcmpl-fake", "object": "chat.completion.chunk", "created": 0,
                       "model": model, "choices": [],
                       "usage": {"prompt_tokens": input, "completion_tokens": output,
                                 "total_tokens": input + output}});
    events.push((String::new(), usage.to_string()));
    events.push((String::new(), "[DONE]".to_owned()));
    events
}

fn response_object(model: &str, text: &str, (input, output): (u64, u64)) -> Value {
    json!({
        "id": "resp_fake", "object": "response", "created_at": 0, "status": "completed",
        "model": model,
        "output": [{"type": "message", "id": "msg_fake", "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": text, "annotations": []}]}],
        "usage": {"input_tokens": input, "output_tokens": output,
                  "total_tokens": input + output},
    })
}

fn response_events(model: &str, text: &str, usage: (u64, u64)) -> Vec<(String, String)> {
    let mut in_progress = response_object(model, "", usage);
    in_progress["status"] = json!("in_progress");
    in_progress["output"] = json!([]);
    let mut events = vec![(
        "response.created".to_owned(),
        json!({"type": "response.created", "response": in_progress}).to_string(),
    )];
    for piece in pieces(text) {
        events.push((
            "response.output_text.delta".to_owned(),
            json!({"type": "response.output_text.delta", "item_id": "msg_fake",
                   "output_index": 0, "content_index": 0, "delta": piece})
            .to_string(),
        ));
    }
    events.push((
        "response.output_text.done".to_owned(),
        json!({"type": "response.output_text.done", "item_id": "msg_fake", "output_index": 0,
               "content_index": 0, "text": text})
        .to_string(),
    ));
    events.push((
        "response.completed".to_owned(),
        json!({"type": "response.completed", "response": response_object(model, text, usage)})
            .to_string(),
    ));
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny blocking HTTP client, so the test needs no HTTP crate.
    fn call(server: &FakeOpenAi, method: &str, path: &str, body: &Value) -> (u16, String) {
        let url = server.base_url();
        let host = url.trim_start_matches("http://").trim_end_matches("/v1");
        let mut stream = TcpStream::connect(host).unwrap();
        let body = body.to_string();
        write!(
            stream,
            "{method} /v1{path} HTTP/1.1\r\nhost: {host}\r\nauthorization: Bearer test-key\r\n\
             content-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response[9..12].parse().unwrap();
        let body = response.split_once("\r\n\r\n").unwrap().1.to_owned();
        (status, body)
    }

    #[test]
    fn answers_chat_completions_whole_and_streamed() {
        let server = FakeOpenAi::start().unwrap();
        server.push(FakeReply::text("hello there"));
        let (status, body) = call(
            &server,
            "POST",
            "/chat/completions",
            &json!({"model": "m", "messages": []}),
        );
        assert_eq!(status, 200);
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["choices"][0]["message"]["content"], "hello there");

        server.push(FakeReply::text("hello there"));
        let (_, body) = call(
            &server,
            "POST",
            "/chat/completions",
            &json!({"model": "m", "stream": true}),
        );
        assert!(body.contains("\"content\":\"hello \""), "{body}");
        assert!(body.trim_end().ends_with("data: [DONE]"));

        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].header("authorization"), Some("Bearer test-key"));
        assert_eq!(requests[1].body["stream"], true);
    }

    #[test]
    fn answers_responses_and_errors_and_lists_models() {
        let server = FakeOpenAi::start().unwrap();
        server.push(FakeReply::json(&json!({"verdict": "approve"})));
        let (status, body) = call(&server, "POST", "/responses", &json!({"model": "m"}));
        assert_eq!(status, 200);
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            value["output"][0]["content"][0]["text"],
            "{\"verdict\":\"approve\"}"
        );

        server.push(FakeReply::Error {
            status: 429,
            message: "slow down".into(),
            retry_after: Some(2),
        });
        let (status, body) = call(&server, "POST", "/responses", &json!({"stream": true}));
        assert_eq!(status, 429);
        assert!(body.contains("slow down"));

        let (status, _) = call(&server, "POST", "/chat/completions", &json!({}));
        assert_eq!(status, 500, "no scripted reply left");

        server.set_models(&["a", "b"]);
        let (_, body) = call(&server, "GET", "/models", &Value::Null);
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["data"].as_array().unwrap().len(), 2);
    }
}
