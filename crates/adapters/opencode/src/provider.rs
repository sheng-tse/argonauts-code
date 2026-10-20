//! Local runtimes as custom OpenAI-compatible providers: OpenCode reaches Ollama,
//! LM Studio, llama-server and vLLM through `@ai-sdk/openai-compatible` with a base URL and a
//! `models` map.
//!
//! It goes in `OPENCODE_CONFIG_CONTENT`, which OpenCode merges over your global and project
//! files key by key, inline last (checked with `opencode debug config` on 1.18.34 and on a
//! private 2.0.26 server). So when your config already declares the provider but not the model,
//! only the model is sent, keeping your base URL and package, and a model of yours gets only the
//! window it lacks. A provider of yours at a runtime's address is yours under whatever id you
//! gave it, such as the `llama.cpp` OpenCode's docs give llama-server. OpenCode answers a model
//! it doesn't know with an "Unexpected server error".
//! The 1.x shape (`provider`, `npm`, `options`) is sent, which both versions read.

use std::collections::BTreeMap;
use std::path::PathBuf;

use argonauts_core::runtimes::{local_route, runtime};
use serde_json::{Value, json};

use crate::user_config::{self, Declared, Wanted, Yours};

pub(crate) const CONFIG_CONTENT_ENV: &str = "OPENCODE_CONFIG_CONTENT";

/// Where each local runtime listens, from your `[endpoints.<runtime>]` `base_url`. A runtime
/// with none set keeps its own default, from the core's table of runtimes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalUrls(BTreeMap<String, String>);

impl LocalUrls {
    pub fn new() -> Self {
        Self::default()
    }

    /// Points `provider` at `base_url`. Only the known local runtimes are providers OpenCode is
    /// told about, so any other id is ignored, and an endpoint of yours can be passed whatever
    /// it's called.
    pub fn with(mut self, provider: &str, base_url: impl Into<String>) -> Self {
        if runtime(provider).is_some() {
            self.0.insert(provider.to_owned(), base_url.into());
        }
        self
    }

    fn base_url<'a>(&'a self, provider: &str, default: &'a str) -> &'a str {
        self.0.get(provider).map_or(default, String::as_str)
    }
}

/// The inline config for a local runtime's route, such as `ollama/<model>`, declaring that one
/// model at the runtime's address in `urls`. `None` for any other provider, which OpenCode
/// already knows.
pub fn local_provider_config(route: &str, urls: &LocalUrls) -> Option<Value> {
    let (provider, model) = route.split_once('/')?;
    if model.is_empty() {
        return None;
    }
    let runtime = runtime(provider)?;
    let (id, name, default) = (runtime.id, runtime.name, runtime.address());
    Some(json!({
        "$schema": "https://opencode.ai/config.json",
        "provider": {
            id: {
                "npm": "@ai-sdk/openai-compatible",
                "name": name,
                "options": { "baseURL": urls.base_url(id, default) },
                "models": { model: { "name": model } },
            }
        }
    }))
}

/// How a session's route reaches OpenCode: the `--model` it goes as, and the inline config
/// sent with it, if any.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Launch {
    pub route: String,
    pub config: Option<Value>,
}

/// How `route` is launched, given OpenCode 2's own `answer` about its config, else the
/// `config_files` it reads. A local runtime goes under your provider for it when you have
/// one, found by the runtime's id or else by its address, so one server is never declared
/// twice, and the inline config adds only what yours lacks. `window` is the window the model's
/// server holds it to.
pub(crate) fn launch(
    route: &str,
    answer: Option<&str>,
    config_files: &[PathBuf],
    window: Option<u64>,
    urls: &LocalUrls,
) -> Launch {
    let Some(yours) = yours(route, answer, config_files, urls) else {
        return Launch {
            route: route.to_owned(),
            config: None,
        };
    };
    let model = route.split_once('/').map_or("", |(_, model)| model);
    let route = format!("{}/{model}", yours.id);
    let config = inline_config(&route, yours.declared, window, urls);
    Launch { route, config }
}

/// Your provider for a local `route`'s runtime, from OpenCode's own account of its config
/// when it gave one, else from the files it reads. `None` for a route that isn't local.
pub(crate) fn yours(
    route: &str,
    answer: Option<&str>,
    config_files: &[PathBuf],
    urls: &LocalUrls,
) -> Option<Yours> {
    let (runtime, model) = local_route(route)?;
    let wanted = Wanted {
        id: runtime.id,
        address: urls.base_url(runtime.id, runtime.address()),
        model,
    };
    Some(
        answer
            .and_then(|answer| user_config::declared_in_answer(answer, wanted))
            .unwrap_or_else(|| user_config::declared(config_files, wanted)),
    )
}

/// The inline config for a local `route` as it is launched, given how much of it your config
/// `declared`. `None` when yours has all of it. When yours declares the provider alone, just
/// the model, so the provider's own settings stay in force. `window` is sent only where yours
/// names none.
fn inline_config(
    route: &str,
    declared: Declared,
    window: Option<u64>,
    urls: &LocalUrls,
) -> Option<Value> {
    let (provider, model) = route.split_once('/')?;
    let limit = window.map(limit);
    let entry = match declared {
        Declared::Nothing => {
            let mut config = local_provider_config(route, urls)?;
            if let Some(limit) = limit {
                config["provider"][provider]["models"][model]["limit"] = limit;
            }
            return Some(config);
        }
        Declared::Provider => match limit {
            Some(limit) => json!({ "name": model, "limit": limit }),
            None => json!({ "name": model }),
        },
        Declared::Model => json!({ "limit": limit? }),
        Declared::ModelAndWindow => return None,
    };
    Some(json!({
        "$schema": "https://opencode.ai/config.json",
        "provider": { provider: { "models": { model: entry } } },
    }))
}

/// OpenCode 2 drops a whole provider whose model has a context limit and no output limit, so
/// the two go together. A quarter of the window for the answer, at most the 32,000 OpenCode
/// gives a model it finds itself, leaves the rest for the conversation.
fn limit(window: u64) -> Value {
    json!({ "context": window, "output": (window / 4).min(32_000) })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use argonauts_core::SessionRequest;

    use super::*;

    fn inline(route: &str, files: &[PathBuf], window: Option<u64>) -> Option<Value> {
        launch(route, None, files, window, &LocalUrls::new()).config
    }

    #[test]
    fn opencode_local_provider() {
        let config = local_provider_config("ollama/llama3.2", &LocalUrls::new()).unwrap();
        let ollama = &config["provider"]["ollama"];
        assert_eq!(ollama["npm"], "@ai-sdk/openai-compatible");
        assert_eq!(ollama["options"]["baseURL"], "http://localhost:11434/v1");
        assert_eq!(ollama["models"]["llama3.2"]["name"], "llama3.2");

        let config = local_provider_config("lmstudio/qwen/qwen3-8b", &LocalUrls::new()).unwrap();
        let lmstudio = &config["provider"]["lmstudio"];
        assert_eq!(lmstudio["options"]["baseURL"], "http://127.0.0.1:1234/v1");
        assert!(lmstudio["models"]["qwen/qwen3-8b"].is_object());

        assert!(local_provider_config("openrouter/z-ai/glm-5", &LocalUrls::new()).is_none());
        assert!(local_provider_config("ollama/", &LocalUrls::new()).is_none());
        assert!(local_provider_config("ollama", &LocalUrls::new()).is_none());

        let req = SessionRequest {
            workdir: PathBuf::from("/w"),
            prompt: "hi".to_owned(),
            model: Some("ollama/llama3.2".to_owned()),
            ..SessionRequest::default()
        };
        let cmd = crate::command::build_with(&req, &[], &LocalUrls::new()).unwrap();
        let (_, inline) = cmd
            .env_set
            .iter()
            .find(|(key, _)| key == CONFIG_CONTENT_ENV)
            .unwrap();
        let inline: Value = serde_json::from_str(inline).unwrap();
        assert_eq!(
            inline["provider"]["ollama"]["options"]["baseURL"],
            "http://localhost:11434/v1"
        );
        let at = cmd.args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(cmd.args[at + 1], "ollama/llama3.2");
    }

    #[test]
    fn llama_server_and_vllm_are_providers_as_ollama_is() {
        let config = local_provider_config("llamacpp/qwen3-8b-q4.gguf", &LocalUrls::new()).unwrap();
        let llama = &config["provider"]["llamacpp"];
        assert_eq!(llama["name"], "llama-server");
        assert_eq!(llama["npm"], "@ai-sdk/openai-compatible");
        assert_eq!(llama["options"]["baseURL"], "http://127.0.0.1:9931/v1");

        let urls = LocalUrls::new().with("vllm", "http://gpu-box:8000/v1");
        let config = local_provider_config("vllm/Qwen/Qwen3-8B", &urls).unwrap();
        let vllm = &config["provider"]["vllm"];
        assert_eq!(vllm["name"], "vLLM");
        assert_eq!(vllm["options"]["baseURL"], "http://gpu-box:8000/v1");
        assert!(vllm["models"]["Qwen/Qwen3-8B"].is_object());

        // OpenCode's own name for llama-server is no provider Argonauts declares.
        assert!(local_provider_config("llama.cpp/m.gguf", &LocalUrls::new()).is_none());
    }

    #[test]
    fn your_own_provider_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let yours = dir.path().join("opencode.json");
        std::fs::write(
            &yours,
            r#"{"provider": {"ollama": {"options": {"baseURL": "http://gpu:11434/v1"},
                                        "models": {"llama3.2": {}}}}}"#,
        )
        .unwrap();
        let files = [yours];
        assert!(inline("ollama/llama3.2", &files, None).is_none());
        assert!(inline("lmstudio/qwen/qwen3-8b", &files, None).is_some());
        assert!(inline("openrouter/z-ai/glm-5", &[], Some(8192)).is_none());

        let req = SessionRequest {
            workdir: dir.path().to_path_buf(),
            prompt: "hi".to_owned(),
            model: Some("ollama/llama3.2".to_owned()),
            ..SessionRequest::default()
        };
        let cmd = crate::command::build_with(&req, &files, &LocalUrls::new()).unwrap();
        assert!(!cmd.env_set.iter().any(|(key, _)| key == CONFIG_CONTENT_ENV));
    }

    // A live run with llama3.2 declared in the temp home sent nothing for gemma4:e2b, and
    // OpenCode 1.18.34 answered "Unexpected server error" (ProviderModelNotFoundError).
    #[test]
    fn a_model_your_provider_lacks_is_added_alone() {
        let dir = tempfile::tempdir().unwrap();
        let yours = dir.path().join("opencode.json");
        std::fs::write(
            &yours,
            r#"{"provider": {"ollama": {"options": {"baseURL": "http://gpu:11434/v1"},
                                        "models": {"llama3.2": {}}}}}"#,
        )
        .unwrap();
        let inline = inline("ollama/gemma4:e2b", &[yours], None).unwrap();
        let ollama = &inline["provider"]["ollama"];
        assert_eq!(ollama["models"]["gemma4:e2b"]["name"], "gemma4:e2b");
        // OpenCode merges key by key, so leaving these out keeps yours.
        assert!(ollama["options"].is_null());
        assert!(ollama["npm"].is_null());
    }

    #[test]
    fn your_provider_for_llama_server_is_launched_under_its_own_id() {
        let dir = tempfile::tempdir().unwrap();
        let yours = dir.path().join("opencode.json");
        std::fs::write(
            &yours,
            r#"{"provider": {"llama.cpp": {"npm": "@ai-sdk/openai-compatible",
                "options": {"baseURL": "http://127.0.0.1:9931/v1"}}}}"#,
        )
        .unwrap();
        let req = SessionRequest {
            workdir: dir.path().to_path_buf(),
            prompt: "hi".to_owned(),
            model: Some("llamacpp/qwen3-8b-q4.gguf".to_owned()),
            window: Some(32_768),
            ..SessionRequest::default()
        };
        let cmd = crate::command::build_with(&req, &[yours], &LocalUrls::new()).unwrap();
        let at = cmd.args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(cmd.args[at + 1], "llama.cpp/qwen3-8b-q4.gguf");
        let (_, inline) = cmd
            .env_set
            .iter()
            .find(|(key, _)| key == CONFIG_CONTENT_ENV)
            .unwrap();
        let inline: Value = serde_json::from_str(inline).unwrap();
        // Only the model goes in, under your provider: no second one for the same server.
        assert_eq!(
            inline["provider"],
            json!({"llama.cpp": {"models": {"qwen3-8b-q4.gguf": {
                "name": "qwen3-8b-q4.gguf",
                "limit": {"context": 32_768, "output": 8_192},
            }}}})
        );
    }

    #[test]
    fn the_window_goes_only_where_yours_names_none() {
        let window = |inline: &Value, provider: &str, model: &str| {
            inline["provider"][provider]["models"][model]["limit"].clone()
        };
        let urls = LocalUrls::new();
        let qwen = "ollama/qwen3-coder:30b-64k";
        let full = inline_config(qwen, Declared::Nothing, Some(65_536), &urls).unwrap();
        assert_eq!(
            window(&full, "ollama", "qwen3-coder:30b-64k"),
            json!({"context": 65_536, "output": 16_384})
        );
        assert_eq!(
            full["provider"]["ollama"]["options"]["baseURL"],
            "http://localhost:11434/v1"
        );

        let added = inline_config(qwen, Declared::Provider, Some(65_536), &urls).unwrap();
        let model = &added["provider"]["ollama"]["models"]["qwen3-coder:30b-64k"];
        assert_eq!(model["name"], "qwen3-coder:30b-64k");
        assert_eq!(model["limit"]["context"], 65_536);
        assert!(added["provider"]["ollama"]["options"].is_null());

        // Your model without a window gets only the window, and nothing without one to give.
        let lacking = inline_config(qwen, Declared::Model, Some(32_768), &urls).unwrap();
        let model = &lacking["provider"]["ollama"]["models"]["qwen3-coder:30b-64k"];
        assert_eq!(
            model,
            &json!({"limit": {"context": 32_768, "output": 8_192}})
        );
        assert!(inline_config(qwen, Declared::Model, None, &urls).is_none());
        assert!(inline_config(qwen, Declared::ModelAndWindow, Some(65_536), &urls).is_none());

        // A large window keeps its answer to OpenCode's own default.
        let big = inline_config("ollama/llama3.2", Declared::Nothing, Some(262_144), &urls);
        assert_eq!(
            window(&big.unwrap(), "ollama", "llama3.2")["output"],
            32_000
        );
        assert!(
            window(
                &inline_config(qwen, Declared::Nothing, None, &urls).unwrap(),
                "ollama",
                "qwen3-coder:30b-64k"
            )
            .is_null()
        );
    }

    #[test]
    fn opencode_2s_own_answer_wins_over_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("opencode.json");
        std::fs::write(&file, r#"{"provider": {"ollama": {}}}"#).unwrap();
        let files = [file];
        let route = "ollama/llama3.2";
        let declared = |answer| {
            yours(route, answer, &files, &LocalUrls::new())
                .unwrap()
                .declared
        };
        assert_eq!(declared(None), Declared::Provider);
        let answer = r#"[{"type": "document", "info": {"providers": {"ollama": {"models":
            {"llama3.2": {"limit": {"context": 8192, "output": 2048}}}}}}}]"#;
        assert_eq!(declared(Some(answer)), Declared::ModelAndWindow);
        // An answer that isn't OpenCode 2's list leaves the files to say.
        assert_eq!(declared(Some("error: no such command")), Declared::Provider);
    }

    #[test]
    fn your_endpoint_base_url_reaches_opencode() {
        let urls = LocalUrls::new()
            .with("ollama", "http://gpu-box:11434/v1")
            .with("gpu-box", "http://elsewhere/v1");
        let config = local_provider_config("ollama/llama3.2", &urls).unwrap();
        assert_eq!(
            config["provider"]["ollama"]["options"]["baseURL"],
            "http://gpu-box:11434/v1"
        );
        // LM Studio keeps its default, and an endpoint that isn't a local runtime adds nothing.
        let config = local_provider_config("lmstudio/qwen/qwen3-8b", &urls).unwrap();
        assert_eq!(
            config["provider"]["lmstudio"]["options"]["baseURL"],
            "http://127.0.0.1:1234/v1"
        );
        assert!(local_provider_config("gpu-box/llama3.2", &urls).is_none());

        let req = SessionRequest {
            workdir: PathBuf::from("/w"),
            prompt: "hi".to_owned(),
            model: Some("ollama/llama3.2".to_owned()),
            ..SessionRequest::default()
        };
        let adapter = crate::OpenCodeAdapter::with_local_urls(urls);
        let cmd = crate::command::build_with(&req, &[], &adapter.local_urls).unwrap();
        let (_, inline) = cmd
            .env_set
            .iter()
            .find(|(key, _)| key == CONFIG_CONTENT_ENV)
            .unwrap();
        let inline: Value = serde_json::from_str(inline).unwrap();
        assert_eq!(
            inline["provider"]["ollama"]["options"]["baseURL"],
            "http://gpu-box:11434/v1"
        );
    }
}
