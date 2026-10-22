//! A fresh no-tools session for the judge and the narrator: `Access::NoTools`, an empty
//! scratch directory as the working directory, nothing writable, and a new session each call,
//! so neither can read the repo or reach an agent.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::layout;
use argonauts_core::names::{RoleName, SessionKey};
use argonauts_core::redact::redact_secrets;
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::session_runner::{
    Access, RunnerKind, SandboxMode, SandboxProfile, SessionLimits, SessionRunner, SessionSpec,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
use argonauts_core::settings::sessions::{Compaction, FanoutSwitch, NativeFanout, SessionLifetime};
use argonauts_core::steps::{
    StepError, StepName, StepObserver, StepOutcome, StepRequest, StepRunner,
};
use argonauts_core::{RunId, Spend};
use serde_json::Value;
use tokio::time;

pub struct NoToolsSession<'a> {
    pub worker: &'a dyn SessionRunner,
    pub role: RoleName,
    pub step: StepName,
    pub run: Option<RunId>,
    /// Such as `judge-approval-003-1`. It never repeats, so no CLI session is resumed.
    pub key: SessionKey,
    /// The session's private folder; its empty `work/` subfolder is the working directory.
    pub session_dir: PathBuf,
    pub prompt: Vec<InputPart>,
    /// The step's output schema.
    pub schema: Value,
    /// For the effort: `roles.<role>.effort`, else `agents.<cli>.effort`.
    pub settings: &'a Settings,
    /// What the session may spend, such as what's left of the narrator's cap.
    pub limit: Option<LimitAmount>,
}

/// The scratch folder inside a session's private folder.
pub const WORKDIR: &str = "work";

/// Past this many sessions with one prefix something is wrong, so claiming stops instead of
/// looping.
const MAX_SESSIONS_PER_PREFIX: u32 = 10_000;

/// Wall-clock minutes a session may run. One answer from a fenced prompt needs far less.
pub const MAX_MINUTES: f64 = 5.0;
/// Minutes with nothing from the model before the worker stops it, such as a CLI stuck
/// at a login prompt or in a rate-limit retry loop.
pub const INACTIVITY_MIN: f64 = 3.0;
/// How long a caller waits for the step itself. A minute past `MAX_MINUTES`, so the worker's
/// own stop, with its spend, comes first, and a worker that never stops still can't hang the
/// caller.
pub const DEADLINE: Duration = Duration::from_secs(6 * 60);
/// How long a session stopped at the deadline gets to report its spend.
const STOP_GRACE: Duration = Duration::from_secs(15);

/// Creates the empty scratch folder and describes the session. An endpoint gets no sandbox,
/// since it has no process; a CLI gets the OS sandbox with nothing writable.
pub fn no_tools_spec(session: NoToolsSession<'_>) -> io::Result<SessionSpec> {
    let workdir = session.session_dir.join(WORKDIR);
    fs::create_dir_all(&workdir)?;
    // A folder with anything in it could hand the model files, so it's refused, not reused.
    if fs::read_dir(&workdir)?.next().is_some() {
        return Err(io::Error::other(format!(
            "the scratch folder {} isn't empty",
            workdir.display()
        )));
    }

    let worker = session.worker.model_ref().clone();
    let effort = session
        .settings
        .roles
        .get(&session.role)
        .and_then(|role| role.effort.clone())
        .or_else(|| {
            session
                .settings
                .agents
                .get(&worker.agent)
                .and_then(|agent| agent.effort.clone())
        });
    let sandbox = match session.worker.kind() {
        RunnerKind::Endpoint => SandboxProfile {
            mode: SandboxMode::None,
            ..SandboxProfile::default()
        },
        RunnerKind::Cli => SandboxProfile {
            mode: SandboxMode::Os,
            writable: Vec::new(),
            readable: Vec::new(),
            deny_read: Vec::new(),
            network: false,
            proxy_port: None,
            ask_port: None,
            closed: Vec::new(),
            listable: Vec::new(),
            loopback: false,
            outside: Default::default(),
        },
    };

    Ok(SessionSpec {
        run: session.run,
        task: None,
        key: session.key,
        role: session.role,
        step: Some(session.step),
        model: worker,
        effort,
        access: Access::NoTools,
        workdir,
        session_dir: session.session_dir,
        prompt: session.prompt,
        json_schema: Some(session.schema),
        resume: None,
        fork: false,
        lifetime: SessionLifetime::Task,
        keep_input_open: false,
        limits: limits(session.limit),
        goal_sha256: None,
        sandbox,
        compaction: Compaction::Inherit,
        native_fanout: NativeFanout::Switch(FanoutSwitch::Off),
        // The judge judges a call; the CLI's own servers and plugins have no part in that.
        isolated: true,
        autopilot: false,
        status_file: None,
        env: Vec::new(),
        asking: None,
        cli_workflow: argonauts_core::session_runner::CliWorkflow::default(),
        shell: argonauts_core::session_runner::SessionShell::None,
        preferences: Default::default(),
    })
}

fn limits(limit: Option<LimitAmount>) -> SessionLimits {
    SessionLimits {
        limit,
        max_minutes: Some(MAX_MINUTES),
        inactivity_min: Some(INACTIVITY_MIN),
        // One answer has nothing to wrap up, so the CLI's own cap sits at the limit itself
        // rather than a little above it.
        native_cap_usd: limit
            .filter(|limit| limit.unit == LimitUnit::Usd)
            .map(|limit| limit.amount),
        ..SessionLimits::default()
    }
}

/// A step still going at its deadline, so it was stopped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimedOut {
    /// What the worker reported on stopping, and zero when it never did.
    pub spend: Spend,
}

/// Runs the session's step until `deadline`, then stops it, so a worker that never ends the
/// session can't hang the caller.
pub async fn run_step(
    steps: &dyn StepRunner,
    request: StepRequest,
    observer: StepObserver,
    deadline: Duration,
) -> Result<Result<StepOutcome, StepError>, TimedOut> {
    let cancel = request.cancel.clone();
    let mut step = steps.run(request, observer);
    if let Ok(finished) = time::timeout(deadline, &mut step).await {
        return Ok(finished);
    }
    cancel.cancel();
    // Never longer than the deadline itself, so a short deadline stays short.
    let spend = match time::timeout(STOP_GRACE.min(deadline), step).await {
        Ok(Ok(outcome)) => outcome.end.spend,
        Ok(Err(StepError::Session(end))) => end.spend,
        Ok(Err(_)) | Err(_) => Spend::default(),
    };
    Err(TimedOut { spend })
}

/// Claims the next unused `<prefix>-<n>` session folder under `run_dir/sessions/`. Creating the
/// folder is the claim, so two callers never get the same key, even across processes.
pub fn claim_session(run_dir: &Path, prefix: &str) -> io::Result<(SessionKey, PathBuf)> {
    fs::create_dir_all(run_dir.join(layout::run_files::SESSIONS))?;
    for n in 1..=MAX_SESSIONS_PER_PREFIX {
        let name = format!("{prefix}-{n}");
        let key = SessionKey::parse(&name)
            .ok_or_else(|| io::Error::other(format!("{name:?} isn't a valid session key")))?;
        let dir = layout::session_dir(run_dir, &key);
        match fs::create_dir(&dir) {
            Ok(()) => return Ok((key, dir)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::other(format!(
        "more than {MAX_SESSIONS_PER_PREFIX} {prefix} sessions in {}",
        run_dir.display()
    )))
}

/// Text from agents or the repo, ready to show a model or you: control sequences stripped and
/// secrets redacted.
pub fn clean_text(text: &str) -> String {
    redact_secrets(&sanitize_for_terminal(text)).into_owned()
}

/// A prompt part, with the usual estimate of about 4 characters a token.
pub fn part(kind: InputPartKind, label: &str, text: String) -> InputPart {
    let approx_tokens = u32::try_from(text.len().div_ceil(4)).unwrap_or(u32::MAX);
    InputPart {
        kind,
        label: label.to_owned(),
        text,
        approx_tokens,
        source: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::settings::agents::AgentSettings;
    use argonauts_core::settings::roles::RoleSettings;
    use std::sync::Arc;

    use argonauts_core::future::BoxFuture;
    use argonauts_core::session_runner::SessionControl;
    use argonauts_core::steps::{SchemaStepRunner, StepObserver};
    use argonauts_testkit::{FakeRunner, FakeSession};

    fn spec_for(worker: &FakeRunner, settings: &Settings, dir: &Path) -> io::Result<SessionSpec> {
        spec_with_limit(worker, settings, dir, None)
    }

    fn spec_with_limit(
        worker: &FakeRunner,
        settings: &Settings,
        dir: &Path,
        limit: Option<LimitAmount>,
    ) -> io::Result<SessionSpec> {
        no_tools_spec(NoToolsSession {
            worker,
            role: RoleName::Judge,
            step: StepName::ModelJudge,
            run: None,
            key: SessionKey::parse("judge-approval-001-1").unwrap(),
            session_dir: dir.to_owned(),
            prompt: vec![part(InputPartKind::Prompt, "prompt", "hi".to_owned())],
            schema: serde_json::json!({"type": "object"}),
            settings,
            limit,
        })
    }

    /// A step runner whose worker never ends the session, even when told to stop.
    struct Hung;

    impl StepRunner for Hung {
        fn run(
            &self,
            _: StepRequest,
            _: StepObserver,
        ) -> BoxFuture<'_, Result<StepOutcome, StepError>> {
            Box::pin(std::future::pending())
        }
    }

    #[test]
    fn a_cli_gets_the_os_sandbox_with_nothing_writable() {
        let dir = tempfile::tempdir().unwrap();
        let worker = FakeRunner::cli("codex", "openai");
        let spec = spec_for(&worker, &Settings::default(), dir.path()).unwrap();
        assert_eq!(spec.access, Access::NoTools);
        assert_eq!(spec.sandbox.mode, SandboxMode::Os);
        assert!(spec.sandbox.writable.is_empty());
        assert!(!spec.sandbox.network);
        assert_eq!(spec.workdir, dir.path().join(WORKDIR));
        assert!(spec.resume.is_none() && spec.status_file.is_none() && spec.env.is_empty());
        assert_eq!(
            spec.json_schema,
            Some(serde_json::json!({"type": "object"}))
        );
    }

    #[test]
    fn an_endpoint_gets_no_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let worker = FakeRunner::endpoint("openrouter/glm", "zhipu");
        let spec = spec_for(&worker, &Settings::default(), dir.path()).unwrap();
        assert_eq!(spec.sandbox.mode, SandboxMode::None);
    }

    #[test]
    fn the_role_effort_wins_over_the_cli_effort() {
        let dir = tempfile::tempdir().unwrap();
        let worker = FakeRunner::cli("codex", "openai");
        let mut settings = Settings::default();
        settings.agents.insert(
            "codex".to_owned(),
            AgentSettings {
                effort: Some("high".to_owned()),
                ..AgentSettings::default()
            },
        );
        let spec = spec_for(&worker, &settings, &dir.path().join("a")).unwrap();
        assert_eq!(spec.effort.as_deref(), Some("high"));

        settings.roles.insert(
            RoleName::Judge,
            RoleSettings {
                effort: Some("low".to_owned()),
                ..RoleSettings::default()
            },
        );
        let spec = spec_for(&worker, &settings, &dir.path().join("b")).unwrap();
        assert_eq!(spec.effort.as_deref(), Some("low"));
    }

    #[test]
    fn every_session_has_a_wall_clock_and_an_inactivity_limit() {
        let dir = tempfile::tempdir().unwrap();
        let worker = FakeRunner::cli("codex", "openai");
        let spec = spec_for(&worker, &Settings::default(), dir.path()).unwrap();
        assert_eq!(spec.limits.max_minutes, Some(MAX_MINUTES));
        assert_eq!(spec.limits.inactivity_min, Some(INACTIVITY_MIN));
        assert!(spec.limits.limit.is_none() && spec.limits.native_cap_usd.is_none());
        assert!(DEADLINE.as_secs_f64() > MAX_MINUTES * 60.0);
    }

    #[test]
    fn a_dollar_limit_is_also_the_agent_cap() {
        let dir = tempfile::tempdir().unwrap();
        let worker = FakeRunner::cli("claude-code", "anthropic");
        let usd = LimitAmount {
            unit: LimitUnit::Usd,
            amount: 0.3,
        };
        let spec = spec_with_limit(
            &worker,
            &Settings::default(),
            &dir.path().join("a"),
            Some(usd),
        )
        .unwrap();
        assert_eq!(spec.limits.limit, Some(usd));
        assert_eq!(spec.limits.native_cap_usd, Some(0.3));

        let tokens = LimitAmount {
            unit: LimitUnit::Tokens,
            amount: 5_000.0,
        };
        let spec = spec_with_limit(
            &worker,
            &Settings::default(),
            &dir.path().join("b"),
            Some(tokens),
        )
        .unwrap();
        assert_eq!(spec.limits.limit, Some(tokens));
        assert!(spec.limits.native_cap_usd.is_none());
    }

    #[tokio::test]
    async fn a_stalled_session_is_stopped_at_the_deadline_with_its_spend() {
        let dir = tempfile::tempdir().unwrap();
        let spend = Spend {
            input_tokens: 700,
            cost_usd: 0.02,
            ..Spend::default()
        };
        let worker = Arc::new(
            FakeRunner::cli("codex", "openai")
                .then(FakeSession::until_stopped(Vec::new()).with_spend(spend)),
        );
        let spec = spec_for(&worker, &Settings::default(), dir.path()).unwrap();
        let request = StepRequest::new(worker.clone(), spec);
        let ended = run_step(
            &SchemaStepRunner,
            request,
            Box::new(|_| {}),
            Duration::from_millis(20),
        )
        .await;
        assert_eq!(ended.unwrap_err(), TimedOut { spend });
        assert!(
            worker
                .controls()
                .iter()
                .any(|(_, control)| *control == SessionControl::Stop)
        );
    }

    #[tokio::test]
    async fn a_worker_that_never_stops_still_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let worker = Arc::new(FakeRunner::cli("codex", "openai"));
        let spec = spec_for(&worker, &Settings::default(), dir.path()).unwrap();
        let request = StepRequest::new(worker, spec);
        let ended = run_step(&Hung, request, Box::new(|_| {}), Duration::from_millis(20)).await;
        assert_eq!(
            ended.unwrap_err(),
            TimedOut {
                spend: Spend::default()
            }
        );
    }

    #[test]
    fn a_scratch_folder_with_files_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(WORKDIR)).unwrap();
        fs::write(dir.path().join(WORKDIR).join("notes.md"), "hi").unwrap();
        let worker = FakeRunner::cli("codex", "openai");
        assert!(spec_for(&worker, &Settings::default(), dir.path()).is_err());
    }

    #[test]
    fn claimed_keys_never_repeat() {
        let run_dir = tempfile::tempdir().unwrap();
        let (a, _) = claim_session(run_dir.path(), "judge-approval-003").unwrap();
        let (b, dir) = claim_session(run_dir.path(), "judge-approval-003").unwrap();
        assert_eq!(a.as_str(), "judge-approval-003-1");
        assert_eq!(b.as_str(), "judge-approval-003-2");
        assert_eq!(dir, layout::session_dir(run_dir.path(), &b));
    }
}
