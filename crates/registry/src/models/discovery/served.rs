//! Each model server's own way of saying what it serves, with the window it holds each model
//! to. The most a model takes and the window it gets are told apart: the server's window is
//! what a session can use.
//!
//! - Ollama: its own API, in [`super::ollama`].
//! - LM Studio: `GET /api/v1/models`, with `max_context_length` and, once loaded, each
//!   instance's `config.context_length`. A model it loads on first use is sized then.
//! - llama-server: `GET /v1/models` for `meta.n_ctx_train`, and `GET /slots` (else `GET /props`)
//!   for the window each request gets.
//! - vLLM: `GET /v1/models`, whose `max_model_len` is the window it serves at.
//! - Any other server, OpenRouter among them: `GET /models` as the OpenAI API has it.

use argonauts_core::models::registry::ModelEntry;
use serde_json::{Value, json};

use super::http::{Server, Unanswered};
use super::ollama;
use super::parse::{endpoint_entry, parse_openai_models, plain, tokens};

/// Which server an endpoint is, by the id the known endpoints give it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Runtime {
    Ollama,
    LmStudio,
    LlamaServer,
    Vllm,
    OpenAi,
}

impl Runtime {
    pub fn of(endpoint: &str) -> Self {
        match endpoint {
            "ollama" => Self::Ollama,
            "lmstudio" => Self::LmStudio,
            "llamacpp" => Self::LlamaServer,
            "vllm" => Self::Vllm,
            _ => Self::OpenAi,
        }
    }
}

/// Every model `server` serves, with what it says of each.
pub(super) async fn list(server: &Server) -> Result<Vec<ModelEntry>, Unanswered> {
    match Runtime::of(&server.id) {
        Runtime::Ollama => ollama::list(server).await,
        Runtime::LmStudio => match server.get("/api/v1/models").await {
            Ok(json) => Ok(parse_lmstudio_models(&server.id, server.local, &json)?),
            // An LM Studio older than its own REST API still lists ids.
            Err(unanswered) if unanswered.not_running => Err(unanswered),
            Err(_) => openai(server).await,
        },
        Runtime::LlamaServer => {
            let mut models = openai(server).await?;
            // One model's window is the server's. A server routing to several sizes each as
            // it loads it.
            if let [only] = models.as_mut_slice() {
                only.window = slot_window(server).await;
            }
            Ok(models)
        }
        Runtime::Vllm | Runtime::OpenAi => openai(server).await,
    }
}

/// Whether a server on ports other apps use too says in its own API that it's the one its id
/// names: llama-server's models are owned by `llamacpp`, or its `/props` names its build;
/// vLLM's are owned by `vllm`. Another app listening there is never taken for either.
pub(super) async fn says_what_it_is(server: &Server) -> bool {
    let owned_by = |json: &str, runtime: &str| {
        serde_json::from_str::<Value>(json).is_ok_and(|list| {
            list["data"]
                .as_array()
                .is_some_and(|models| models.iter().any(|model| model["owned_by"] == runtime))
        })
    };
    match Runtime::of(&server.id) {
        Runtime::LlamaServer => {
            if server
                .openai_models()
                .await
                .is_ok_and(|json| owned_by(&json, "llamacpp"))
            {
                return true;
            }
            server.get("/props").await.is_ok_and(|json| {
                serde_json::from_str::<Value>(&json)
                    .is_ok_and(|props| props["build_info"].is_string())
            })
        }
        Runtime::Vllm => server
            .openai_models()
            .await
            .is_ok_and(|json| owned_by(&json, "vllm")),
        Runtime::Ollama | Runtime::LmStudio | Runtime::OpenAi => true,
    }
}

async fn openai(server: &Server) -> Result<Vec<ModelEntry>, Unanswered> {
    let json = server.openai_models().await?;
    Ok(parse_openai_models(&server.id, server.local, &json)?)
}

/// The window llama-server gives each request: a slot's `n_ctx`, which `/props` repeats for
/// the first slot when `/slots` is turned off.
async fn slot_window(server: &Server) -> Option<u64> {
    if let Ok(json) = server.get("/slots").await
        && let Some(window) = parse_llama_slots(&json)
    {
        return Some(window);
    }
    parse_llama_props(&server.get("/props").await.ok()?)
}

/// The smallest slot's window, since a request may land in any of them.
pub fn parse_llama_slots(json: &str) -> Option<u64> {
    let slots: Value = serde_json::from_str(json).ok()?;
    slots
        .as_array()?
        .iter()
        .filter_map(|slot| tokens(&slot["n_ctx"]))
        .min()
}

pub fn parse_llama_props(json: &str) -> Option<u64> {
    let props: Value = serde_json::from_str(json).ok()?;
    tokens(&props["default_generation_settings"]["n_ctx"])
}

/// LM Studio's `GET /api/v1/models`: its language models, each with the most it takes and,
/// while loaded, the smallest window an instance holds it to. Embedding models are left out.
pub fn parse_lmstudio_models(
    endpoint: &str,
    local: bool,
    json: &str,
) -> Result<Vec<ModelEntry>, String> {
    let reply: Value = serde_json::from_str(json)
        .map_err(|e| format!("{endpoint}'s model list isn't JSON: {e}"))?;
    let models = reply["models"]
        .as_array()
        .ok_or_else(|| format!("{endpoint}'s model list has no `models` array"))?;
    Ok(models
        .iter()
        .filter(|model| model["type"].as_str().is_none_or(|kind| kind == "llm"))
        .filter_map(|model| {
            let key = model["key"].as_str()?;
            let mut entry = endpoint_entry(endpoint, local, &json!({ "id": key }))?;
            let capabilities = &model["capabilities"];
            entry.name = plain(&model["display_name"]);
            entry.context_window = tokens(&model["max_context_length"]);
            entry.window = model["loaded_instances"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|instance| tokens(&instance["config"]["context_length"]))
                .min();
            entry.tools = capabilities["trained_for_tool_use"].as_bool() == Some(true);
            entry.thinking = Some(capabilities["reasoning"].is_object());
            Some(entry)
        })
        .collect())
}

/// The window `model` was loaded with, from the servers that size a model as they load it.
/// `None` from any other, or while it isn't loaded.
pub(super) async fn loaded_window(server: &Server, model: &str) -> Option<u64> {
    match Runtime::of(&server.id) {
        Runtime::Ollama => ollama::loaded_window(server, model).await,
        Runtime::LmStudio => {
            let json = server.get("/api/v1/models").await.ok()?;
            parse_lmstudio_models(&server.id, server.local, &json)
                .ok()?
                .into_iter()
                .find(|entry| entry.id.model.as_deref() == Some(model))?
                .window
        }
        Runtime::LlamaServer | Runtime::Vllm | Runtime::OpenAi => None,
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::models::registry::ModelAccess;

    use super::*;

    const LMSTUDIO: &str =
        include_str!("../../../tests/fixtures/servers/synthetic/lmstudio-models.json");
    const LLAMA_MODELS: &str =
        include_str!("../../../tests/fixtures/servers/synthetic/llama-server-models.json");
    const LLAMA_PROPS: &str =
        include_str!("../../../tests/fixtures/servers/synthetic/llama-server-props.json");
    const LLAMA_SLOTS: &str =
        include_str!("../../../tests/fixtures/servers/synthetic/llama-server-slots.json");
    const VLLM: &str = include_str!("../../../tests/fixtures/servers/synthetic/vllm-models.json");
    const OPENROUTER: &str =
        include_str!("../../../tests/fixtures/openai-models/synthetic/openrouter.json");

    #[test]
    fn lm_studio_holds_a_loaded_model_to_its_instance() {
        let models = parse_lmstudio_models("lmstudio", true, LMSTUDIO).unwrap();
        let ids: Vec<String> = models.iter().map(|e| e.id.to_string()).collect();
        assert_eq!(
            ids,
            [
                "lmstudio/qwen/qwen3-coder-30b",
                "lmstudio/google/gemma-3-12b"
            ]
        );
        let (qwen, gemma) = (&models[0], &models[1]);
        assert_eq!(qwen.name.as_deref(), Some("Qwen3 Coder 30B"));
        assert_eq!(
            (qwen.context_window, qwen.window),
            (Some(262_144), Some(4_096))
        );
        assert_eq!(qwen.session_window(), Some(4_096));
        assert!(qwen.tools);
        assert_eq!(qwen.thinking, Some(false));
        // Not loaded: sized as LM Studio loads it.
        assert_eq!((gemma.context_window, gemma.window), (Some(131_072), None));
        assert_eq!(gemma.session_window(), None);
        assert!(!gemma.tools);
        assert_eq!(gemma.thinking, Some(true));
        assert!(models.iter().all(|e| e.access == ModelAccess::Local));
    }

    #[test]
    fn llama_server_and_vllm_say_their_windows() {
        let llama = parse_openai_models("llamacpp", true, LLAMA_MODELS).unwrap();
        assert_eq!(llama[0].context_window, Some(262_144));
        assert_eq!(parse_llama_slots(LLAMA_SLOTS), Some(32_768));
        assert_eq!(parse_llama_props(LLAMA_PROPS), Some(32_768));
        assert_eq!(parse_llama_slots("{}"), None);

        let vllm = parse_openai_models("vllm", true, VLLM).unwrap();
        assert_eq!(vllm[0].id.to_string(), "vllm/Qwen/Qwen3-8B");
        assert_eq!(
            (vllm[0].context_window, vllm[0].window),
            (Some(32_768), Some(32_768))
        );
        assert_eq!(vllm[0].session_window(), Some(32_768));
    }

    #[test]
    fn openrouter_holds_a_model_to_its_top_provider() {
        let models = parse_openai_models("openrouter", false, OPENROUTER).unwrap();
        let deepseek = &models[1];
        assert_eq!(deepseek.context_window, Some(163_840));
        assert_eq!(deepseek.window, Some(131_072));
        assert_eq!(deepseek.session_window(), Some(131_072));
        assert_eq!(models[0].session_window(), Some(202_752));
    }

    #[test]
    fn runtimes_go_by_the_known_ids() {
        assert_eq!(Runtime::of("ollama"), Runtime::Ollama);
        assert_eq!(Runtime::of("lmstudio"), Runtime::LmStudio);
        assert_eq!(Runtime::of("llamacpp"), Runtime::LlamaServer);
        assert_eq!(Runtime::of("vllm"), Runtime::Vllm);
        assert_eq!(Runtime::of("my-box"), Runtime::OpenAi);
    }
}
