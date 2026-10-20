//! A local model server that answers as Ollama, LM Studio, llama-server or vLLM do in their own
//! APIs: what it serves, what it holds in memory, and the calls that load and free a model. It
//! records every request and runs on plain threads, like [`crate::FakeOpenAi`].

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Context as _;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Which server it answers as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeKind {
    Ollama,
    LmStudio,
    /// `slots` is `/props`' `total_slots`. A router serves several models and can drop one.
    LlamaServer {
        slots: u32,
        router: bool,
    },
    Vllm {
        max_model_len: u64,
    },
    /// Some other app with an OpenAI-style model list, such as a dev server on port 8000.
    Other,
}

/// A model held in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeLoaded {
    pub name: String,
    pub size: u64,
    pub context_length: u64,
    /// RFC 3339, as Ollama's `/api/ps` writes it.
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerRequest {
    pub method: String,
    pub path: String,
    pub body: Value,
}

struct State {
    kind: FakeKind,
    models: Vec<String>,
    loaded: Vec<FakeLoaded>,
    /// What a model takes once loaded, for loads the fake does itself.
    size: u64,
    context_length: u64,
    requests: Vec<ServerRequest>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Stops on drop.
pub struct FakeModelServer {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeModelServer {
    /// Serves `models`, holding none of them yet.
    pub fn start(kind: FakeKind, models: &[&str]) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").context("binding the fake model server")?;
        let addr = listener.local_addr().context("reading its port")?;
        let state = Arc::new(Mutex::new(State {
            kind,
            models: models.iter().map(|model| (*model).to_owned()).collect(),
            loaded: Vec::new(),
            size: 2_000_000_000,
            context_length: 4096,
            requests: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (state, stop) = (state.clone(), stop.clone());
            thread::Builder::new()
                .name("argonauts-fake-model-server".to_owned())
                .spawn(move || accept(&listener, &state, &stop))
                .context("starting the fake model server")?
        };
        Ok(Self {
            addr,
            state,
            stop,
            thread: Some(thread),
        })
    }

    /// The OpenAI-style base URL, such as `http://127.0.0.1:49152/v1`.
    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Holds `name` as if someone had used it, expiring at `expires_at`.
    pub fn hold(&self, name: &str, size: u64, expires_at: OffsetDateTime) {
        let mut state = lock(&self.state);
        let context_length = state.context_length;
        state.loaded.retain(|loaded| loaded.name != name);
        state.loaded.push(FakeLoaded {
            name: name.to_owned(),
            size,
            context_length,
            expires_at: rfc3339(expires_at),
        });
    }

    /// What a model the fake loads itself takes, and its window.
    pub fn set_load(&self, size: u64, context_length: u64) {
        let mut state = lock(&self.state);
        state.size = size;
        state.context_length = context_length;
    }

    pub fn loaded(&self) -> Vec<FakeLoaded> {
        lock(&self.state).loaded.clone()
    }

    pub fn requests(&self) -> Vec<ServerRequest> {
        lock(&self.state).requests.clone()
    }
}

impl Drop for FakeModelServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn rfc3339(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap_or_default()
}

fn accept(listener: &TcpListener, state: &Arc<Mutex<State>>, stop: &AtomicBool) {
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(stream) = stream else { continue };
        let state = state.clone();
        let _ = thread::Builder::new()
            .name("argonauts-fake-model-server-conn".to_owned())
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
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim_end().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let request = ServerRequest {
        method,
        path,
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    };
    let answer = {
        let mut state = lock(state);
        state.requests.push(request.clone());
        answer(&mut state, &request)
    };
    let (status, body) = answer.unwrap_or((404, json!({"error": "not found"})));
    let body = body.to_string();
    let mut out = stream;
    write!(
        out,
        "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{body}",
        if status == 200 { "OK" } else { "Not Found" },
        body.len()
    )?;
    out.flush()
}

fn answer(state: &mut State, request: &ServerRequest) -> Option<(u16, Value)> {
    let path = request.path.split('?').next().unwrap_or_default();
    let get = request.method == "GET";
    let body = &request.body;
    let ok = |value: Value| Some((200, value));
    match (state.kind.clone(), get, path) {
        (FakeKind::Ollama, true, "/api/tags") => {
            let models: Vec<Value> = state
                .models
                .iter()
                .map(|name| json!({"name": name, "model": name}))
                .collect();
            ok(json!({ "models": models }))
        }
        (FakeKind::Ollama, true, "/api/ps") => {
            let models: Vec<Value> = state
                .loaded
                .iter()
                .map(|loaded| {
                    json!({
                        "name": loaded.name,
                        "model": loaded.name,
                        "size": loaded.size,
                        "size_vram": loaded.size,
                        "context_length": loaded.context_length,
                        "expires_at": loaded.expires_at,
                    })
                })
                .collect();
            ok(json!({ "models": models }))
        }
        (FakeKind::Ollama, false, "/api/generate") => {
            let name = body["model"].as_str()?.to_owned();
            if body["keep_alive"].as_i64() == Some(0) || body["keep_alive"].as_str() == Some("0") {
                state.loaded.retain(|loaded| loaded.name != name);
                return ok(json!({"model": name, "done": true, "done_reason": "unload"}));
            }
            if !state.models.contains(&name) {
                return Some((404, json!({"error": format!("model '{name}' not found")})));
            }
            let expires = OffsetDateTime::now_utc() + time::Duration::minutes(5);
            let (size, context_length) = (state.size, state.context_length);
            state.loaded.retain(|loaded| loaded.name != name);
            state.loaded.push(FakeLoaded {
                name: name.clone(),
                size,
                context_length,
                expires_at: rfc3339(expires),
            });
            ok(json!({"model": name, "done": true, "done_reason": "load"}))
        }
        (FakeKind::LmStudio, true, "/api/v1/models") => {
            let models: Vec<Value> = state
                .models
                .iter()
                .map(|key| {
                    let instances: Vec<Value> = state
                        .loaded
                        .iter()
                        .filter(|loaded| &loaded.name == key)
                        .map(|loaded| {
                            json!({"id": loaded.name,
                                   "config": {"context_length": loaded.context_length}})
                        })
                        .collect();
                    json!({"type": "llm", "key": key, "max_context_length": 32768,
                           "loaded_instances": instances})
                })
                .collect();
            ok(json!({ "models": models }))
        }
        (FakeKind::LmStudio, false, "/api/v1/models/unload") => {
            let id = body["instance_id"].as_str()?.to_owned();
            state.loaded.retain(|loaded| loaded.name != id);
            ok(json!({ "instance_id": id }))
        }
        (FakeKind::LlamaServer { slots, .. }, true, "/props") => ok(json!({
            "default_generation_settings": {"n_ctx": state.context_length},
            "total_slots": slots,
            "build_info": "b7000-fake",
        })),
        (FakeKind::LlamaServer { router: true, .. }, true, "/models") => {
            let data: Vec<Value> = state
                .models
                .iter()
                .map(|id| {
                    let value = if state.loaded.iter().any(|loaded| &loaded.name == id) {
                        "loaded"
                    } else {
                        "unloaded"
                    };
                    json!({"id": id, "status": {"value": value}})
                })
                .collect();
            ok(json!({ "data": data }))
        }
        (FakeKind::LlamaServer { router: true, .. }, false, "/models/unload") => {
            let model = body["model"].as_str()?.to_owned();
            state.loaded.retain(|loaded| loaded.name != model);
            ok(json!({ "success": true }))
        }
        (kind, true, "/v1/models") => {
            let data: Vec<Value> = state
                .models
                .iter()
                .map(|id| match &kind {
                    FakeKind::Vllm { max_model_len } => {
                        json!({"id": id, "object": "model", "owned_by": "vllm",
                               "max_model_len": max_model_len})
                    }
                    FakeKind::LlamaServer { .. } => {
                        json!({"id": id, "object": "model", "owned_by": "llamacpp"})
                    }
                    FakeKind::Ollama => json!({"id": id, "object": "model", "owned_by": "library"}),
                    FakeKind::LmStudio | FakeKind::Other => {
                        json!({"id": id, "object": "model", "owned_by": "someone"})
                    }
                })
                .collect();
            ok(json!({"object": "list", "data": data}))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(server: &FakeModelServer, method: &str, path: &str, body: &Value) -> Value {
        let mut stream = TcpStream::connect(server.addr).unwrap();
        let body = body.to_string();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nhost: x\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut text = String::new();
        stream.read_to_string(&mut text).unwrap();
        let (_, body) = text.split_once("\r\n\r\n").unwrap();
        serde_json::from_str(body).unwrap_or(Value::Null)
    }

    #[test]
    fn an_ollama_loads_and_frees_with_its_own_calls() {
        let server = FakeModelServer::start(FakeKind::Ollama, &["llama3.2:latest"]).unwrap();
        server.set_load(23_779_979_264, 131_072);
        assert_eq!(
            call(&server, "GET", "/api/ps", &Value::Null)["models"],
            json!([])
        );
        let load = call(
            &server,
            "POST",
            "/api/generate",
            &json!({"model": "llama3.2:latest"}),
        );
        assert_eq!(load["done_reason"], "load");
        let ps = call(&server, "GET", "/api/ps", &Value::Null);
        assert_eq!(ps["models"][0]["context_length"], 131_072);
        let body = json!({"model": "llama3.2:latest", "keep_alive": 0});
        assert_eq!(
            call(&server, "POST", "/api/generate", &body)["done_reason"],
            "unload"
        );
        assert!(server.loaded().is_empty());
    }

    #[test]
    fn each_kind_says_what_it_is() {
        let vllm = FakeModelServer::start(
            FakeKind::Vllm {
                max_model_len: 32768,
            },
            &["Qwen/Q"],
        )
        .unwrap();
        let list = call(&vllm, "GET", "/v1/models", &Value::Null);
        assert_eq!(list["data"][0]["owned_by"], "vllm");
        let llama = FakeKind::LlamaServer {
            slots: 4,
            router: false,
        };
        let llama = FakeModelServer::start(llama, &["m.gguf"]).unwrap();
        assert_eq!(
            call(&llama, "GET", "/props", &Value::Null)["total_slots"],
            4
        );
        let other = FakeModelServer::start(FakeKind::Other, &["x"]).unwrap();
        assert_eq!(
            call(&other, "GET", "/props", &Value::Null)["error"],
            "not found"
        );
    }
}
