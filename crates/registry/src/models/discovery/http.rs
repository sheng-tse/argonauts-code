//! Asking one model server for its lists, with the key and headers its settings name. Every
//! answer is read up to [`MAX_MODELS_BYTES`], and no message carries the key.

use argonauts_core::settings::Settings;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use super::MAX_MODELS_BYTES;
use crate::models::builtin::endpoint_settings;

/// Why a server gave no answer. A server that isn't listening at all is told apart, since a
/// built-in one you never set up is often simply not running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Unanswered {
    pub why: String,
    pub not_running: bool,
}

impl Unanswered {
    fn new(why: String) -> Self {
        Self {
            why,
            not_running: false,
        }
    }
}

/// An answer that came but can't be read.
impl From<String> for Unanswered {
    fn from(why: String) -> Self {
        Self::new(why)
    }
}

/// One endpoint as discovery asks it.
#[derive(Debug, Clone)]
pub(super) struct Server {
    pub id: String,
    pub local: bool,
    /// Its `base_url`, where the OpenAI-style API answers, such as `http://localhost:11434/v1`.
    base_url: String,
    client: reqwest::Client,
    headers: HeaderMap,
}

impl Server {
    /// The key comes only from the variable `api_key_env` names.
    pub fn new(client: &reqwest::Client, settings: &Settings, id: &str) -> Result<Self, String> {
        let endpoint = endpoint_settings(settings, id)
            .ok_or_else(|| format!("endpoint {id} has no settings"))?;
        let base_url = endpoint
            .base_url
            .as_deref()
            .ok_or_else(|| format!("endpoint {id} has no base_url"))?
            .trim_end_matches('/')
            .to_owned();
        let mut headers = HeaderMap::new();
        let key = endpoint
            .api_key_env
            .as_deref()
            .and_then(|name| std::env::var(name).ok())
            .filter(|key| !key.is_empty());
        if let Some(key) = key
            && let Ok(mut value) = HeaderValue::from_str(&format!("Bearer {key}"))
        {
            value.set_sensitive(true);
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        for (header, variable) in &endpoint.header_env {
            let Ok(value) = std::env::var(variable) else {
                continue;
            };
            if let (Ok(name), Ok(mut value)) = (
                HeaderName::from_bytes(header.as_bytes()),
                HeaderValue::from_str(&value),
            ) {
                value.set_sensitive(true);
                headers.insert(name, value);
            }
        }
        Ok(Self {
            id: id.to_owned(),
            local: endpoint.local,
            base_url,
            client: client.clone(),
            headers,
        })
    }

    /// The server's own root, below the OpenAI-style API: `http://localhost:11434` for
    /// `http://localhost:11434/v1`. Native lists such as Ollama's `/api/tags` hang from it.
    pub fn root(&self) -> &str {
        self.base_url.strip_suffix("/v1").unwrap_or(&self.base_url)
    }

    /// `GET {base_url}/models`, the OpenAI-style list every server here answers.
    pub async fn openai_models(&self) -> Result<String, Unanswered> {
        let url = format!("{}/models", self.base_url);
        self.read(self.client.get(&url), "GET", &url).await
    }

    /// `GET {root}{path}`.
    pub async fn get(&self, path: &str) -> Result<String, Unanswered> {
        let url = format!("{}{path}", self.root());
        self.read(self.client.get(&url), "GET", &url).await
    }

    /// `POST {root}{path}` with a JSON body.
    pub async fn post(&self, path: &str, body: &Value) -> Result<String, Unanswered> {
        let url = format!("{}{path}", self.root());
        self.read(self.client.post(&url).json(body), "POST", &url)
            .await
    }

    async fn read(
        &self,
        request: reqwest::RequestBuilder,
        method: &str,
        url: &str,
    ) -> Result<String, Unanswered> {
        let id = &self.id;
        let unread = |e: reqwest::Error| {
            Unanswered::new(format!(
                "endpoint {id}: reading {url} failed: {}",
                e.without_url()
            ))
        };
        let mut reply = request
            .headers(self.headers.clone())
            .send()
            .await
            .map_err(|e| Unanswered {
                not_running: e.is_connect(),
                why: format!("endpoint {id}: {method} {url} failed: {}", e.without_url()),
            })?;
        let status = reply.status();
        if !status.is_success() {
            return Err(Unanswered::new(format!(
                "endpoint {id}: {method} {url} answered {status}"
            )));
        }
        let mut body = Vec::new();
        while let Some(chunk) = reply.chunk().await.map_err(unread)? {
            if body.len() + chunk.len() > MAX_MODELS_BYTES {
                let mib = MAX_MODELS_BYTES / (1024 * 1024);
                return Err(Unanswered::new(format!(
                    "endpoint {id}: {method} {url} sent more than {mib} MiB"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    }
}
