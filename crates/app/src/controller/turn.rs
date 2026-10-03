//! One chat turn's session: which worker reads it, what the request's record holds, and the
//! read-only session the step runs in, on its own task.

use std::path::PathBuf;
use std::sync::Arc;

use argonauts_core::NoteLevel;
use argonauts_core::events::session::{EndReason, InputPart, InputPartKind};
use argonauts_core::input::EFFORT_LEVELS;
use argonauts_core::limits::{HealthState, QuotaStatus};
use argonauts_core::names::{ChatId, RoleName, SessionKey, TurnId, WorkerRef};
use argonauts_core::paths::create_private_dir_all;
use argonauts_core::registry::{ModelRegistry, effort_to_run, known_efforts};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::SandboxChoice;
use argonauts_core::settings::handoff::HandoffSettings;
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::steps::intake::{ChatAnswer, TriageEstimate};
use argonauts_core::steps::{
    StepError, StepName, StepObserver, StepOutcome, StepRequest, output_schema,
};
use argonauts_core::worker::{
    Access, SandboxMode, SandboxProfile, SessionControl, SessionLimits, SessionSpec, Worker,
    WorkerError, WorkerEvent, WorkerKind,
};
use tokio_util::sync::CancellationToken;

use super::chat::TurnUpdate;
use super::{Ctx, Internal};
use crate::git;

/// Where the request stood when it was sent.
pub(crate) struct Record {
    pub commit: String,
    /// Left out of a run's base; empty when `intake.include_uncommitted` is on.
    pub uncommitted: Vec<String>,
}

impl Record {
    /// A git failure, such as a repo with no commits yet, leaves that part empty and says so.
    pub(crate) fn now(ctx: &Ctx) -> Self {
        let root = &ctx.env.repo_root;
        let commit = git::head(root).unwrap_or_else(|err| {
            ctx.notice(
                NoteLevel::Warn,
                format!("The request is recorded without a commit: {err:#}"),
            );
            String::new()
        });
        let uncommitted = if ctx.settings().intake.include_uncommitted {
            Vec::new()
        } else {
            git::uncommitted_paths(root).unwrap_or_else(|err| {
                ctx.notice(
                    NoteLevel::Warn,
                    format!("Couldn't list the uncommitted files: {err:#}"),
                );
                Vec::new()
            })
        };
        Self {
            commit,
            uncommitted,
        }
    }

    /// For the prompt, such as "2 uncommitted files are not included".
    pub(crate) fn uncommitted_note(&self) -> Option<String> {
        match self.uncommitted.len() {
            0 => None,
            1 => Some("1 uncommitted file is not included".to_owned()),
            n => Some(format!("{n} uncommitted files are not included")),
        }
    }
}

/// The default agent, then its fallbacks in the order `roles.default.handoff` gives them.
pub(crate) fn candidates(settings: &Settings) -> Vec<WorkerRef> {
    let default = settings.default_agent.clone();
    let role = settings
        .roles
        .get(&RoleName::Default)
        .map(|role| &role.handoff);
    let applied = HandoffSettings::applied(&settings.handoff, role, &default.vendor());
    let mut workers = vec![default];
    for entry in &applied.candidates {
        if !workers.contains(entry.worker()) {
            workers.push(entry.worker().clone());
        }
    }
    workers
}

/// The first candidate not tried yet whose vendor is neither out of quota nor down. When
/// every one is, the first that exists anyway, so its own error says why.
pub(crate) fn pick(
    ctx: &Ctx,
    tried: &[WorkerRef],
) -> Result<(WorkerRef, Arc<dyn Worker>), WorkerError> {
    let mut first_error = None;
    let mut out_of_service = None;
    for worker in candidates(ctx.settings()) {
        if tried.contains(&worker) {
            continue;
        }
        let handle = match ctx.services.workers.get(&worker) {
            Ok(handle) => handle,
            Err(err) => {
                first_error.get_or_insert(err);
                continue;
            }
        };
        if vendor_out(ctx, &worker) {
            out_of_service.get_or_insert((worker, handle));
            continue;
        }
        return Ok((worker, handle));
    }
    out_of_service.ok_or_else(|| {
        first_error.unwrap_or_else(|| WorkerError::Unknown(ctx.settings().default_agent.clone()))
    })
}

/// The default agent itself, for a turn no fallback can take.
pub(crate) fn pick_main(ctx: &Ctx) -> Result<(WorkerRef, Arc<dyn Worker>), WorkerError> {
    let main = ctx.settings().default_agent.clone();
    let handle = ctx.services.workers.get(&main)?;
    Ok((main, handle))
}

pub(crate) fn vendor_out(ctx: &Ctx, worker: &WorkerRef) -> bool {
    let vendor = worker.vendor();
    let down = ctx.services.health.state(&vendor).state == HealthState::Down;
    let out = ctx
        .services
        .quota
        .get(&vendor)
        .is_some_and(|reading| reading.status == QuotaStatus::Rejected);
    down || out
}

/// Failures another vendor might not have, so the next fallback gets the turn.
pub(crate) fn worth_a_fallback(err: &StepError) -> bool {
    match err {
        StepError::Worker(
            WorkerError::NotInstalled(_) | WorkerError::Disabled { .. } | WorkerError::Unknown(_),
        ) => true,
        StepError::Session(end) => matches!(
            end.reason,
            EndReason::UsageLimit { .. }
                | EndReason::RateLimited
                | EndReason::VendorError { .. }
                | EndReason::Auth { .. }
                | EndReason::Billing { .. }
                | EndReason::ModelUnavailable { .. }
                | EndReason::StartFailed { .. }
        ),
        // A refusal or a local failure would repeat with any worker.
        StepError::Worker(WorkerError::Refused(_) | WorkerError::Io(_))
        | StepError::SchemaMismatch { .. } => false,
    }
}

/// One line for the transcript.
pub(crate) fn describe(err: &StepError) -> String {
    match err {
        StepError::Worker(err) => err.to_string(),
        StepError::Session(end) => match &end.reason {
            EndReason::UsageLimit { .. } => "it hit its usage limit".to_owned(),
            EndReason::RateLimited => "it is rate limited".to_owned(),
            EndReason::VendorError { message, .. }
            | EndReason::Auth { message }
            | EndReason::Billing { message }
            | EndReason::StartFailed { message }
            | EndReason::Crashed { message } => message.clone(),
            EndReason::ModelUnavailable { model, .. } => format!("{model} isn't available"),
            EndReason::AdapterBroken { detail } => detail.clone(),
            EndReason::ContextFull => "its context window is full".to_owned(),
            EndReason::Stopped | EndReason::Paused => "it was stopped".to_owned(),
            other => format!("it ended early ({other:?})"),
        },
        StepError::SchemaMismatch { raw: None, .. } => "it ended without an answer".to_owned(),
        StepError::SchemaMismatch { errors, .. } => {
            format!("its answer didn't match the schema: {}", errors.join("; "))
        }
    }
}

/// What the turn asks.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TurnStep {
    /// A new request: answer, change, or unclear.
    Triage { uncommitted_note: Option<String> },
    /// `/ask`: an answer only.
    Ask { run_status: Option<String> },
    /// One of the main agent CLI's own commands. `prompt` is the whole input, so the CLI
    /// reads it as the command, and its reply is plain text with no schema.
    Vendor { prompt: String },
    /// `/btw`: an answer from a session of its own, kept out of the conversation.
    Side,
    /// `/rename` with no name: the lead names the conversation, out of its history too.
    Title,
    /// `/compact`: a summary the next session starts from.
    Compact { instructions: Option<String> },
    /// `/review` and `/security-review`, by whoever [`crate::controller::chat`] picked.
    Review {
        security: bool,
        /// Such as "the uncommitted changes".
        target: String,
        changes: String,
        other_company: bool,
    },
}

impl TurnStep {
    fn name(&self) -> StepName {
        match self {
            Self::Triage { .. } => StepName::TriageEstimate,
            Self::Ask { .. }
            | Self::Vendor { .. }
            | Self::Side
            | Self::Title
            | Self::Compact { .. }
            | Self::Review { .. } => StepName::ChatAnswer,
        }
    }

    /// Only the main agent knows its own commands, so no fallback takes one.
    pub(crate) fn main_agent_only(&self) -> bool {
        matches!(self, Self::Vendor { .. })
    }

    /// Answered in a session of its own and left out of the journal and the history, so the
    /// conversation reads as if it never happened.
    pub(crate) fn kept_out(&self) -> bool {
        matches!(self, Self::Side | Self::Title)
    }

    /// Never resumes the conversation's vendor session, and never becomes it: a side question
    /// would land in its history, and a reviewer may be another vendor's.
    pub(crate) fn own_session(&self) -> bool {
        self.kept_out() || matches!(self, Self::Review { .. })
    }

    fn prompt(&self, text: &str) -> InputPart {
        let (label, prompt) = match self {
            Self::Triage { uncommitted_note } => (
                "triage estimate",
                crate::prompts::triage_estimate(&argonauts_core::steps::intake::IntakeInput {
                    request: text.to_owned(),
                    uncommitted_note: uncommitted_note.clone(),
                }),
            ),
            Self::Ask { run_status } => (
                "chat answer",
                crate::prompts::chat_answer(&argonauts_core::steps::intake::ChatAnswerInput {
                    question: text.to_owned(),
                    run_status: run_status.clone(),
                }),
            ),
            Self::Vendor { prompt } => ("vendor command", prompt.clone()),
            Self::Side => (
                "side question",
                crate::prompts::chat_answer(&argonauts_core::steps::intake::ChatAnswerInput {
                    question: text.to_owned(),
                    run_status: None,
                }),
            ),
            Self::Title => ("name", crate::prompts::title()),
            Self::Compact { instructions } => {
                ("compact", crate::prompts::compact(instructions.as_deref()))
            }
            Self::Review {
                security,
                target,
                changes,
                other_company,
            } => (
                "review",
                crate::prompts::review(&crate::prompts::ReviewInput {
                    security: *security,
                    target,
                    changes,
                    other_company: *other_company,
                }),
            ),
        };
        part(InputPartKind::Prompt, label, prompt)
    }

    fn schema(&self) -> Option<serde_json::Value> {
        match self {
            Self::Triage { .. } => Some(output_schema::<TriageEstimate>()),
            Self::Vendor { .. } => None,
            _ => Some(output_schema::<ChatAnswer>()),
        }
    }
}

pub(crate) fn part(kind: InputPartKind, label: &str, text: String) -> InputPart {
    InputPart {
        kind,
        label: label.to_owned(),
        approx_tokens: u32::try_from(text.len() / 4).unwrap_or(u32::MAX),
        text,
        source: None,
    }
}

/// Everything a turn's session needs besides the settings.
pub(crate) struct Launch<'a> {
    pub chat: &'a ChatId,
    pub turn: &'a TurnId,
    pub step: &'a TurnStep,
    pub text: &'a str,
    pub worker: WorkerRef,
    pub handle: Arc<dyn Worker>,
    /// The vendor session to resume, when it belongs to this worker.
    pub resume: Option<String>,
    /// For a fresh session: the chat so far.
    pub excerpt: Option<InputPart>,
    /// After the step's prompt, such as how to write the answer.
    pub notes: Vec<InputPart>,
    /// `/fast`: the lowest effort the model takes, over the one set.
    pub fast: bool,
}

/// Starts the step on its own task. Its events and its end come back as `Internal::Chat`.
pub(crate) fn launch(
    ctx: &Ctx,
    launch: Launch<'_>,
) -> anyhow::Result<(SessionKey, CancellationToken)> {
    let key_text = format!("{}-{}", launch.chat, launch.turn);
    let key = SessionKey::parse(&key_text)
        .ok_or_else(|| anyhow::anyhow!("{key_text:?} can't name a session"))?;
    let session_dir = ctx
        .env
        .chat_dir()
        .join(launch.chat.as_str())
        .join(launch.turn.as_str());
    create_private_dir_all(&session_dir)
        .map_err(|err| anyhow::anyhow!("creating {}: {err}", session_dir.display()))?;
    let spec = session_spec(ctx, &launch, key.clone(), session_dir);
    let mut request = StepRequest::new(launch.handle, spec);
    let cancel = CancellationToken::new();
    request.cancel = cancel.clone();

    let steps = Arc::clone(&ctx.services.steps);
    let events = ctx.internal.clone();
    let done = ctx.internal.clone();
    let turn = launch.turn.clone();
    let seen = turn.clone();
    let observer: StepObserver = Box::new(move |event: &WorkerEvent| {
        let update = TurnUpdate::Event {
            turn: seen.clone(),
            event: event.clone(),
        };
        let _ = events.send(Internal::Chat(update));
    });
    tokio::spawn(async move {
        let outcome = if request.spec.json_schema.is_none() {
            run_plain(request, observer).await
        } else {
            steps.run(request, observer).await
        };
        // After Exit nobody listens, and the outcome has nowhere to go.
        let _ = done.send(Internal::Chat(TurnUpdate::Done { turn, outcome }));
    });
    Ok((key, cancel))
}

/// A session whose reply is the CLI's own text, such as a vendor command's, so there is no
/// schema to check or retry against. A session that didn't complete is the error.
async fn run_plain(
    request: StepRequest,
    mut observer: StepObserver,
) -> Result<StepOutcome, StepError> {
    let mut handle = request.worker.start(request.spec).await?;
    let mut stop_sent = false;
    loop {
        tokio::select! {
            event = handle.events.recv() => match event {
                Some(event) => observer(&event),
                None => break,
            },
            () = request.cancel.cancelled(), if !stop_sent => {
                stop_sent = true;
                // A full or closed channel means the session is already on its way out.
                let _ = handle.control.try_send(SessionControl::Stop);
            }
        }
    }
    let end = handle
        .ended
        .await
        .map_err(|_| WorkerError::Io("the session ended without saying how".to_owned()))?;
    if end.reason != EndReason::Completed {
        return Err(StepError::Session(Box::new(end)));
    }
    Ok(StepOutcome {
        value: serde_json::Value::Null,
        end,
        attempts: 1,
    })
}

/// The worker and effort a chat turn runs with: the model and effort you set for the agent
/// when `worker` names none, and the default role's effort over the agent's.
pub(crate) fn chat_agent(settings: &Settings, worker: &WorkerRef) -> (WorkerRef, Option<String>) {
    let role = settings.roles.get(&RoleName::Default);
    let agent = settings.agents.get(&worker.worker);
    let mut worker = worker.clone();
    if worker.model.is_none() {
        worker.model = agent.and_then(|agent| agent.model.clone());
    }
    let effort = role
        .and_then(|role| role.effort.clone())
        .or_else(|| agent.and_then(|agent| agent.effort.clone()));
    (worker, effort)
}

/// What `/fast` runs `worker` as: the lowest level its model takes, `low` when they aren't
/// known, or for a model fixed at one level, the listed model of its line at the lowest, as
/// agy lists `gemini-3.8-flash-low` beside `gemini-3.8-flash-high`. `None` when nothing is
/// faster: a model that takes no effort setting, or one fixed at its line's lowest.
pub(crate) fn fast_choice(ctx: &Ctx, worker: &WorkerRef) -> Option<(WorkerRef, String)> {
    let registry = ctx.services.registry.as_ref();
    let Some(levels) = known_efforts(registry, worker) else {
        return Some((worker.clone(), "low".to_owned()));
    };
    match levels.as_slice() {
        [] => None,
        [level] => lowest_sibling(registry, worker, level).filter(|(_, lowest)| lowest != level),
        levels => {
            let lowest = EFFORT_LEVELS
                .iter()
                .find(|level| levels.iter().any(|known| known == *level))
                .map_or("low", |level| *level);
            Some((worker.clone(), lowest.to_owned()))
        }
    }
}

/// Among the listed models named like `worker` but for another level, the one at the lowest.
fn lowest_sibling(
    registry: &dyn ModelRegistry,
    worker: &WorkerRef,
    level: &str,
) -> Option<(WorkerRef, String)> {
    let stem = |model: &str, level: &str| {
        model
            .strip_suffix(level)
            .and_then(|rest| rest.strip_suffix('-'))
            .map(str::to_owned)
    };
    let line = stem(worker.model.as_deref()?, level)?;
    registry
        .list()
        .into_iter()
        .filter(|entry| entry.id.worker == worker.worker)
        .filter_map(|entry| match entry.efforts.as_slice() {
            [only] => {
                let named = stem(entry.id.model.as_deref()?, only)?;
                (named == line).then(|| (entry.id.clone(), only.clone()))
            }
            _ => None,
        })
        .min_by_key(|(_, level)| EFFORT_LEVELS.iter().position(|known| known == level))
}

/// A chat turn reads your checkout. With `sandbox.mode` set to `tool` or `none` it runs outside
/// ours only when the CLI keeps it read-only itself, since macOS refuses a CLI's own sandbox
/// inside ours and its shell commands would fail.
fn chat_sandbox(choice: SandboxChoice, keeps_read_only: bool) -> SandboxMode {
    match choice {
        SandboxChoice::Container => SandboxMode::Container,
        SandboxChoice::Tool | SandboxChoice::None if keeps_read_only => SandboxMode::None,
        SandboxChoice::Os | SandboxChoice::Tool | SandboxChoice::None => SandboxMode::Os,
    }
}

pub(crate) fn session_spec(
    ctx: &Ctx,
    launch: &Launch<'_>,
    key: SessionKey,
    session_dir: PathBuf,
) -> SessionSpec {
    let settings = ctx.settings();
    let role = settings.roles.get(&RoleName::Default);
    let (mut worker, effort) = chat_agent(settings, &launch.worker);
    let mut effort = effort_to_run(ctx.services.registry.as_ref(), &worker, effort);
    if launch.fast
        && let Some((fast, level)) = fast_choice(ctx, &worker)
    {
        worker = fast;
        effort = Some(level);
    }
    let mut prompt: Vec<InputPart> = launch.excerpt.iter().cloned().collect();
    prompt.push(launch.step.prompt(launch.text));
    prompt.extend(launch.notes.iter().cloned());
    // An endpoint has no tools, so it gets an empty folder and no sandbox of its own.
    let endpoint = launch.handle.kind() == WorkerKind::Endpoint;
    let (access, workdir, mode) = if endpoint {
        (Access::NoTools, session_dir.clone(), SandboxMode::None)
    } else {
        let keeps_read_only = ctx.services.workers.enforces_read_only(&worker);
        let mode = chat_sandbox(settings.sandbox.mode, keeps_read_only);
        (Access::ReadOnly, ctx.env.repo_root.clone(), mode)
    };
    let state_root = ctx
        .env
        .state_dir
        .parent()
        .map_or_else(|| ctx.env.state_dir.clone(), |dir| dir.to_owned());
    SessionSpec {
        run: None,
        card: None,
        key,
        role: RoleName::Default,
        step: Some(launch.step.name()),
        worker,
        effort,
        access,
        workdir,
        session_dir,
        prompt,
        json_schema: launch.step.schema(),
        resume: launch.resume.clone(),
        lifetime: SessionLifetime::Run,
        keep_input_open: false,
        limits: SessionLimits::default(),
        goal_sha256: None,
        sandbox: SandboxProfile {
            mode,
            writable: Vec::new(),
            readable: Vec::new(),
            deny_read: vec![state_root],
            network: settings.sandbox.network,
            proxy_port: None,
        },
        compaction: role
            .and_then(|role| role.compaction)
            .unwrap_or(settings.compaction),
        native_fanout: role
            .and_then(|role| role.native_fanout)
            .unwrap_or(settings.native_fanout),
        isolated: false,
        status_file: None,
        env: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::app::{AppEvent, ChatEvent};
    use argonauts_core::registry::ModelRegistry;
    use argonauts_core::settings::handoff::FallbackEntry;
    use argonauts_testkit::TempRepo;
    use argonauts_testkit::worker::session_end;

    use crate::controller::chat::{self, ChatState};
    use crate::controller::test_support::{HarnessOptions, TestCtx, WAIT};
    use crate::host::harnesses::RepoModels;
    use crate::host::services::{LiveRegistry, SettingsSource, directory};

    #[test]
    fn the_default_agent_comes_first_then_its_fallbacks_once_each() {
        let mut settings = Settings {
            default_agent: "claude-code".parse().unwrap(),
            ..Settings::default()
        };
        let mut role = settings
            .roles
            .get(&RoleName::Default)
            .cloned()
            .unwrap_or_default();
        role.handoff.fallback = vec![
            FallbackEntry::Worker("codex".parse().unwrap()),
            FallbackEntry::Worker("claude-code".parse().unwrap()),
            FallbackEntry::Worker("antigravity-cli".parse().unwrap()),
        ];
        settings.roles.insert(RoleName::Default, role);
        let names: Vec<String> = candidates(&settings)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(names, ["claude-code", "codex", "antigravity-cli"]);
    }

    #[test]
    fn vendor_trouble_is_worth_a_fallback_but_a_stop_is_not() {
        let ended = |reason| StepError::Session(Box::new(session_end(reason, None, None)));
        assert!(worth_a_fallback(&ended(EndReason::RateLimited)));
        assert!(worth_a_fallback(&ended(EndReason::UsageLimit {
            resets_at: None
        })));
        assert!(!worth_a_fallback(&ended(EndReason::Stopped)));
        assert!(worth_a_fallback(&StepError::Worker(
            WorkerError::NotInstalled("codex".to_owned())
        )));
        assert_eq!(
            describe(&ended(EndReason::RateLimited)),
            "it is rate limited"
        );
    }

    /// Codex is the default agent and off in the file. Its binary is a path that doesn't exist,
    /// so a turn never reaches a real vendor.
    fn codex_off(repo: &TempRepo) {
        std::fs::write(
            repo.global_config(),
            "default_agent = \"codex\"\n\n[agents.codex]\nenabled = false\nbinary = \"/nonexistent/codex\"\n",
        )
        .unwrap();
    }

    #[tokio::test]
    async fn an_agent_turned_on_in_the_app_takes_the_next_turn() {
        let (mut parts, _) = TestCtx::new(HarnessOptions {
            prepare: Some(Box::new(codex_off)),
            ..HarnessOptions::default()
        });
        let ctx = &mut parts.ctx;
        // The directory the app builds at start, in place of the harness's fake one.
        let repo_models = RepoModels::new(&ctx.env.repo_root, &ctx.env.state_dir);
        let registry: Arc<dyn ModelRegistry> = Arc::new(LiveRegistry::new(
            ctx.settings.follow(),
            Some(repo_models.clone()),
            None,
        ));
        ctx.services.workers = directory(
            SettingsSource::live(&ctx.settings),
            &ctx.env.paths,
            Some(&repo_models),
            Arc::clone(&registry),
            ctx.services.quota.clone(),
            ctx.services.health.clone(),
        );
        ctx.services.registry = registry;
        let mut chat = ChatState::new(ctx).unwrap();

        let off = chat::new_request(ctx, &mut chat, "hi".to_owned())
            .await
            .unwrap_err();
        assert!(
            format!("{off:#}").contains("agents.codex.enabled is false"),
            "{off:#}"
        );

        let env = ctx.env.clone();
        ctx.settings
            .set_session(&env, "agents.codex.enabled", "true")
            .unwrap();
        chat::new_request(ctx, &mut chat, "hi".to_owned())
            .await
            .unwrap();
        let mut started = None;
        while let Ok(event) = parts.events.try_recv() {
            if let AppEvent::Chat {
                event: ChatEvent::TurnStarted { worker, .. },
            } = event
            {
                started = Some(worker);
            }
        }
        assert_eq!(started, Some(WorkerRef::new("codex", None)));
        let done = tokio::time::timeout(WAIT, async {
            loop {
                match parts.internal.recv().await {
                    Some(Internal::Chat(update @ TurnUpdate::Done { .. })) => return update,
                    Some(_) => {}
                    None => panic!("the channel closed"),
                }
            }
        })
        .await
        .expect("the turn ends");
        let TurnUpdate::Done { outcome, .. } = &done else {
            unreachable!()
        };
        let err = outcome.as_ref().err().map(ToString::to_string);
        assert!(
            err.as_deref()
                .is_none_or(|err| !err.contains("enabled is false")),
            "{err:?}"
        );
        chat::on_update(&mut parts.ctx, &mut chat, done)
            .await
            .unwrap();
    }

    /// The sandbox mode a chat turn on `agent` gets under `[sandbox] mode = <mode>`, through the
    /// directory the app builds at start.
    fn chat_sandbox_for(mode: &'static str, agent: &str) -> SandboxMode {
        let (mut parts, _) = TestCtx::new(HarnessOptions {
            prepare: Some(Box::new(move |repo: &TempRepo| {
                let config = format!("[sandbox]\nmode = \"{mode}\"\n");
                std::fs::write(repo.global_config(), config).unwrap();
            })),
            ..HarnessOptions::default()
        });
        let ctx = &mut parts.ctx;
        let repo_models = RepoModels::new(&ctx.env.repo_root, &ctx.env.state_dir);
        let registry: Arc<dyn ModelRegistry> = Arc::new(LiveRegistry::new(
            ctx.settings.follow(),
            Some(repo_models.clone()),
            None,
        ));
        ctx.services.workers = directory(
            SettingsSource::live(&ctx.settings),
            &ctx.env.paths,
            Some(&repo_models),
            registry,
            ctx.services.quota.clone(),
            ctx.services.health.clone(),
        );
        let chat = ChatId::numbered(1);
        let turn = TurnId::numbered(1);
        let step = TurnStep::Ask { run_status: None };
        let launch = Launch {
            chat: &chat,
            turn: &turn,
            step: &step,
            text: "what does main do?",
            worker: agent.parse().unwrap(),
            handle: Arc::new(argonauts_testkit::worker::FakeWorker::harness(
                agent, "test",
            )),
            resume: None,
            excerpt: None,
            notes: Vec::new(),
            fast: false,
        };
        let key = SessionKey::numbered(1);
        let dir = ctx.env.state_dir.join("chat-test");
        session_spec(ctx, &launch, key, dir).sandbox.mode
    }

    #[test]
    fn with_the_sandbox_off_a_chat_turn_leaves_ours_only_for_a_cli_that_keeps_it_read_only() {
        assert_eq!(chat_sandbox_for("off", "claude-code"), SandboxMode::None);
        assert_eq!(chat_sandbox_for("off", "codex"), SandboxMode::None);
        assert_eq!(chat_sandbox_for("off", "antigravity-cli"), SandboxMode::Os);
        assert_eq!(chat_sandbox_for("off", "opencode"), SandboxMode::Os);
        assert_eq!(chat_sandbox_for("os", "claude-code"), SandboxMode::Os);
        assert_eq!(
            chat_sandbox_for("container", "claude-code"),
            SandboxMode::Container
        );
    }

    #[test]
    fn the_chat_runs_the_agents_model_and_the_default_roles_effort() {
        let mut settings = Settings::default();
        let agent = settings.agents.entry("codex".to_owned()).or_default();
        agent.model = Some("gpt-5.6-luna".to_owned());
        agent.effort = Some("low".to_owned());
        let codex: WorkerRef = "codex".parse().unwrap();
        let (worker, effort) = chat_agent(&settings, &codex);
        assert_eq!(worker.to_string(), "codex/gpt-5.6-luna");
        assert_eq!(effort.as_deref(), Some("low"));

        let mut role = settings
            .roles
            .get(&RoleName::Default)
            .cloned()
            .unwrap_or_default();
        role.effort = Some("high".to_owned());
        settings.roles.insert(RoleName::Default, role);
        let named: WorkerRef = "codex/gpt-5.6".parse().unwrap();
        let (worker, effort) = chat_agent(&settings, &named);
        assert_eq!(worker.to_string(), "codex/gpt-5.6");
        assert_eq!(effort.as_deref(), Some("high"));
    }

    #[test]
    fn the_note_counts_uncommitted_files() {
        let record = |n: usize| Record {
            commit: String::new(),
            uncommitted: (0..n).map(|i| format!("f{i}")).collect(),
        };
        assert_eq!(record(0).uncommitted_note(), None);
        assert_eq!(
            record(1).uncommitted_note().as_deref(),
            Some("1 uncommitted file is not included")
        );
        assert_eq!(
            record(2).uncommitted_note().as_deref(),
            Some("2 uncommitted files are not included")
        );
    }
}
