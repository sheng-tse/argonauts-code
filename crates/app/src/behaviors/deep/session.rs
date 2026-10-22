//! A CLI's model as the summarizer: each request is one session with no tools, started in an
//! empty folder, so the model reads only the actions it's sent and runs nothing.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use argonauts_core::events::session::{EndReason, InputPart, InputPartKind};
use argonauts_core::future::BoxFuture;
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::session_runner::{
    Access, CliWorkflow, RunnerEvent, SandboxMode, SandboxProfile, SessionLimits,
    SessionPreferences, SessionRunner, SessionShell, SessionSpec,
};
use argonauts_core::settings::sessions::{Compaction, FanoutSwitch, NativeFanout, SessionLifetime};
use argonauts_core::{AgentEvent, create_private_dir_all};

use super::summarize::Summarizer;

pub(crate) struct SessionSummarizer {
    runner: Arc<dyn SessionRunner>,
    id: ModelRef,
    effort: Option<String>,
    /// Each request's session gets a folder of its own under it.
    scratch: PathBuf,
    sessions: AtomicU32,
}

impl SessionSummarizer {
    pub fn new(
        runner: Arc<dyn SessionRunner>,
        effort: Option<String>,
        scratch: PathBuf,
    ) -> SessionSummarizer {
        SessionSummarizer {
            id: runner.model_ref().clone(),
            runner,
            effort,
            scratch,
            sessions: AtomicU32::new(0),
        }
    }

    fn spec(&self, request: &str) -> std::io::Result<SessionSpec> {
        let n = self.sessions.fetch_add(1, Ordering::SeqCst) + 1;
        let folder = self.scratch.join(format!("summary-{n:03}"));
        let workdir = folder.join("work");
        let session_dir = folder.join("session");
        create_private_dir_all(&workdir)?;
        create_private_dir_all(&session_dir)?;
        Ok(SessionSpec {
            run: None,
            task: None,
            key: SessionKey::numbered(n),
            role: RoleName::Main,
            step: None,
            model: self.id.clone(),
            effort: self.effort.clone(),
            access: Access::NoTools,
            workdir: workdir.clone(),
            session_dir,
            prompt: vec![InputPart {
                kind: InputPartKind::Prompt,
                label: "actions to summarize".to_owned(),
                text: request.to_owned(),
                approx_tokens: (request.len() / 4) as u32,
                source: None,
            }],
            json_schema: None,
            resume: None,
            fork: false,
            lifetime: SessionLifetime::Task,
            keep_input_open: false,
            limits: SessionLimits::default(),
            goal_sha256: None,
            sandbox: SandboxProfile {
                mode: SandboxMode::None,
                writable: vec![workdir],
                network: false,
                ..SandboxProfile::default()
            },
            compaction: Compaction::Off,
            native_fanout: NativeFanout::Switch(FanoutSwitch::Off),
            isolated: true,
            autopilot: true,
            status_file: None,
            env: Vec::new(),
            asking: None,
            cli_workflow: CliWorkflow::default(),
            shell: SessionShell::default(),
            preferences: SessionPreferences {
                thinking_off: true,
                ..SessionPreferences::default()
            },
        })
    }

    /// One request's session, to its end.
    async fn run(&self, spec: SessionSpec) -> Result<String, String> {
        let mut handle = self
            .runner
            .start(spec)
            .await
            .map_err(|err| format!("{} couldn't start: {err}", self.id))?;
        let mut said = String::new();
        let mut streamed = String::new();
        while let Some(event) = handle.events.recv().await {
            match event {
                RunnerEvent::Agent(AgentEvent::Message { text, parent: None }) => {
                    said.push_str(&text);
                    said.push('\n');
                }
                RunnerEvent::Agent(AgentEvent::TextDelta { text, parent: None }) => {
                    streamed.push_str(&text);
                }
                _ => {}
            }
        }
        let end = handle
            .ended
            .await
            .map_err(|_| format!("{} ended without saying how", self.id))?;
        if end.reason != EndReason::Completed {
            return Err(format!("{} stopped: {:?}", self.id, end.reason));
        }
        let reply = end
            .result_text
            .filter(|text| !text.trim().is_empty())
            .or_else(|| (!said.trim().is_empty()).then_some(said))
            .unwrap_or(streamed);
        if reply.trim().is_empty() {
            return Err(format!("{} answered nothing", self.id));
        }
        Ok(reply)
    }
}

/// A request's folders, which go once it has answered, or when `x` stops it unanswered.
struct RequestFolder(PathBuf);

impl Drop for RequestFolder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Drop for SessionSummarizer {
    /// Its folder goes once every request's has; `remove_dir` leaves one still in use alone.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.scratch);
    }
}

impl Summarizer for SessionSummarizer {
    fn id(&self) -> &ModelRef {
        &self.id
    }

    fn answer<'a>(&'a self, request: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let spec = self.spec(request).map_err(|err| err.to_string())?;
            let _folder = spec
                .session_dir
                .parent()
                .map(|folder| RequestFolder(folder.to_path_buf()));
            self.run(spec).await
        })
    }
}

#[cfg(test)]
mod tests {
    use argonauts_testkit::{FakeRunner, FakeSession};

    use super::*;

    #[tokio::test]
    async fn each_request_is_a_session_with_no_tools_in_an_empty_folder() {
        let runner = Arc::new(
            FakeRunner::cli("claude-code/haiku", "anthropic")
                .then(FakeSession::completed("1. Read recent git history")),
        );
        let scratch = tempfile::tempdir().unwrap();
        let summarizer = SessionSummarizer::new(
            runner.clone(),
            Some("low".to_owned()),
            scratch.path().into(),
        );
        let reply = summarizer.answer("the request").await.unwrap();
        assert_eq!(reply.trim(), "1. Read recent git history");
        let specs = runner.specs();
        assert_eq!(specs.len(), 1);
        let spec = &specs[0];
        assert_eq!(spec.access, Access::NoTools);
        assert!(spec.isolated);
        assert_eq!(spec.effort.as_deref(), Some("low"));
        assert_eq!(spec.prompt[0].text, "the request");
        assert!(spec.workdir.starts_with(scratch.path()));
        assert!(!spec.workdir.exists(), "its folders go once it answered");
    }

    #[tokio::test]
    async fn a_request_stopped_unanswered_leaves_no_folder_behind() {
        let runner = Arc::new(
            FakeRunner::cli("claude-code/haiku", "anthropic")
                .then(FakeSession::until_stopped(Vec::new())),
        );
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("summaries");
        let summarizer = SessionSummarizer::new(runner, None, root.clone());
        let unanswered = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            summarizer.answer("the request"),
        )
        .await;
        assert!(unanswered.is_err(), "stopped before it answered");
        assert!(!root.join("summary-001").exists());
        drop(summarizer);
        assert!(!root.exists(), "the summarizer's folder goes with it");
    }

    #[tokio::test]
    async fn a_session_that_doesnt_complete_is_a_failed_request() {
        let runner = Arc::new(
            FakeRunner::cli("codex/gpt-6-luna", "openai")
                .then(FakeSession::ended(EndReason::Stopped)),
        );
        let scratch = tempfile::tempdir().unwrap();
        let summarizer = SessionSummarizer::new(runner, None, scratch.path().into());
        assert!(summarizer.answer("the request").await.is_err());
    }
}
