//! Local runtimes as custom OpenAI-compatible providers: OpenCode reaches Ollama and
//! LM Studio through `@ai-sdk/openai-compatible` with a base URL and a `models` map.
//!
//! It goes in `OPENCODE_CONFIG_CONTENT`, which OpenCode 1.18.34 merges over your global and
//! project files key by key (checked with `opencode debug config`). So when your config already
//! declares the provider but not the model, only the model is sent, keeping your base URL and
//! package. OpenCode answers a model it doesn't know with an "Unexpected server error".

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::user_config::{self, Declared};

pub(crate) const CONFIG_CONTENT_ENV: &str = "OPENCODE_CONFIG_CONTENT";

/// (provider id, display name, base URL), the URLs from OpenCode's provider docs. They are the
/// runtimes' own defaults, used when neither your settings nor your OpenCode config names
/// another address.
const LOCAL_PROVIDERS: &[(&str, &str, &str)] = &[
    ("ollama", "Ollama", "http://localhost:11434/v1"),
    ("lmstudio", "LM Studio", "http://127.0.0.1:1234/v1"),
];

/// Where each local runtime listens, from your `[endpoints.ollama]` and `[endpoints.lmstudio]`
/// `base_url`. A runtime with none set keeps its own default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalUrls(BTreeMap<String, String>);

impl LocalUrls {
    pub fn new() -> Self {
        Self::default()
    }

    /// Points `provider` at `base_url`. Only `ollama` and `lmstudio` are local runtimes OpenCode
    /// is told about, so any other id is ignored, and an endpoint of yours can be passed
    /// whatever it's called.
    pub fn with(mut self, provider: &str, base_url: impl Into<String>) -> Self {
        if LOCAL_PROVIDERS.iter().any(|(id, ..)| *id == provider) {
            self.0.insert(provider.to_owned(), base_url.into());
        }
        self
    }

    fn base_url<'a>(&'a self, provider: &str, default: &'a str) -> &'a str {
        self.0.get(provider).map_or(default, String::as_str)
    }
}

/// The inline config for an `ollama/<model>` or `lmstudio/<model>` route, declaring that one
/// model at the runtime's address in `urls`. `None` for any other provider, which OpenCode
/// already knows.
pub fn local_provider_config(route: &str, urls: &LocalUrls) -> Option<Value> {
    let (provider, model) = route.split_once('/')?;
    if model.is_empty() {
        return None;
    }
    let &(id, name, default) = LOCAL_PROVIDERS.iter().find(|(id, ..)| *id == provider)?;
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

/// The inline config to send for `route`, if any. `None` when the route isn't local, or when one
/// of `config_files` already declares its model. When they declare the provider alone, just the
/// model, so the provider's own settings stay in force.
pub(crate) fn inline_config(
    route: &str,
    config_files: &[PathBuf],
    urls: &LocalUrls,
) -> Option<Value> {
    let config = local_provider_config(route, urls)?;
    let (provider, model) = route.split_once('/')?;
    match user_config::declared(config_files, provider, model) {
        Declared::Nothing => Some(config),
        Declared::Provider => Some(json!({
            "$schema": "https://opencode.ai/config.json",
            "provider": { provider: { "models": { model: { "name": model } } } },
        })),
        Declared::Model => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use argonauts_core::SessionRequest;

    use super::*;

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
        assert!(inline_config("ollama/llama3.2", &files, &LocalUrls::new()).is_none());
        assert!(inline_config("lmstudio/qwen/qwen3-8b", &files, &LocalUrls::new()).is_some());
        assert!(inline_config("openrouter/z-ai/glm-5", &[], &LocalUrls::new()).is_none());

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
        let inline = inline_config("ollama/gemma4:e2b", &[yours], &LocalUrls::new()).unwrap();
        let ollama = &inline["provider"]["ollama"];
        assert_eq!(ollama["models"]["gemma4:e2b"]["name"], "gemma4:e2b");
        // OpenCode merges key by key, so leaving these out keeps yours.
        assert!(ollama["options"].is_null());
        assert!(ollama["npm"].is_null());
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
