//! A session whose model a local runtime serves, such as OpenCode on `ollama/qwen3-coder:30b`.
//! The CLI talks to the server itself, so Argonauts does around it what its own endpoint
//! sessions do: before the launch the server's memory notes the model for the run or
//! conversation, and Ollama loads it with a request that generates nothing, so the window it
//! gets is known before the first turn; the session holds one of the server's request places
//! for as long as it runs, so a second session waits its turn instead of sharing it; and when it
//! ends, what the server says of the model is noted.

use argonauts_core::Paths;
use argonauts_core::agents::runtimes::{RuntimeKind, local_route};
use argonauts_core::agents::session::SessionRequest;
use argonauts_core::agents::session_runner::SessionSpec;
use argonauts_core::models::model_memory::{Holder, Server};
use argonauts_core::models::server_requests::ServerRequests;
use argonauts_core::settings::Settings;
use argonauts_core::settings::agents::EndpointSettings;
use argonauts_core::settings::endpoints::{endpoint_settings, requests_at_once};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LocalModel {
    /// The runtime's id, which names its `[endpoints.<id>]` table.
    pub id: &'static str,
    pub kind: RuntimeKind,
    pub model: String,
    pub endpoint: EndpointSettings,
    /// `None` for a server that isn't on this machine, whose memory isn't Argonauts' to free.
    pub server: Option<Server>,
    pub holder: Holder,
}

impl LocalModel {
    /// The model the launch runs, when a known runtime serves it.
    pub(crate) fn of(
        settings: &Settings,
        spec: &SessionSpec,
        request: &SessionRequest,
    ) -> Option<Self> {
        let (runtime, model) = local_route(request.model.as_deref()?)?;
        let endpoint = endpoint_settings(settings, runtime.id)?;
        Some(Self {
            id: runtime.id,
            kind: runtime.kind,
            model: model.to_owned(),
            server: Server::local(runtime.id, &endpoint),
            endpoint,
            holder: Holder::of(spec.run.as_ref()),
        })
    }

    /// Only Ollama loads a model on an empty request and says the window it gave it.
    pub(crate) fn loads_first(&self) -> bool {
        self.kind == RuntimeKind::Ollama
    }

    /// The server's request count, as many at once as it takes. `reported` is what the server
    /// said it has slots for.
    pub(crate) fn requests(&self, home: &Paths, reported: Option<u32>) -> Option<ServerRequests> {
        let base_url = self.endpoint.base_url.as_deref()?;
        let number = requests_at_once(self.id, &self.endpoint, reported);
        Some(ServerRequests::new(home, base_url, number))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use argonauts_core::agents::session_runner::SessionRunner;
    use argonauts_core::base::future::BoxFuture;
    use argonauts_core::models::model_memory::{Hold, ModelMemory};
    use argonauts_core::runs::events::session::EndReason;
    use argonauts_core::{AgentEvent, RunId};

    use super::*;
    use crate::cli_runner::testing::{TestAdapter, TestDriver, collect, event_line, spec_in};

    fn request(model: &str) -> SessionRequest {
        SessionRequest {
            model: Some(model.to_owned()),
            ..SessionRequest::default()
        }
    }

    fn spec() -> SessionSpec {
        let dir = std::env::temp_dir();
        spec_in(&dir, &dir, "hi")
    }

    #[test]
    fn a_route_on_a_runtime_is_a_local_model() {
        let settings = Settings::default();
        let mut spec = spec();
        spec.run = Some(RunId::new("r-0001").unwrap());
        let local =
            LocalModel::of(&settings, &spec, &request("ollama/qwen3-coder:30b-64k")).unwrap();
        assert_eq!(
            (local.id, local.model.as_str()),
            ("ollama", "qwen3-coder:30b-64k")
        );
        assert!(local.loads_first());
        assert_eq!(local.holder.key, "run r-0001");
        assert_eq!(local.server.unwrap().base_url, "http://localhost:11434/v1");

        let vllm = LocalModel::of(&settings, &spec, &request("vllm/Qwen/Qwen3-8B")).unwrap();
        assert_eq!(vllm.model, "Qwen/Qwen3-8B");
        assert!(!vllm.loads_first());

        assert!(LocalModel::of(&settings, &spec, &request("opencode/big-pickle")).is_none());
        assert!(LocalModel::of(&settings, &spec, &request("gpt-6-luna")).is_none());
    }

    /// What a session asked of the server's memory, in order.
    #[derive(Default)]
    struct Asked(std::sync::Mutex<Vec<String>>);

    impl ModelMemory for Asked {
        fn hold<'a>(
            &'a self,
            server: &'a Server,
            model: &'a str,
            holder: &'a Holder,
        ) -> BoxFuture<'a, Hold> {
            self.0.lock().unwrap().push(format!(
                "hold {} {model} for {}",
                server.endpoint, holder.key
            ));
            Box::pin(async { Hold::default() })
        }

        fn load<'a>(&'a self, server: &'a Server, model: &'a str) -> BoxFuture<'a, Option<u64>> {
            self.0
                .lock()
                .unwrap()
                .push(format!("load {} {model}", server.endpoint));
            Box::pin(async { Some(65_536) })
        }

        fn touch<'a>(&'a self, server: &'a Server, model: &'a str) -> BoxFuture<'a, ()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("touch {} {model}", server.endpoint));
            Box::pin(async {})
        }
    }

    #[tokio::test]
    async fn a_session_on_ollama_loads_first_and_waits_for_the_servers_place() {
        let mut settings = Settings::default();
        settings.endpoints.insert(
            "ollama".to_owned(),
            EndpointSettings {
                base_url: Some("http://localhost:11434/v1".to_owned()),
                ..EndpointSettings::default()
            },
        );
        let answer = event_line(&AgentEvent::Result {
            text: Some("done".into()),
            structured: None,
            is_error: false,
        });
        let script = format!("echo '{}'", answer.replace('\'', r"'\''"));
        let mut driver = TestDriver::with_settings(TestAdapter::new(&script), settings);
        let asked = Arc::new(Asked::default());
        driver.worker = driver.worker.with_memory(asked.clone());
        let home = Paths::new(driver.home.path());

        // Another session holds Ollama's one place.
        let requests = ServerRequests::new(&home, "http://localhost:11434/v1", 1);
        let taken = requests.take().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec_in(dir.path(), &dir.path().join("session"), "hi");
        spec.model = "fake/ollama/qwen3-coder:30b-64k".parse().unwrap();
        let mut handle = driver.worker.start(spec).await.unwrap();
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert!(matches!(
            handle.ended.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        // Held at once, but not loaded while another session has the server.
        assert_eq!(
            *asked.0.lock().unwrap(),
            ["hold ollama qwen3-coder:30b-64k for conversation"]
        );

        drop(taken);
        let (_, end) = collect(handle).await;
        assert_eq!(end.reason, EndReason::Completed);
        assert_eq!(
            *asked.0.lock().unwrap(),
            [
                "hold ollama qwen3-coder:30b-64k for conversation",
                "load ollama qwen3-coder:30b-64k",
                "touch ollama qwen3-coder:30b-64k",
            ]
        );
    }

    #[test]
    fn a_moved_server_still_counts_its_requests_but_keeps_its_memory() {
        let mut settings = Settings::default();
        settings.endpoints.insert(
            "ollama".to_owned(),
            EndpointSettings {
                base_url: Some("http://gpu-box:11434/v1".to_owned()),
                ..EndpointSettings::default()
            },
        );
        let local = LocalModel::of(&settings, &spec(), &request("ollama/llama3.2")).unwrap();
        assert!(local.server.is_none());
        let home = tempfile::tempdir().unwrap();
        let requests = local.requests(&Paths::new(home.path()), None).unwrap();
        assert_eq!(requests.count(), 1);
    }
}
