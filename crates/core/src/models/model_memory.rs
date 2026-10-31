//! The memory a local model server gives the models Argonauts Code uses. Ollama keeps a model
//! five minutes after its last request, and a 30B model at 64k holds 25 GB, so when the last
//! run or conversation using a model ends, Argonauts lets go of the ones it loaded itself.
//!
//! Before its first request for a model, a session notes whether the server already held it.
//! One held before is yours and is never freed. Every run and conversation that uses a model is
//! written down in one record the app and its runs share, and the model goes only when the last
//! of them ends. The servers' own calls live with the endpoints, behind [`ModelMemory`].

use serde::{Deserialize, Serialize};

use crate::agents::runtimes::runtime;
use crate::base::future::BoxFuture;
use crate::base::ids::RunId;
use crate::models::model_catalog::display_name_from_id;
use crate::settings::agents::EndpointSettings;

/// A local server as a session reaches it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server {
    /// The `[endpoints.<id>]` key, which names its runtime.
    pub endpoint: String,
    pub base_url: String,
    /// The variable holding its key, for a server that wants one (LM Studio, when you turn
    /// authentication on). Never the key itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// `free_memory`, as your table had it when the model was first held.
    pub free_memory: bool,
}

impl Server {
    /// Endpoint `id` as Argonauts reaches it. `None` for a server that isn't a local runtime on
    /// this machine, whose memory isn't Argonauts' to look after.
    pub fn local(id: &str, endpoint: &EndpointSettings) -> Option<Self> {
        runtime(id)?;
        if !endpoint.local {
            return None;
        }
        Some(Self {
            endpoint: id.to_owned(),
            base_url: endpoint.base_url.clone()?,
            api_key_env: endpoint.api_key_env.clone(),
            free_memory: endpoint.free_memory,
        })
    }

    /// As the screens write its runtime, such as `Ollama`.
    pub fn name(&self) -> &str {
        runtime(&self.endpoint).map_or(self.endpoint.as_str(), |runtime| runtime.name)
    }
}

/// Who uses a model: a run, or the conversation of one app. `pid` is the process that will let
/// go of it, so a holder whose process died no longer counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub key: String,
    pub pid: u32,
}

impl Holder {
    pub fn run(run: &RunId) -> Self {
        Self {
            key: format!("run {run}"),
            pid: std::process::id(),
        }
    }

    /// This app's conversation, and its narrator. A new conversation is a new holder only once
    /// the last one let go.
    pub fn conversation() -> Self {
        Self {
            key: "conversation".to_owned(),
            pid: std::process::id(),
        }
    }

    /// The run a session belongs to, else this app's conversation.
    pub fn of(run: Option<&RunId>) -> Self {
        run.map_or_else(Self::conversation, Self::run)
    }
}

/// What a server said when a session held a model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hold {
    /// The window the server gave the model, when it holds it already: such as Ollama's 65,536
    /// for a tag with `num_ctx` 65536.
    pub window: Option<u64>,
    /// The requests the server takes at once, when it says (llama-server's slots).
    pub slots: Option<u32>,
}

/// A model Argonauts let go of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freed {
    pub server: Server,
    pub model: String,
    /// The memory it held, when the server said.
    pub bytes: Option<u64>,
}

impl Freed {
    /// `Freed Qwen3 Coder 30B (64k) from Ollama (25 GB of RAM)`, naming the model as `name`
    /// says, or by its id.
    pub fn words(&self, name: Option<&str>) -> String {
        let name = name.map_or_else(|| display_name_from_id(&self.model), str::to_owned);
        let mut words = format!("Freed {name} from {}", self.server.name());
        if let Some(bytes) = self.bytes.filter(|bytes| *bytes > 0) {
            words.push_str(&format!(" ({} of RAM)", memory_words(bytes)));
        }
        words
    }
}

/// Memory as the screens write it: whole gigabytes (10⁹ bytes), or megabytes under one.
pub fn memory_words(bytes: u64) -> String {
    const GB: f64 = 1e9;
    let gigabytes = bytes as f64 / GB;
    if gigabytes >= 0.95 {
        format!("{} GB", gigabytes.round() as u64)
    } else {
        format!("{} MB", ((bytes as f64 / 1e6).round() as u64).max(1))
    }
}

/// The servers' side of it, for the sessions: one holds a model through it before its first
/// request and touches it when it ends. The run or conversation that held it lets go when it
/// ends, through the app, which keeps the record.
pub trait ModelMemory: Send + Sync {
    /// Before the first request for `model`: notes whether the server held it already and that
    /// `holder` uses it.
    fn hold<'a>(
        &'a self,
        server: &'a Server,
        model: &'a str,
        holder: &'a Holder,
    ) -> BoxFuture<'a, Hold>;

    /// Loads `model` now, with a request that generates nothing, so the window the server
    /// gives it is known before the session starts. Only Ollama loads that way; `None` when the
    /// server doesn't say.
    fn load<'a>(&'a self, server: &'a Server, model: &'a str) -> BoxFuture<'a, Option<u64>>;

    /// A session on `model` ended: notes what the server says of it now, so freeing it later
    /// can tell whether someone else used it since.
    fn touch<'a>(&'a self, server: &'a Server, model: &'a str) -> BoxFuture<'a, ()>;
}

/// Looks after nothing, for tests and for a process with no home.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoModelMemory;

impl ModelMemory for NoModelMemory {
    fn hold<'a>(
        &'a self,
        _server: &'a Server,
        _model: &'a str,
        _holder: &'a Holder,
    ) -> BoxFuture<'a, Hold> {
        Box::pin(async { Hold::default() })
    }

    fn load<'a>(&'a self, _server: &'a Server, _model: &'a str) -> BoxFuture<'a, Option<u64>> {
        Box::pin(async { None })
    }

    fn touch<'a>(&'a self, _server: &'a Server, _model: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ollama() -> Server {
        Server {
            endpoint: "ollama".to_owned(),
            base_url: "http://localhost:11434/v1".to_owned(),
            api_key_env: None,
            free_memory: true,
        }
    }

    #[test]
    fn the_freed_line_names_the_model_its_server_and_the_memory() {
        let qwen = Freed {
            server: ollama(),
            model: "qwen3-coder:30b-64k".to_owned(),
            bytes: Some(25_311_000_000),
        };
        assert_eq!(
            qwen.words(Some("Qwen3 Coder 30B (64k)")),
            "Freed Qwen3 Coder 30B (64k) from Ollama (25 GB of RAM)"
        );
        let llama = Freed {
            model: "llama3.2:latest".to_owned(),
            bytes: Some(23_779_979_264),
            ..qwen.clone()
        };
        assert_eq!(
            llama.words(None),
            "Freed Llama3.2 from Ollama (24 GB of RAM)"
        );
        let lm_studio = Freed {
            server: Server {
                endpoint: "lmstudio".to_owned(),
                ..ollama()
            },
            model: "qwen/qwen3-8b".to_owned(),
            bytes: None,
        };
        assert_eq!(
            lm_studio.words(Some("Qwen3 8B")),
            "Freed Qwen3 8B from LM Studio"
        );
    }

    #[test]
    fn memory_reads_in_whole_gigabytes() {
        assert_eq!(memory_words(25_311_000_000), "25 GB");
        assert_eq!(memory_words(1_000_000_000), "1 GB");
        assert_eq!(memory_words(700_000_000), "700 MB");
        assert_eq!(memory_words(1), "1 MB");
    }

    #[test]
    fn only_a_local_runtime_has_memory_to_look_after() {
        let mut endpoint = EndpointSettings {
            base_url: Some("http://localhost:11434/v1".to_owned()),
            local: true,
            ..EndpointSettings::default()
        };
        assert_eq!(Server::local("ollama", &endpoint), Some(ollama()));
        assert!(Server::local("gpu-box", &endpoint).is_none());
        endpoint.local = false;
        assert!(Server::local("ollama", &endpoint).is_none());
    }

    #[test]
    fn a_session_holds_for_its_run_or_the_conversation() {
        let run = RunId::new("r-0007").unwrap();
        assert_eq!(Holder::of(Some(&run)).key, "run r-0007");
        assert_eq!(Holder::of(None), Holder::conversation());
        assert_eq!(Holder::conversation().pid, std::process::id());
    }
}
