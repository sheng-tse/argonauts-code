//! Who writes the summaries. A summarizer takes one request, ten numbered actions under the
//! prompt, and answers with their lines; every summary of a map comes from the same one.

use std::time::Duration;

use argonauts_core::base::future::BoxFuture;
use argonauts_core::base::names::ModelRef;
use serde_json::json;

/// A model that writes summaries.
pub(crate) trait Summarizer: Send + Sync {
    /// As the store keys its summaries, such as `ollama/qwen3-coder:30b`.
    fn id(&self) -> &ModelRef;
    /// As you know it, such as `Qwen3 Coder 30B`, for saying what went wrong.
    fn name(&self) -> &str;
    /// The reply to one request, or what went wrong in a sentence that names the model.
    fn answer<'a>(&'a self, request: &'a str) -> BoxFuture<'a, Result<String, String>>;
}

/// A model on a server on this machine, asked through its OpenAI-style API: Ollama, LM Studio,
/// llama-server and vLLM all speak it.
pub(crate) struct LocalServer {
    id: ModelRef,
    name: String,
    /// Ends in `/v1`.
    base_url: String,
    client: reqwest::Client,
}

/// A local model on a laptop may take a minute over ten actions.
const LOCAL_ANSWER_TIMEOUT: Duration = Duration::from_secs(600);

impl LocalServer {
    /// `runtime` is the server's id, such as `ollama`, `model` the name it serves, and `name`
    /// what you know it as.
    pub fn new(runtime: &str, base_url: &str, model: &str, name: &str) -> Option<LocalServer> {
        let client = argonauts_endpoints::session::runtime_api::local_client(LOCAL_ANSWER_TIMEOUT)?;
        Some(LocalServer {
            id: ModelRef {
                agent: runtime.to_owned(),
                model: Some(model.to_owned()),
            },
            name: name.to_owned(),
            base_url: base_url.trim_end_matches('/').to_owned(),
            client,
        })
    }
}

impl Summarizer for LocalServer {
    fn id(&self) -> &ModelRef {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn answer<'a>(&'a self, request: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let body = json!({
                "model": self.id.model,
                "temperature": 0,
                "stream": false,
                "messages": [{ "role": "user", "content": request }],
            });
            let url = format!("{}/chat/completions", self.base_url);
            let unanswered = |why: String| format!("{} didn't answer ({why})", self.name);
            let response = self
                .client
                .post(&url)
                .json(&body)
                .send()
                .await
                .map_err(|err| unanswered(format!("{url}: {err}")))?;
            let status = response.status();
            if !status.is_success() {
                return Err(unanswered(format!("{url} answered {status}")));
            }
            let reply: serde_json::Value = response
                .json()
                .await
                .map_err(|err| unanswered(format!("{url}: {err}")))?;
            reply["choices"][0]["message"]["content"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| unanswered(format!("{url} sent no reply")))
        })
    }
}

#[cfg(test)]
pub(crate) mod fakes {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Answers each numbered action with `purpose of <its text>`, and counts what it was asked.
    pub struct Echo {
        id: ModelRef,
        name: String,
        pub requests: AtomicUsize,
        pub asked: Mutex<Vec<String>>,
        /// Fails every request from this one on, counted from 0.
        pub fail_from: Option<usize>,
    }

    impl Echo {
        pub fn new(id: &str) -> Echo {
            Echo {
                id: id.parse().expect("a model"),
                name: format!("the fake {id}"),
                requests: AtomicUsize::new(0),
                asked: Mutex::new(Vec::new()),
                fail_from: None,
            }
        }

        pub fn requests(&self) -> usize {
            self.requests.load(Ordering::SeqCst)
        }

        /// Every action line it was sent, in order.
        pub fn lines(&self) -> Vec<String> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl Summarizer for Echo {
        fn id(&self) -> &ModelRef {
            &self.id
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn answer<'a>(&'a self, request: &'a str) -> BoxFuture<'a, Result<String, String>> {
            Box::pin(async move {
                let made = self.requests.fetch_add(1, Ordering::SeqCst);
                if self.fail_from.is_some_and(|from| made >= from) {
                    return Err("the fake stopped answering".to_owned());
                }
                let actions: Vec<&str> = request
                    .lines()
                    .skip_while(|line| !line.starts_with("1. "))
                    .collect();
                self.asked
                    .lock()
                    .unwrap()
                    .extend(actions.iter().map(|line| (*line).to_owned()));
                Ok(actions
                    .iter()
                    .map(|line| {
                        let (number, text) = line.split_once(". ").unwrap_or(("0", line));
                        format!("{number}. purpose of {text}")
                    })
                    .collect::<Vec<_>>()
                    .join("\n"))
            })
        }
    }

    /// Answers every request with the same reply.
    pub struct Says {
        id: ModelRef,
        name: String,
        reply: String,
    }

    impl Says {
        pub fn new(id: &str, name: &str, reply: &str) -> Says {
            Says {
                id: id.parse().expect("a model"),
                name: name.to_owned(),
                reply: reply.to_owned(),
            }
        }
    }

    impl Summarizer for Says {
        fn id(&self) -> &ModelRef {
            &self.id
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn answer<'a>(&'a self, _request: &'a str) -> BoxFuture<'a, Result<String, String>> {
            Box::pin(async move { Ok(self.reply.clone()) })
        }
    }

    /// A summarizer that fails the test when it's asked anything.
    pub struct Refuses(pub ModelRef);

    impl Summarizer for Refuses {
        fn id(&self) -> &ModelRef {
            &self.0
        }

        fn name(&self) -> &str {
            "the summarizer that must not be asked"
        }

        fn answer<'a>(&'a self, _request: &'a str) -> BoxFuture<'a, Result<String, String>> {
            panic!("the summarizer was called")
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_testkit::{FakeOpenAi, FakeReply};

    use super::*;

    #[tokio::test]
    async fn a_local_server_is_asked_through_its_openai_style_api() {
        let server = FakeOpenAi::start().unwrap();
        server.push(FakeReply::text("1. Read recent git history"));
        let summarizer = LocalServer::new(
            "ollama",
            &server.base_url(),
            "qwen3-coder:30b",
            "Qwen3 Coder 30B",
        )
        .unwrap();
        assert_eq!(summarizer.id().to_string(), "ollama/qwen3-coder:30b");
        let reply = summarizer.answer("the request").await.unwrap();
        assert_eq!(reply, "1. Read recent git history");
        let asked = server.requests();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].path, "/v1/chat/completions");
        assert_eq!(asked[0].body["model"], "qwen3-coder:30b");
        assert_eq!(asked[0].body["messages"][0]["content"], "the request");
    }

    #[tokio::test]
    async fn a_server_error_says_the_model_didnt_answer_and_why() {
        let server = FakeOpenAi::start().unwrap();
        server.push(FakeReply::error(500, "out of memory"));
        let summarizer = LocalServer::new(
            "ollama",
            &server.base_url(),
            "qwen3-coder:30b",
            "Qwen3 Coder 30B",
        )
        .unwrap();
        let why = summarizer.answer("the request").await.unwrap_err();
        assert!(
            why.starts_with("Qwen3 Coder 30B didn't answer (")
                && why.ends_with("/v1/chat/completions answered 500 Internal Server Error)"),
            "{why}"
        );
    }
}
