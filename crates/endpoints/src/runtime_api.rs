//! What a local runtime says in its own API, beside the OpenAI-style one every endpoint speaks:
//! that it is the server it claims to be, the models it serves, what it holds in memory, and
//! the calls that load and free a model.
//!
//! - Ollama: `GET /api/tags`, `GET /api/ps`, and `POST /api/generate` with no prompt to load
//!   (nothing is generated) or with `keep_alive: 0` to free. `keep_alive` in an OpenAI-style
//!   request is ignored, so only Ollama's own call frees a model.
//! - LM Studio: `GET /api/v1/models` with each model's loaded instances, and
//!   `POST /api/v1/models/unload` by instance id. Older builds list at `/api/v0/models`.
//! - llama-server: `/v1/models` owned by `llamacpp`, or `/props` with its `build_info` and
//!   `total_slots`. As a router it serves several models and can drop one: `GET /models` with
//!   each one's status, `POST /models/unload`.
//! - vLLM: `/v1/models` owned by `vllm`. It holds its one model for as long as it runs.

use std::time::Duration;

use argonauts_core::runtimes::{Runtime, RuntimeKind};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// A server that said what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    pub runtime: &'static Runtime,
    pub base_url: String,
    /// The models it offers, by the id requests name them with.
    pub models: Vec<String>,
    /// Requests it takes at once, when it says (llama-server).
    pub slots: Option<u32>,
    /// A llama-server serving several models, which can drop one.
    pub router: bool,
}

/// A model the server holds in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub model: String,
    /// LM Studio's instance id, which its unload takes.
    pub instance: Option<String>,
    pub bytes: Option<u64>,
    pub window: Option<u64>,
    /// When Ollama will let it go by itself.
    pub expires_at: Option<OffsetDateTime>,
}

/// A client for this machine's servers: no proxy, and a short wait.
pub fn local_client(timeout: Duration) -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .no_proxy()
        .build()
        .ok()
}

/// The server's own API sits beside `/v1`.
fn root(base_url: &str) -> &str {
    let trimmed = base_url.trim_end_matches('/');
    trimmed.strip_suffix("/v1").unwrap_or(trimmed)
}

async fn get(client: &reqwest::Client, url: &str, token: Option<&str>) -> Option<Value> {
    let mut request = client.get(url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json().await.ok()
}

async fn post(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
    token: Option<&str>,
) -> Result<Value, String> {
    let mut request = client.post(url).json(body);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.map_err(|err| err.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("{url} answered {status}"));
    }
    Ok(response.json().await.unwrap_or(Value::Null))
}

fn strings(list: &Value, field: &str) -> Vec<String> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item[field].as_str().map(str::to_owned))
        .collect()
}

/// Asks the server at `base_url` whether it is `runtime`, and what it serves. `None` when it
/// doesn't answer, or answers as something else, so another app on a common port is never
/// taken for one.
pub async fn identify(
    client: &reqwest::Client,
    runtime: &'static Runtime,
    base_url: &str,
    token: Option<&str>,
) -> Option<Served> {
    let root = root(base_url);
    let served = |models: Vec<String>| Served {
        runtime,
        base_url: base_url.to_owned(),
        models,
        slots: None,
        router: false,
    };
    match runtime.kind {
        RuntimeKind::Ollama => {
            let tags = get(client, &format!("{root}/api/tags"), token).await?;
            tags["models"]
                .is_array()
                .then(|| served(strings(&tags["models"], "name")))
        }
        RuntimeKind::LmStudio => {
            if let Some(list) = get(client, &format!("{root}/api/v1/models"), token).await
                && list["models"].is_array()
            {
                return Some(served(language_models(&list["models"], "key")));
            }
            let list = get(client, &format!("{root}/api/v0/models"), token).await?;
            list["data"]
                .is_array()
                .then(|| served(language_models(&list["data"], "id")))
        }
        RuntimeKind::LlamaServer => {
            let models = get(client, &format!("{root}/v1/models"), token).await;
            let props = get(client, &format!("{root}/props"), token).await;
            let owned = models
                .as_ref()
                .is_some_and(|list| owned_by(list, "llamacpp"));
            let built = props
                .as_ref()
                .is_some_and(|props| props["build_info"].is_string());
            if !owned && !built {
                return None;
            }
            let mut found = served(
                models
                    .map(|list| strings(&list["data"], "id"))
                    .unwrap_or_default(),
            );
            found.slots = props
                .as_ref()
                .and_then(|props| props["total_slots"].as_u64())
                .and_then(|slots| u32::try_from(slots).ok());
            found.router = get(client, &format!("{root}/models"), token)
                .await
                .is_some_and(|list| {
                    list["data"].as_array().is_some_and(|data| {
                        data.iter()
                            .any(|model| model["status"]["value"].is_string())
                    })
                });
            Some(found)
        }
        RuntimeKind::Vllm => {
            let list = get(client, &format!("{root}/v1/models"), token).await?;
            owned_by(&list, "vllm").then(|| served(strings(&list["data"], "id")))
        }
    }
}

/// A model list whose `owned_by` names the server itself, as llama-server's and vLLM's do.
fn owned_by(list: &Value, server: &str) -> bool {
    list["data"]
        .as_array()
        .is_some_and(|data| data.iter().any(|model| model["owned_by"] == server))
}

/// LM Studio lists embedding models too, which can't answer.
fn language_models(list: &Value, field: &str) -> Vec<String> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter(|model| matches!(model["type"].as_str(), Some("llm" | "vlm") | None))
        .filter_map(|model| model[field].as_str().map(str::to_owned))
        .collect()
}

/// What the server holds in memory now. `None` when it can't say, or didn't answer: vLLM and a
/// llama-server with one model hold it for as long as they run.
pub async fn held(
    client: &reqwest::Client,
    runtime: &Runtime,
    base_url: &str,
    token: Option<&str>,
) -> Option<Vec<Held>> {
    let root = root(base_url);
    match runtime.kind {
        RuntimeKind::Ollama => {
            let ps = get(client, &format!("{root}/api/ps"), token).await?;
            Some(
                ps["models"]
                    .as_array()?
                    .iter()
                    .filter_map(|model| {
                        Some(Held {
                            model: model["name"].as_str()?.to_owned(),
                            instance: None,
                            bytes: model["size"].as_u64(),
                            window: model["context_length"].as_u64(),
                            expires_at: model["expires_at"]
                                .as_str()
                                .and_then(|at| OffsetDateTime::parse(at, &Rfc3339).ok()),
                        })
                    })
                    .collect(),
            )
        }
        RuntimeKind::LmStudio => {
            let list = get(client, &format!("{root}/api/v1/models"), token).await?;
            let mut held = Vec::new();
            for model in list["models"].as_array()? {
                let Some(key) = model["key"].as_str() else {
                    continue;
                };
                for instance in model["loaded_instances"].as_array().into_iter().flatten() {
                    held.push(Held {
                        model: key.to_owned(),
                        instance: instance["id"].as_str().map(str::to_owned),
                        bytes: None,
                        window: instance["config"]["context_length"].as_u64(),
                        expires_at: None,
                    });
                }
            }
            Some(held)
        }
        RuntimeKind::LlamaServer => {
            let list = get(client, &format!("{root}/models"), token).await?;
            Some(
                list["data"]
                    .as_array()?
                    .iter()
                    .filter(|model| model["status"]["value"] == "loaded")
                    .filter_map(|model| {
                        Some(Held {
                            model: model["id"].as_str()?.to_owned(),
                            instance: None,
                            bytes: None,
                            window: None,
                            expires_at: None,
                        })
                    })
                    .collect(),
            )
        }
        RuntimeKind::Vllm => None,
    }
}

/// Loads `model` on Ollama with a request that generates nothing, then says what it holds.
pub async fn load(
    client: &reqwest::Client,
    runtime: &Runtime,
    base_url: &str,
    model: &str,
) -> Result<Option<Held>, String> {
    if runtime.kind != RuntimeKind::Ollama {
        return Ok(None);
    }
    let root = root(base_url);
    post(
        client,
        &format!("{root}/api/generate"),
        &json!({ "model": model }),
        None,
    )
    .await?;
    let held = held(client, runtime, base_url, None)
        .await
        .unwrap_or_default();
    Ok(held.into_iter().find(|held| held.model == model))
}

/// Asks the server to let go of `held`. `Ok(false)` when it has no way to drop one model.
pub async fn free(
    client: &reqwest::Client,
    runtime: &Runtime,
    base_url: &str,
    held: &Held,
    token: Option<&str>,
) -> Result<bool, String> {
    let root = root(base_url);
    let (url, body) = match runtime.kind {
        RuntimeKind::Ollama => (
            format!("{root}/api/generate"),
            json!({ "model": held.model, "keep_alive": 0 }),
        ),
        RuntimeKind::LmStudio => {
            let instance = held.instance.as_deref().unwrap_or(&held.model);
            (
                format!("{root}/api/v1/models/unload"),
                json!({ "instance_id": instance }),
            )
        }
        RuntimeKind::LlamaServer => (
            format!("{root}/models/unload"),
            json!({ "model": held.model }),
        ),
        RuntimeKind::Vllm => return Ok(false),
    };
    post(client, &url, &body, token).await.map(|_| true)
}

#[cfg(test)]
mod tests {
    use argonauts_core::runtimes::runtime;
    use argonauts_testkit::{FakeKind, FakeModelServer};

    use super::*;

    fn client() -> reqwest::Client {
        local_client(Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn the_own_api_sits_beside_v1() {
        assert_eq!(root("http://localhost:11434/v1"), "http://localhost:11434");
        assert_eq!(root("http://localhost:11434/v1/"), "http://localhost:11434");
        assert_eq!(root("http://localhost:11434"), "http://localhost:11434");
    }

    #[tokio::test]
    async fn each_runtime_is_found_only_when_it_says_what_it_is() {
        let ollama = FakeModelServer::start(FakeKind::Ollama, &["llama3.2:latest"]).unwrap();
        let found = identify(
            &client(),
            runtime("ollama").unwrap(),
            &ollama.base_url(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(found.models, ["llama3.2:latest"]);

        let llama = FakeKind::LlamaServer {
            slots: 4,
            router: false,
        };
        let llama = FakeModelServer::start(llama, &["qwen3-8b.gguf"]).unwrap();
        let found = identify(
            &client(),
            runtime("llamacpp").unwrap(),
            &llama.base_url(),
            None,
        )
        .await
        .unwrap();
        assert_eq!((found.slots, found.router), (Some(4), false));

        let vllm = FakeModelServer::start(
            FakeKind::Vllm {
                max_model_len: 32768,
            },
            &["Qwen/Q"],
        )
        .unwrap();
        let found = identify(&client(), runtime("vllm").unwrap(), &vllm.base_url(), None).await;
        assert_eq!(found.unwrap().models, ["Qwen/Q"]);

        // Another app's OpenAI-style list is no runtime of these.
        let other = FakeModelServer::start(FakeKind::Other, &["x"]).unwrap();
        for id in ["ollama", "lmstudio", "llamacpp", "vllm"] {
            let found = identify(&client(), runtime(id).unwrap(), &other.base_url(), None).await;
            assert!(found.is_none(), "{id}");
        }
        // Nor is vLLM llama-server, or the other way round.
        let found = identify(
            &client(),
            runtime("llamacpp").unwrap(),
            &vllm.base_url(),
            None,
        )
        .await;
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn ollama_loads_and_frees_with_its_own_calls() {
        let server = FakeModelServer::start(FakeKind::Ollama, &["llama3.2:latest"]).unwrap();
        server.set_load(23_779_979_264, 131_072);
        let ollama = runtime("ollama").unwrap();
        let loaded = load(&client(), ollama, &server.base_url(), "llama3.2:latest")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (loaded.bytes, loaded.window),
            (Some(23_779_979_264), Some(131_072))
        );
        assert!(loaded.expires_at.is_some());
        assert!(
            free(&client(), ollama, &server.base_url(), &loaded, None)
                .await
                .unwrap()
        );
        assert!(server.loaded().is_empty());
        let last = server.requests().pop().unwrap();
        assert_eq!(
            last.body,
            json!({"model": "llama3.2:latest", "keep_alive": 0})
        );
    }

    #[tokio::test]
    async fn lm_studio_and_a_llama_router_free_one_model_and_vllm_none() {
        let client = client();
        let lm = FakeModelServer::start(FakeKind::LmStudio, &["qwen/qwen3-8b"]).unwrap();
        lm.hold("qwen/qwen3-8b", 0, OffsetDateTime::now_utc());
        let lm_studio = runtime("lmstudio").unwrap();
        let held_now = held(&client, lm_studio, &lm.base_url(), None)
            .await
            .unwrap();
        assert_eq!(held_now[0].instance.as_deref(), Some("qwen/qwen3-8b"));
        assert!(
            free(&client, lm_studio, &lm.base_url(), &held_now[0], None)
                .await
                .unwrap()
        );
        assert!(lm.loaded().is_empty());

        let router = FakeKind::LlamaServer {
            slots: 2,
            router: true,
        };
        let router = FakeModelServer::start(router, &["a.gguf", "b.gguf"]).unwrap();
        router.hold("b.gguf", 0, OffsetDateTime::now_utc());
        let llama = runtime("llamacpp").unwrap();
        let found = identify(&client, llama, &router.base_url(), None)
            .await
            .unwrap();
        assert!(found.router);
        let held_now = held(&client, llama, &router.base_url(), None)
            .await
            .unwrap();
        assert_eq!(held_now.len(), 1);
        assert!(
            free(&client, llama, &router.base_url(), &held_now[0], None)
                .await
                .unwrap()
        );
        assert!(router.loaded().is_empty());

        let vllm = runtime("vllm").unwrap();
        let any = Held {
            model: "Qwen/Q".to_owned(),
            instance: None,
            bytes: None,
            window: None,
            expires_at: None,
        };
        assert!(
            !free(&client, vllm, "http://127.0.0.1:9/v1", &any, None)
                .await
                .unwrap()
        );
    }
}
