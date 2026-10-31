//! The model servers Argonauts Code knows by name: Ollama, LM Studio, llama.cpp's llama-server
//! and vLLM. Every list of local runtimes reads this one table: the known endpoints, the
//! OpenCode providers, the setup's probe, Codex's local mode and the names on screen.
//!
//! Each serves an OpenAI-style API on this machine. OpenCode codes with all four; Argonauts
//! reviews and judges with them directly.

/// Which server it is, for the parts that ask it something in its own API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeKind {
    Ollama,
    LmStudio,
    LlamaServer,
    Vllm,
}

/// How many requests a server takes at once when your `[endpoints.<id>]` table sets no
/// `max_concurrency`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requests {
    At(u32),
    /// As many as the server says it has slots for (llama-server's `--parallel`).
    TheServerSlots,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Runtime {
    pub kind: RuntimeKind,
    /// The `[endpoints.<id>]` key and the OpenCode provider id.
    pub id: &'static str,
    /// As the screens write it.
    pub name: &'static str,
    /// Where it listens unless your table moves it. The setup asks each, in this order.
    pub addresses: &'static [&'static str],
    pub requests: Requests,
    /// Its ports are ones other servers use too, so a session may reach it only once you have a
    /// table for it, which the setup writes when the server says what it is.
    pub common_ports: bool,
    /// Codex's local mode (`--oss --local-provider <id>`) takes it, as Codex itself offers.
    pub codex_local: bool,
}

/// llama-server moved to port 9931 on 2026-10-09; older builds and its Docker image keep 8080.
pub const RUNTIMES: &[Runtime] = &[
    Runtime {
        kind: RuntimeKind::Ollama,
        id: "ollama",
        name: "Ollama",
        addresses: &["http://localhost:11434/v1"],
        // OLLAMA_NUM_PARALLEL defaults to 1.
        requests: Requests::At(1),
        common_ports: false,
        codex_local: true,
    },
    Runtime {
        kind: RuntimeKind::LmStudio,
        id: "lmstudio",
        name: "LM Studio",
        addresses: &["http://127.0.0.1:1234/v1"],
        requests: Requests::At(1),
        common_ports: false,
        codex_local: true,
    },
    Runtime {
        kind: RuntimeKind::LlamaServer,
        id: "llamacpp",
        name: "llama-server",
        addresses: &["http://127.0.0.1:9931/v1", "http://127.0.0.1:8080/v1"],
        requests: Requests::TheServerSlots,
        common_ports: true,
        codex_local: false,
    },
    Runtime {
        kind: RuntimeKind::Vllm,
        id: "vllm",
        name: "vLLM",
        addresses: &["http://localhost:8000/v1"],
        requests: Requests::At(4),
        common_ports: true,
        codex_local: false,
    },
];

/// The runtime an `[endpoints.<id>]` key or an OpenCode provider id names.
pub fn runtime(id: &str) -> Option<&'static Runtime> {
    RUNTIMES.iter().find(|runtime| runtime.id == id)
}

/// The runtime a model route such as `ollama/qwen3-coder:30b` runs on, and the model's own id
/// there. vLLM's ids keep their slash (`vllm/Qwen/Qwen3-8B` is `Qwen/Qwen3-8B`).
pub fn local_route(route: &str) -> Option<(&'static Runtime, &str)> {
    let (provider, model) = route.split_once('/')?;
    Some((runtime(provider)?, model)).filter(|(_, model)| !model.is_empty())
}

impl Runtime {
    /// The address it listens on when your table names none.
    pub fn address(&self) -> &'static str {
        self.addresses[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_runtime_has_one_id_and_its_own_name() {
        let ids: Vec<&str> = RUNTIMES.iter().map(|runtime| runtime.id).collect();
        assert_eq!(ids, ["ollama", "lmstudio", "llamacpp", "vllm"]);
        let llama = runtime("llamacpp").unwrap();
        assert_eq!(llama.name, "llama-server");
        assert_eq!(llama.address(), "http://127.0.0.1:9931/v1");
        assert_eq!(llama.addresses[1], "http://127.0.0.1:8080/v1");
        assert_eq!(
            runtime("vllm").unwrap().address(),
            "http://localhost:8000/v1"
        );
        // OpenCode's own name for llama-server is not a second id.
        assert!(runtime("llama.cpp").is_none());
        assert!(runtime("openrouter").is_none());
    }

    #[test]
    fn codex_takes_only_the_runtimes_codex_offers() {
        let codex: Vec<&str> = RUNTIMES
            .iter()
            .filter(|runtime| runtime.codex_local)
            .map(|runtime| runtime.id)
            .collect();
        assert_eq!(codex, ["ollama", "lmstudio"]);
    }

    #[test]
    fn a_route_names_its_runtime_and_model() {
        let (ollama, model) = local_route("ollama/qwen3-coder:30b").unwrap();
        assert_eq!((ollama.id, model), ("ollama", "qwen3-coder:30b"));
        let (vllm, model) = local_route("vllm/Qwen/Qwen3-8B").unwrap();
        assert_eq!((vllm.id, model), ("vllm", "Qwen/Qwen3-8B"));
        assert!(local_route("ollama/").is_none());
        assert!(local_route("opencode/big-pickle").is_none());
        assert!(local_route("llama3.2").is_none());
    }
}
