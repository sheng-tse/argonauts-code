//! One conversation turn's session: which worker reads it, what the request's record holds, and the
//! session the step runs in, on its own task: read-only, or for a direct turn, your checkout as
//! its CLI works in it.

use std::path::PathBuf;
use std::sync::Arc;

use argonauts_core::NoteLevel;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::session::{
    EndReason, InputPart, InputPartKind, loaded_words, stuck_words,
};
use argonauts_core::input::EFFORT_LEVELS;
use argonauts_core::limits::{HealthState, ModelState, PlanUsageStatus};
use argonauts_core::names::{ConversationId, ModelRef, RoleName, SessionKey, TurnId};
use argonauts_core::paths::create_private_dir_all;
use argonauts_core::pictures::Picture;
use argonauts_core::registry::listed::{nearest_listed, runs_in_place};
use argonauts_core::registry::{ModelRegistry, effort_to_run, known_efforts};
use argonauts_core::session_runner::{
    Access, Asking, CliWorkflow, RunnerError, RunnerEvent, RunnerKind, SandboxMode, SandboxProfile,
    SessionControl, SessionLimits, SessionPreferences, SessionRunner, SessionSpec,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::SandboxChoice;
use argonauts_core::settings::fallback::FallbackMode;
use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::steps::intake::{ConversationAnswer, Estimate};
use argonauts_core::steps::{
    StepError, StepName, StepObserver, StepOutcome, StepRequest, output_schema,
};
use argonauts_core::{DEFAULT_SOFT_RATIO, sha256_hex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::conversation::TurnUpdate;
use super::{Ctx, Internal};
use crate::controller::instructions::Command;
use crate::env::FolderGit;
use crate::git;
use crate::repo_files;

/// Where the request stood when it was sent.
pub(crate) struct Record {
    pub commit: String,
    /// Left out of a run's base; empty when `intake.include_uncommitted` is on.
    pub uncommitted: Vec<String>,
}

impl Record {
    /// A git failure, such as a repo with no commits yet, leaves that part empty and says so.
    pub(crate) fn now(ctx: &Ctx) -> Self {
        // A folder without git of its own has no commit to name, and a run there starts from
        // the folder as it is, so nothing is left out.
        if ctx.env.git() != FolderGit::Repository {
            return Self {
                commit: String::new(),
                uncommitted: Vec::new(),
            };
        }
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

/// The main agent, then its fallbacks in the order `[fallback.main]` gives them. With no list
/// anywhere it gets what the crew gets: the other models you have installed.
pub(crate) fn candidates(ctx: &Ctx) -> Vec<ModelRef> {
    in_order(ctx.settings(), || default_fallbacks(ctx))
}

/// How the main session's fallbacks are taken when `stopped` can't go on: `[fallback.main]`'s
/// own mode, or else the one every role follows.
pub(crate) fn fallback_mode(settings: &Settings, stopped: &ModelRef) -> FallbackMode {
    let applied = settings
        .fallback
        .applied_to(RoleName::Main, &stopped.agent_id());
    applied.mode
}

/// The main agent first, then the listed fallbacks or else `defaults`, each once. With
/// `same_cli` only those on the main agent's own CLI stay, and with the mode `wait` nobody
/// does: the main session never switches.
fn in_order(settings: &Settings, defaults: impl FnOnce() -> Vec<ModelRef>) -> Vec<ModelRef> {
    let default = settings.crew.main.clone();
    let applied = settings
        .fallback
        .applied_to(RoleName::Main, &default.agent_id());
    if applied.mode == FallbackMode::Wait {
        return vec![default];
    }
    let listed: Vec<ModelRef> = if applied.candidates.is_empty() {
        defaults()
    } else {
        let models = applied.candidates.iter().map(|entry| entry.model().clone());
        models.collect()
    };
    let mut workers = vec![default];
    for model in listed {
        let stays = !applied.same_cli || model.agent == workers[0].agent;
        if stays && !workers.contains(&model) {
            workers.push(model);
        }
    }
    workers
}

/// The fallbacks when no list names any, as a crew run's jobs get them: the crew's own members
/// in crew order, then whatever else is installed, CLIs only. Who is down, out of plan usage or
/// signed out is passed over when a turn picks, so this is only the order.
pub(crate) fn default_fallbacks(ctx: &Ctx) -> Vec<ModelRef> {
    let workers = &ctx.services.workers;
    let available = workers.available();
    let mut pool: Vec<ModelRef> = ctx
        .settings()
        .crew
        .members()
        .into_iter()
        .filter(|model| available.contains(model))
        .collect();
    pool.extend(available);
    pool.retain(|model| {
        workers
            .get(model)
            .is_ok_and(|agent| agent.kind() == RunnerKind::Cli)
    });
    pool
}

/// The first candidate not tried yet whose agent is neither out of plan usage nor down. When
/// every one is, the first that exists anyway, so its own error says why.
pub(crate) fn pick(
    ctx: &Ctx,
    tried: &[ModelRef],
) -> Result<(ModelRef, Arc<dyn SessionRunner>), RunnerError> {
    let mut first_error = None;
    let mut out_of_service = None;
    for worker in candidates(ctx) {
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
        if agent_out(ctx, &worker) {
            out_of_service.get_or_insert((worker, handle));
            continue;
        }
        return Ok((worker, handle));
    }
    out_of_service.ok_or_else(|| {
        first_error.unwrap_or_else(|| RunnerError::Unknown(ctx.settings().crew.main.clone()))
    })
}

/// The main agent itself, for a turn no fallback can take.
pub(crate) fn pick_main(ctx: &Ctx) -> Result<(ModelRef, Arc<dyn SessionRunner>), RunnerError> {
    let main = ctx.settings().crew.main.clone();
    let handle = ctx.services.workers.get(&main)?;
    Ok((main, handle))
}

pub(crate) fn agent_out(ctx: &Ctx, worker: &ModelRef) -> bool {
    let agent_id = worker.agent_id();
    let health = &ctx.services.health;
    let down = health.state(&agent_id).state == HealthState::Down
        || health.model(worker).state == ModelState::Out;
    let out = ctx
        .services
        .plan_usage
        .get(&agent_id)
        .is_some_and(|reading| reading.status == PlanUsageStatus::Rejected);
    down || out
}

/// Failures another agent might not have, so the next fallback gets the turn.
pub(crate) fn worth_a_fallback(err: &StepError) -> bool {
    match err {
        StepError::Runner(
            RunnerError::NotInstalled(_) | RunnerError::Disabled { .. } | RunnerError::Unknown(_),
        ) => true,
        StepError::Session(end) => matches!(
            end.reason,
            EndReason::UsageLimit { .. }
                | EndReason::RateLimited
                | EndReason::CliError { .. }
                | EndReason::Auth { .. }
                | EndReason::Billing { .. }
                | EndReason::ModelUnavailable { .. }
                | EndReason::StartFailed { .. }
        ),
        // A refusal or a local failure would repeat with any worker.
        StepError::Runner(RunnerError::Refused(_) | RunnerError::Io(_))
        | StepError::SchemaMismatch { .. } => false,
    }
}

/// One line for the transcript.
pub(crate) fn describe(err: &StepError) -> String {
    match err {
        StepError::Runner(err) => err.to_string(),
        StepError::Session(end) => match &end.reason {
            EndReason::UsageLimit { .. } => "it hit its usage limit".to_owned(),
            EndReason::RateLimited => "it is rate limited".to_owned(),
            EndReason::CliError { message, .. }
            | EndReason::Auth { message }
            | EndReason::Billing { message }
            | EndReason::StartFailed { message }
            | EndReason::Crashed { message } => message.clone(),
            EndReason::ModelUnavailable { model, .. } => format!("{model} isn't available"),
            EndReason::AdapterBroken { detail } => detail.clone(),
            EndReason::IsolationBroken { plugins, skills } => {
                format!("it loaded {}", loaded_words(plugins, skills))
            }
            EndReason::ContextFull => "its context window is full".to_owned(),
            EndReason::Stopped | EndReason::Paused => "it was stopped".to_owned(),
            EndReason::Stuck { watchdog, calls } => {
                format!("it {}", stuck_words(*watchdog, calls.as_ref()))
            }
            other => format!("it ended early ({other:?})"),
        },
        StepError::SchemaMismatch { raw: None, .. } => "it ended without an answer".to_owned(),
        StepError::SchemaMismatch { errors, .. } => {
            format!("its answer didn't match the schema: {}", errors.join("; "))
        }
    }
}

/// The line under a turn a watchdog stopped, naming the model as the turn ran it: `Stopped:
/// Qwen3 Coder 30B repeated the same execute call 12 times.` `None` for any other end.
pub(crate) fn stopped_by_watchdog(
    names: &DisplayNames,
    worker: &ModelRef,
    err: &StepError,
) -> Option<String> {
    let StepError::Session(end) = err else {
        return None;
    };
    let EndReason::Stuck { watchdog, calls } = &end.reason else {
        return None;
    };
    let running = names.running(worker);
    let who = names
        .model(&running.agent, running.model.as_deref())
        .unwrap_or_else(|| names.cli(&running.agent));
    Some(format!(
        "Stopped: {who} {}.",
        stuck_words(*watchdog, calls.as_ref())
    ))
}

/// Whether `err` says `agent`'s CLI is signed out, as [`argonauts_core::sign_in`] reads a
/// sign-in error's words.
pub(crate) fn signed_out(err: &StepError, agent: &str) -> Option<argonauts_core::app::SignedOut> {
    let StepError::Session(end) = err else {
        return None;
    };
    argonauts_core::sign_in::signed_out(&end.reason, agent)
}

/// How a signed-out CLI's turn reads when nobody carries it on, such as `Claude Code is signed
/// out (its sign-in was revoked) · run claude /login`.
pub(crate) fn signed_out_words(cli: &str, out: &argonauts_core::app::SignedOut) -> String {
    let mut words = format!("{cli} is signed out");
    if !out.why.is_empty() {
        words.push_str(&format!(" ({})", out.why));
    }
    if let Some(login) = &out.login {
        words.push_str(&format!(" · run {login}"));
    }
    words
}

/// What the turn asks.
/// What your checked words change about a direct turn: the main agent's model and effort, a
/// lower limit for the message, no network, and asking you first. A direct turn has no roles,
/// so words that give one make the Captain checked instead.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DirectWords {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub limit_usd: Option<f64>,
    pub no_network: bool,
    pub supervised: bool,
    /// What the CLI's own workflow does with this message, such as the `ultracode` you typed.
    pub cli_workflow: CliWorkflow,
    /// "Keep going until its tests pass": the goal your words set for this request.
    pub goal: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TurnStep {
    /// A new request: answer, change, or unclear.
    Estimate {
        uncommitted_note: Option<String>,
        /// `captain_or_crew.read_instructions`: the estimate lists how the request says to run it.
        read_instructions: bool,
        /// `/captain` or `/crew` with a task: the estimate reads your words, and the command's
        /// mode stands whatever they or the rules say.
        command: Option<Command>,
    },
    /// `/ask`: an answer only.
    Ask { run_status: Option<String> },
    /// One of the main agent CLI's own commands. `prompt` is the whole input, so the CLI
    /// reads it as the command, and its reply is plain text with no schema.
    Cli {
        prompt: String,
        /// In a direct conversation the command works in your checkout, so Claude Code's
        /// `/init` writes `CLAUDE.md` as it does in Claude Code. The turn's listener, as for
        /// [`TurnStep::Direct`].
        direct: Option<Option<Asking>>,
    },
    /// `/btw`: an answer from a session of its own, kept out of the conversation.
    Side,
    /// `/rename` with no name: the main agent names the conversation, out of its history too.
    Title,
    /// Direct Captain: your message to the main session, which works in your checkout as its CLI
    /// does and ends with its own answer. `asking` is the turn's listener for what it asks you,
    /// and for the main session's tools; `None` asks nothing. `go_ahead` is what the session reads
    /// in place of your message when it has it already, as after its estimate or its plan.
    Direct {
        asking: Option<Asking>,
        go_ahead: Option<String>,
        words: DirectWords,
    },
    /// Plan first in direct Captain: your message to the main session, which reads and plans
    /// without changing anything and ends with its plan. "Go ahead with this plan?" then resumes
    /// it as a [`TurnStep::Direct`], as Claude Code's plan mode does.
    Plan { words: DirectWords },
    /// `/compact`: a summary the next session starts from.
    Compact { instructions: Option<String> },
    /// The summary a main model on another CLI starts from. With no `record`, `writer` is the
    /// model being left, which writes it in its own session `resume` with `/compact`'s prompt;
    /// with one, `writer` is the new model, which writes it from that record of the
    /// conversation.
    Carry {
        writer: ModelRef,
        resume: Option<String>,
        record: Option<String>,
    },
    /// `/review` and `/security-review`, by whoever [`crate::controller::conversation`] picked.
    Review {
        security: bool,
        /// Such as "the uncommitted changes".
        target: String,
        changes: String,
        other_provider: bool,
    },
    /// Argonauts Code tells the main session, in the turn's text, what became of a run it started,
    /// and the session says so to you in one line. Read-only, and it starts nothing.
    Told,
}

impl TurnStep {
    fn name(&self) -> StepName {
        match self {
            Self::Estimate { .. } => StepName::Estimate,
            Self::Direct { .. } => StepName::Direct,
            // Read-only, with the tools an answer gets.
            Self::Plan { .. } => StepName::ConversationAnswer,
            Self::Cli { .. } => StepName::CliCommand,
            Self::Ask { .. }
            | Self::Side
            | Self::Title
            | Self::Compact { .. }
            | Self::Carry { .. }
            | Self::Review { .. }
            | Self::Told => StepName::ConversationAnswer,
        }
    }

    /// Steps whose text is your own words, so the files they mention go along.
    pub(crate) fn takes_mentions(&self) -> bool {
        matches!(
            self,
            Self::Estimate { .. }
                | Self::Ask { .. }
                | Self::Side
                | Self::Direct { .. }
                | Self::Plan { .. }
        ) && !self.continues()
    }

    /// A turn that goes on from the one before with the same session, which already has your
    /// message, the files it mentions, and the `!` commands you ran. Being told of a run is one
    /// too: it has no message of yours to show.
    pub(crate) fn continues(&self) -> bool {
        matches!(
            self,
            Self::Direct {
                go_ahead: Some(_),
                ..
            } | Self::Told
        )
    }

    /// A turn that may change your files.
    pub(crate) fn writes(&self) -> bool {
        self.direct_asking().is_some()
    }

    /// Direct Captain's turns, a plan first included, whose own answer is the end.
    pub(crate) fn in_your_checkout(&self) -> bool {
        self.writes() || matches!(self, Self::Plan { .. })
    }

    /// What your words changed about a direct turn.
    fn direct_words(&self) -> Option<&DirectWords> {
        match self {
            Self::Direct { words, .. } | Self::Plan { words } => Some(words),
            _ => None,
        }
    }

    /// A direct turn's listener, or `None` for a turn that only reads.
    fn direct_asking(&self) -> Option<&Option<Asking>> {
        match self {
            Self::Direct { asking, .. } => Some(asking),
            Self::Cli { direct, .. } => direct.as_ref(),
            _ => None,
        }
    }

    /// The turn's session was offered some of the main session's tools.
    pub(crate) fn has_tools(&self) -> bool {
        matches!(self.direct_asking(), Some(Some(asking)) if !asking.tools.is_empty())
    }

    /// The turn's session was offered the tool `name`, so a call to it is its own.
    pub(crate) fn offers_tool(&self, name: &str) -> bool {
        matches!(self.direct_asking(), Some(Some(asking))
            if asking.tools.iter().any(|tool| tool == name))
    }

    /// Steps whose reply is the CLI's own text, with no schema to read.
    pub(crate) fn plain_reply(&self) -> bool {
        matches!(
            self,
            Self::Cli { .. } | Self::Direct { .. } | Self::Plan { .. }
        )
    }

    /// Only the main agent knows its own commands, so no fallback takes one.
    pub(crate) fn main_agent_only(&self) -> bool {
        matches!(self, Self::Cli { .. })
    }

    /// A message of yours to the main agent, so it hears of the `!` commands you ran before it. A
    /// CLI command must be the whole prompt, and a side question, a title, a summary, and a
    /// review stand apart from the conversation.
    pub(crate) fn hears_shell(&self) -> bool {
        matches!(
            self,
            Self::Estimate { .. } | Self::Ask { .. } | Self::Direct { .. } | Self::Plan { .. }
        ) && !self.continues()
    }

    /// Answered in a session of its own and left out of the journal and the history, so the
    /// conversation reads as if it never happened.
    pub(crate) fn kept_out(&self) -> bool {
        matches!(self, Self::Side | Self::Title | Self::Carry { .. })
    }

    /// Never resumes the conversation's CLI session, and never becomes it: a side question
    /// would land in its history, and a reviewer may be another agent's.
    pub(crate) fn own_session(&self) -> bool {
        self.kept_out() || matches!(self, Self::Review { .. })
    }

    fn prompt(&self, text: &str) -> InputPart {
        let (label, prompt) = match self {
            Self::Estimate {
                uncommitted_note,
                read_instructions,
                ..
            } => (
                "estimate",
                crate::prompts::estimate(
                    &argonauts_core::steps::intake::IntakeInput {
                        request: text.to_owned(),
                        uncommitted_note: uncommitted_note.clone(),
                    },
                    *read_instructions,
                ),
            ),
            Self::Ask { run_status } => (
                "conversation answer",
                crate::prompts::conversation_answer(
                    &argonauts_core::steps::intake::ConversationAnswerInput {
                        question: text.to_owned(),
                        run_status: run_status.clone(),
                    },
                ),
            ),
            Self::Cli { prompt, .. } => ("agent command", prompt.clone()),
            Self::Direct {
                go_ahead: Some(words),
                ..
            } => return part(InputPartKind::Message, "go ahead", words.clone()),
            // Your words as you typed them, with nothing of Argonauts Code's own.
            Self::Direct { .. } => {
                return part(InputPartKind::Message, "your message", text.to_owned());
            }
            // Your words, and that plan first is on, as Claude Code's plan mode tells its model.
            Self::Plan { .. } => ("plan first", format!("{}\n\n{PLAN_FIRST}", text.trim())),
            Self::Told => (
                "notice",
                crate::prompts::conversation_answer(
                    &argonauts_core::steps::intake::ConversationAnswerInput {
                        question: text.to_owned(),
                        run_status: None,
                    },
                ),
            ),
            Self::Side => (
                "side question",
                crate::prompts::conversation_answer(
                    &argonauts_core::steps::intake::ConversationAnswerInput {
                        question: text.to_owned(),
                        run_status: None,
                    },
                ),
            ),
            Self::Title => ("name", crate::prompts::title()),
            Self::Compact { instructions } => {
                ("compact", crate::prompts::compact(instructions.as_deref()))
            }
            Self::Carry { record: None, .. } => ("summary", crate::prompts::compact(None)),
            Self::Carry {
                record: Some(record),
                ..
            } => ("summary", crate::prompts::carry_summary(record)),
            Self::Review {
                security,
                target,
                changes,
                other_provider,
            } => (
                "review",
                crate::prompts::review(&crate::prompts::ReviewInput {
                    security: *security,
                    target,
                    changes,
                    other_provider: *other_provider,
                }),
            ),
        };
        part(InputPartKind::Prompt, label, prompt)
    }

    fn schema(&self) -> Option<serde_json::Value> {
        match self {
            Self::Estimate { .. } => Some(output_schema::<Estimate>()),
            Self::Cli { .. } | Self::Direct { .. } | Self::Plan { .. } => None,
            _ => Some(output_schema::<ConversationAnswer>()),
        }
    }
}

/// What a plan-first turn reads after your message.
const PLAN_FIRST: &str = "Plan first is on: read what you need and change nothing yet. End with \
your plan. You'll be told when to go ahead.";

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
    pub conversation: &'a ConversationId,
    pub turn: &'a TurnId,
    pub step: &'a TurnStep,
    pub text: &'a str,
    /// The pictures your message holds, as its `[Image #N]` chips name them.
    pub pictures: Vec<Picture>,
    pub worker: ModelRef,
    pub handle: Arc<dyn SessionRunner>,
    /// The CLI session to resume, when it belongs to this worker.
    pub resume: Option<String>,
    /// Resume it as a copy, which leaves the session itself as it was.
    pub fork: bool,
    /// For a fresh session: the conversation so far.
    pub excerpt: Option<InputPart>,
    /// After the step's prompt, such as how to write the answer.
    pub notes: Vec<InputPart>,
    /// The `!` commands you ran since the main agent's last turn, ahead of your message.
    pub shell: Option<InputPart>,
    /// `/fast`: the lowest effort the model takes, in place of the one set.
    pub fast: bool,
    /// Your checkout, or the folder of an agent's session the conversation carries on.
    pub workdir: PathBuf,
}

/// A turn's session once it started: its key, the token that stops it, and the way in for
/// what you send it while it works.
pub(crate) struct Launched {
    pub key: SessionKey,
    pub cancel: CancellationToken,
    pub control: mpsc::Sender<SessionControl>,
}

/// Starts the step on its own task. Its events and its end come back as `Internal::Conversation`.
pub(crate) fn launch(ctx: &Ctx, launch: Launch<'_>) -> anyhow::Result<Launched> {
    let key_text = format!("{}-{}", launch.conversation, launch.turn);
    let key = SessionKey::parse(&key_text)
        .ok_or_else(|| anyhow::anyhow!("{key_text:?} can't name a session"))?;
    let session_dir = ctx
        .env
        .conversation_dir()
        .join(launch.conversation.as_str())
        .join(launch.turn.as_str());
    create_private_dir_all(&session_dir)
        .map_err(|err| anyhow::anyhow!("creating {}: {err}", session_dir.display()))?;
    say_in_place(ctx, &launch);
    let spec = session_spec(ctx, &launch, key.clone(), session_dir);
    if spec.cli_workflow.ultracode || spec.cli_workflow.typed_keyword {
        crate::telemetry::used(&ctx.env.paths, crate::telemetry::Use::Ultracode);
    }
    let asked_again = ctx.internal.clone();
    let retried = launch.turn.clone();
    let worker = super::tries::Tries::wrap(launch.handle, move || {
        let update = TurnUpdate::Retried {
            turn: retried.clone(),
        };
        let _ = asked_again.send(Internal::Conversation(update));
    });
    let mut request = StepRequest::new(worker, spec);
    let cancel = CancellationToken::new();
    request.cancel = cancel.clone();
    let (control, steering) = mpsc::channel(16);
    request.control = Some(steering);

    let steps = Arc::clone(&ctx.services.steps);
    let events = ctx.internal.clone();
    let done = ctx.internal.clone();
    let turn = launch.turn.clone();
    let seen = turn.clone();
    let observer: StepObserver = Box::new(move |event: &RunnerEvent| {
        let update = TurnUpdate::Event {
            turn: seen.clone(),
            event: event.clone(),
        };
        let _ = events.send(Internal::Conversation(update));
    });
    tokio::spawn(async move {
        let outcome = if request.spec.json_schema.is_none() {
            run_plain(request, observer).await
        } else {
            steps.run(request, observer).await
        };
        // After Exit nobody listens, and the outcome has nowhere to go.
        let _ = done.send(Internal::Conversation(TurnUpdate::Done { turn, outcome }));
    });
    Ok(Launched {
        key,
        cancel,
        control,
    })
}

/// A session whose reply is the CLI's own text, such as a CLI command's, so there is no
/// schema to check or retry against. A session that didn't complete is the error.
async fn run_plain(
    request: StepRequest,
    mut observer: StepObserver,
) -> Result<StepOutcome, StepError> {
    let mut handle = request.worker.start(request.spec).await?;
    let mut steering = request.control;
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
            command = next_command(&mut steering), if !stop_sent => match command {
                Some(command) => {
                    let _ = handle.control.send(command).await;
                }
                None => steering = None,
            },
        }
    }
    let end = handle
        .ended
        .await
        .map_err(|_| RunnerError::Io("the session ended without saying how".to_owned()))?;
    if end.reason != EndReason::Completed {
        return Err(StepError::Session(Box::new(end)));
    }
    Ok(StepOutcome {
        value: serde_json::Value::Null,
        end,
        tries: 1,
    })
}

/// What you send the running session, until nobody can send more.
async fn next_command(
    steering: &mut Option<mpsc::Receiver<SessionControl>>,
) -> Option<SessionControl> {
    match steering {
        Some(steering) => steering.recv().await,
        None => std::future::pending().await,
    }
}

/// The worker and effort a conversation turn runs with: the model and effort you set for the agent
/// when `worker` names none, and the main role's effort over the agent's.
pub(crate) fn conversation_agent(
    settings: &Settings,
    worker: &ModelRef,
) -> (ModelRef, Option<String>) {
    let role = settings.roles.get(&RoleName::Main);
    let agent = settings.agents.get(&worker.agent);
    let mut worker = worker.clone();
    if worker.model.is_none() {
        worker.model = agent.and_then(|agent| agent.model.clone());
    }
    let effort = role
        .and_then(|role| role.effort.clone())
        .or_else(|| agent.and_then(|agent| agent.effort.clone()));
    (worker, effort)
}

/// The worker and effort a turn names: [`conversation_agent`]'s, with the model and effort
/// your words asked for over them.
fn named_worker(settings: &Settings, launch: &Launch<'_>) -> (ModelRef, Option<String>) {
    let (mut worker, mut effort) = conversation_agent(settings, &launch.worker);
    if let Some(words) = launch.step.direct_words() {
        if let Some(model) = &words.model {
            worker.model = Some(model.clone());
        }
        if let Some(asked) = &words.effort {
            effort = Some(asked.clone());
        }
    }
    (worker, effort)
}

/// Says before a turn with your words starts that its CLI no longer lists the model it names,
/// and which one runs in its place. The screen shows a warning once, so later turns can say it
/// again.
pub(crate) fn say_in_place(ctx: &Ctx, launch: &Launch<'_>) {
    if !launch.step.takes_mentions() {
        return;
    }
    let registry = ctx.services.registry.as_ref();
    let (named, _) = named_worker(ctx.settings(), launch);
    if let Some(nearest) = nearest_listed(registry, &named) {
        let line = runs_in_place(registry, "This conversation", &named, &nearest);
        ctx.notice(NoteLevel::Warn, line);
    }
}

/// What `/fast` runs `worker` as: the lowest level its model takes, `low` when they aren't
/// known, or for a model fixed at one level, the listed model of its line at the lowest, as
/// agy lists `gemini-3.8-flash-low` beside `gemini-3.8-flash-high`. `None` when nothing is
/// faster: a model that takes no effort setting, or one fixed at its line's lowest.
pub(crate) fn fast_choice(ctx: &Ctx, worker: &ModelRef) -> Option<(ModelRef, String)> {
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
    worker: &ModelRef,
    level: &str,
) -> Option<(ModelRef, String)> {
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
        .filter(|entry| entry.id.agent == worker.agent)
        .filter_map(|entry| match entry.efforts.as_slice() {
            [only] => {
                let named = stem(entry.id.model.as_deref()?, only)?;
                (named == line).then(|| (entry.id.clone(), only.clone()))
            }
            _ => None,
        })
        .min_by_key(|(_, level)| EFFORT_LEVELS.iter().position(|known| known == level))
}

/// A conversation turn reads your checkout. With `sandbox.mode` set to `tool` or `none` it runs
/// outside ours only when the CLI keeps it read-only itself, since macOS refuses a CLI's own
/// sandbox inside ours and its shell commands would fail.
fn conversation_sandbox(choice: SandboxChoice, keeps_read_only: bool) -> SandboxMode {
    match choice {
        SandboxChoice::Container => SandboxMode::Container,
        SandboxChoice::Tool | SandboxChoice::None if keeps_read_only => SandboxMode::None,
        SandboxChoice::Os | SandboxChoice::Tool | SandboxChoice::None => SandboxMode::Os,
    }
}

/// A picture as a part of the prompt: the file, which the CLI reads as an image, and no text.
fn picture_part(picture: &Picture) -> InputPart {
    InputPart {
        kind: InputPartKind::Picture,
        label: picture.label(),
        text: String::new(),
        approx_tokens: 0,
        source: Some(picture.path.display().to_string()),
    }
}

pub(crate) fn session_spec(
    ctx: &Ctx,
    launch: &Launch<'_>,
    key: SessionKey,
    session_dir: PathBuf,
) -> SessionSpec {
    let settings = ctx.settings();
    let role = settings.roles.get(&RoleName::Main);
    let (mut worker, effort) = named_worker(settings, launch);
    let words = launch.step.direct_words();
    // The model that runs sets the level it takes, so the swap comes first.
    if let Some(nearest) = nearest_listed(ctx.services.registry.as_ref(), &worker) {
        worker = nearest;
    }
    let mut effort = effort_to_run(ctx.services.registry.as_ref(), &worker, effort);
    if launch.fast
        && let Some((fast, level)) = fast_choice(ctx, &worker)
    {
        worker = fast;
        effort = Some(level);
    }
    // An endpoint has no tools, so it gets an empty folder and no sandbox of its own.
    let endpoint = launch.handle.kind() == RunnerKind::Endpoint;
    let mut prompt: Vec<InputPart> = launch.excerpt.iter().cloned().collect();
    prompt.extend(launch.shell.iter().cloned());
    prompt.push(launch.step.prompt(launch.text));
    prompt.extend(launch.pictures.iter().map(picture_part));
    if launch.step.takes_mentions() {
        let root = &launch.workdir;
        let mentioned = repo_files::mentioned(root, launch.text);
        prompt.extend(repo_files::parts(root, &mentioned, !endpoint));
    }
    prompt.extend(launch.notes.iter().cloned());
    let (access, workdir, mode) = if endpoint {
        (Access::NoTools, session_dir.clone(), SandboxMode::None)
    } else if launch.step.writes() {
        let mode = direct_sandbox(settings.sandbox.mode);
        (Access::Direct, launch.workdir.clone(), mode)
    } else {
        let keeps_read_only = ctx.services.workers.enforces_read_only(&worker);
        let mode = conversation_sandbox(settings.sandbox.mode, keeps_read_only);
        (Access::ReadOnly, launch.workdir.clone(), mode)
    };
    let direct = access == Access::Direct;
    let asking = launch
        .step
        .direct_asking()
        .filter(|_| direct)
        .cloned()
        .flatten();
    let state_root = ctx
        .env
        .state_dir
        .parent()
        .map_or_else(|| ctx.env.state_dir.clone(), |dir| dir.to_owned());
    SessionSpec {
        run: None,
        task: None,
        key,
        role: RoleName::Main,
        step: Some(launch.step.name()),
        model: worker,
        effort,
        access,
        workdir,
        session_dir,
        prompt,
        json_schema: launch.step.schema(),
        resume: launch.resume.clone(),
        fork: launch.fork,
        lifetime: SessionLifetime::Run,
        keep_input_open: false,
        limits: if direct {
            message_limits(settings, words)
        } else {
            watched(settings)
        },
        // Sending your message is the approval, as `captain.understand_first = false` already
        // treats your prompt.
        goal_sha256: direct.then(|| sha256_hex(launch.text.as_bytes())),
        sandbox: SandboxProfile {
            mode,
            writable: Vec::new(),
            readable: Vec::new(),
            deny_read: vec![state_root],
            network: settings.sandbox.network && !words.is_some_and(|words| words.no_network),
            proxy_port: None,
            ask_port: None,
            closed: Vec::new(),
            listable: Vec::new(),
            loopback: false,
        },
        compaction: role
            .and_then(|role| role.compaction)
            .unwrap_or(settings.compaction),
        native_fanout: role
            .and_then(|role| role.native_fanout)
            .unwrap_or(settings.native_fanout),
        isolated: false,
        // You are here to answer, and only talking it over starts nothing.
        autopilot: false,
        status_file: None,
        env: Vec::new(),
        asking,
        cli_workflow: cli_workflow(settings, direct, words),
        shell: argonauts_core::session_runner::SessionShell::None,
        preferences: preferences(settings, launch.fast),
    }
}

/// What you set for the session you talk to that its CLI takes as settings of its own.
fn preferences(settings: &Settings, fast: bool) -> SessionPreferences {
    let main = &settings.main_session;
    let language = main.language.trim();
    SessionPreferences {
        thinking_off: !main.thinking,
        fast,
        language: (!language.is_empty()).then(|| language.to_owned()),
        workflow_word_off: !settings.workflows.ultracode_trigger,
    }
}

/// A direct turn's CLI runs its own workflow as ultracode and your words say: ultracode on in
/// Captain, or the `ultracode` you typed with this message.
fn cli_workflow(settings: &Settings, direct: bool, words: Option<&DirectWords>) -> CliWorkflow {
    if !direct {
        return CliWorkflow::default();
    }
    let asked = words.map(|words| words.cli_workflow).unwrap_or_default();
    CliWorkflow {
        ultracode: asked.ultracode || super::ultra::main_session(settings).ultracode,
        typed_keyword: asked.typed_keyword,
        ..CliWorkflow::default()
    }
}

/// A direct turn is your checkout as the CLI works in it. Ours wraps it unless you turned it
/// off, and then the CLI's own sandbox and approvals hold, exactly as when you start it.
fn direct_sandbox(choice: SandboxChoice) -> SandboxMode {
    match choice {
        SandboxChoice::Os => SandboxMode::Os,
        SandboxChoice::Container => SandboxMode::Container,
        SandboxChoice::Tool | SandboxChoice::None => SandboxMode::None,
    }
}

/// Every turn, talk included, has the watchdogs a run's sessions have for a session that
/// loops or hangs: a nudge at `repeat_limit` identical calls, and the stop two calls on or
/// after `inactivity_min` quiet minutes. They are no limit on the work, so they stay on.
fn watched(settings: &Settings) -> SessionLimits {
    let watchdogs = &settings.watchdogs;
    SessionLimits {
        inactivity_min: Some(watchdogs.inactivity_min).filter(|minutes| *minutes > 0.0),
        repeat_limit: Some(watchdogs.repeat_limit).filter(|limit| *limit > 0),
        ..SessionLimits::default()
    }
}

/// Each message is held to the run's limit when you set one, as a run is: the CLI's own cap
/// sits just above it, and the meter acts for the rest. With none, only the watchdogs stop it.
fn message_limits(settings: &Settings, words: Option<&DirectWords>) -> SessionLimits {
    let limit = &settings.limits.spend;
    // Your words only ever lower it, so theirs stands in for the setting's.
    let asked = words.and_then(|words| words.limit_usd);
    let amount = asked
        .or(limit.spend_per_run)
        .map(|amount| LimitAmount {
            unit: LimitUnit::Usd,
            amount,
        })
        .or_else(|| {
            limit.run_tokens.map(|tokens| LimitAmount {
                unit: LimitUnit::Tokens,
                amount: tokens as f64,
            })
        });
    SessionLimits {
        limit: amount,
        soft_stop: if limit.soft_stop > 0.0 && limit.soft_stop < 1.0 {
            limit.soft_stop
        } else {
            DEFAULT_SOFT_RATIO
        },
        max_minutes: limit.run_minutes,
        ..watched(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::app::{AppEvent, ConversationEvent};
    use argonauts_core::registry::ModelRegistry;
    use argonauts_core::settings::crew::CrewRole;
    use argonauts_core::settings::fallback::FallbackEntry;
    use argonauts_testkit::TempRepo;
    use argonauts_testkit::runner::session_end;

    use crate::controller::conversation::{self, ConversationState};
    use crate::controller::test_support::{CliOptions, TestCtx, WAIT};
    use crate::host::clis::RepoModels;
    use crate::host::services::{LiveRegistry, SettingsSource, directory};

    #[test]
    fn the_main_agent_comes_first_then_its_fallbacks_once_each() {
        let mut settings = Settings::default();
        let role = settings.fallback.roles.entry(CrewRole::Main).or_default();
        role.order = vec![
            FallbackEntry::Model("codex".parse().unwrap()),
            FallbackEntry::Model("claude-code".parse().unwrap()),
            FallbackEntry::Model("antigravity-cli".parse().unwrap()),
        ];
        let names = |settings: &Settings, defaults: &[&str]| -> Vec<String> {
            let defaults = || {
                defaults
                    .iter()
                    .map(|model| model.parse().unwrap())
                    .collect()
            };
            let order = in_order(settings, defaults);
            order.iter().map(ToString::to_string).collect()
        };
        assert_eq!(
            names(&settings, &["opencode"]),
            ["claude-code", "codex", "antigravity-cli"],
            "a list you set is the whole order"
        );

        // With no list anywhere, the main session gets what the crew gets.
        let unlisted = Settings::default();
        assert_eq!(
            names(&unlisted, &["claude-code/sonnet", "claude-code", "codex"]),
            ["claude-code", "claude-code/sonnet", "codex"]
        );

        // Staying on its CLI passes over every other one, listed or not.
        let mut same_cli = Settings::default();
        same_cli.fallback.same_cli = Some(true);
        assert_eq!(
            names(&same_cli, &["claude-code/sonnet", "codex"]),
            ["claude-code", "claude-code/sonnet"]
        );
        settings.fallback.same_cli = Some(true);
        assert_eq!(names(&settings, &[]), ["claude-code"]);

        // Told to wait, the main session has nobody behind it, listed or not; the mode set
        // for it alone wins over the one every role follows.
        let mut waits = Settings::default();
        waits.fallback.mode = FallbackMode::Wait;
        assert_eq!(names(&waits, &["codex"]), ["claude-code"]);
        let main = waits.fallback.roles.entry(CrewRole::Main).or_default();
        main.mode = FallbackMode::Auto;
        assert_eq!(names(&waits, &["codex"]), ["claude-code", "codex"]);
        let claude: ModelRef = "claude-code".parse().unwrap();
        assert_eq!(fallback_mode(&waits, &claude), FallbackMode::Auto);
    }

    #[test]
    fn every_turn_has_the_watchdogs_and_a_stop_by_one_says_what_it_saw() {
        use argonauts_core::events::session::{CallCount, Watchdog};

        let settings = Settings::default();
        for limits in [watched(&settings), message_limits(&settings, None)] {
            assert_eq!(limits.repeat_limit, Some(10));
            assert_eq!(limits.inactivity_min, Some(15.0));
            assert_eq!(limits.limit, None, "no spend limit until you set one");
            assert_eq!(limits.max_minutes, None);
            assert_eq!(limits.no_progress_calls, None, "a turn is no later try");
        }
        let mut off = Settings::default();
        off.watchdogs.repeat_limit = 0;
        off.watchdogs.inactivity_min = 0.0;
        let limits = message_limits(&off, None);
        assert_eq!((limits.repeat_limit, limits.inactivity_min), (None, None));

        let ended = |reason| StepError::Session(Box::new(session_end(reason, None, None)));
        let repeated = ended(EndReason::Stuck {
            watchdog: Watchdog::Repeat,
            calls: Some(CallCount {
                name: "execute".into(),
                times: 12,
            }),
        });
        assert!(!worth_a_fallback(&repeated));
        let names = DisplayNames::default();
        let qwen: ModelRef = "opencode/ollama/qwen3-coder:30b-64k".parse().unwrap();
        let name = names
            .model("opencode", qwen.model.as_deref())
            .expect("a model name");
        assert_eq!(
            stopped_by_watchdog(&names, &qwen, &repeated),
            Some(format!(
                "Stopped: {name} repeated the same execute call 12 times."
            ))
        );
        assert_eq!(
            describe(&repeated),
            "it repeated the same execute call 12 times"
        );
        let quiet = ended(EndReason::Stuck {
            watchdog: Watchdog::Inactivity,
            calls: None,
        });
        let codex: ModelRef = "codex".parse().unwrap();
        assert_eq!(
            stopped_by_watchdog(&names, &codex, &quiet).as_deref(),
            Some("Stopped: Codex went quiet, with no output or activity.")
        );
        assert_eq!(
            stopped_by_watchdog(&names, &codex, &ended(EndReason::RateLimited)),
            None
        );
    }

    #[test]
    fn agent_trouble_is_worth_a_fallback_but_a_stop_is_not() {
        let ended = |reason| StepError::Session(Box::new(session_end(reason, None, None)));
        assert!(worth_a_fallback(&ended(EndReason::RateLimited)));
        assert!(worth_a_fallback(&ended(EndReason::UsageLimit {
            resets_at: None
        })));
        assert!(!worth_a_fallback(&ended(EndReason::Stopped)));
        assert!(worth_a_fallback(&StepError::Runner(
            RunnerError::NotInstalled("codex".to_owned())
        )));
        assert_eq!(
            describe(&ended(EndReason::RateLimited)),
            "it is rate limited"
        );
    }

    /// A sign-in error that says the sign-in is gone means signed out, whatever the CLI's own
    /// check says. A bare 401, or "expired" in an error that isn't about signing in, doesn't.
    #[test]
    fn a_sign_in_error_that_says_revoked_expired_or_log_in_again_is_signed_out() {
        let ended = |reason| StepError::Session(Box::new(session_end(reason, None, None)));
        let auth = |message: &str| {
            ended(EndReason::Auth {
                message: message.to_owned(),
            })
        };
        let revoked = signed_out(
            &auth(
                "Failed to authenticate: OAuth token revoked. Please log in again or contact \
                 your administrator.",
            ),
            "claude-code",
        )
        .expect("signed out");
        assert_eq!(revoked.why, "its sign-in was revoked");
        assert_eq!(revoked.login.as_deref(), Some("claude /login"));
        assert_eq!(
            signed_out_words("Claude Code", &revoked),
            "Claude Code is signed out (its sign-in was revoked) · run claude /login"
        );
        let expired = signed_out(&auth("Login expired · Please run /login"), "codex");
        assert_eq!(
            expired.map(|out| (out.why, out.login)),
            Some((
                "its sign-in expired".to_owned(),
                Some("codex login".to_owned())
            ))
        );
        let asked = signed_out(&auth("Not logged in · Please run /login"), "claude-code");
        assert_eq!(asked.map(|out| out.why), Some(String::new()));
        let cli_error = ended(EndReason::CliError {
            category: argonauts_core::ErrorCategory::Auth,
            message: "Your session has been revoked".to_owned(),
        });
        assert!(signed_out(&cli_error, "opencode").is_some());
        assert_eq!(signed_out(&auth("401 Unauthorized"), "claude-code"), None);
        let network = ended(EndReason::CliError {
            category: argonauts_core::ErrorCategory::Network,
            message: "token expired while offline".to_owned(),
        });
        assert_eq!(signed_out(&network, "claude-code"), None);
    }

    /// Codex is the main agent and off in the file. Its binary is a path that doesn't exist,
    /// so a turn never reaches a real agent, and the main session stays on its CLI, so with
    /// Codex off a turn has nowhere else to go.
    fn codex_off(repo: &TempRepo) {
        std::fs::write(
            repo.global_config(),
            "crew.main = \"codex\"\n\n[agents.codex]\nenabled = false\nbinary = \"/nonexistent/codex\"\n\n\
             [fallback.main]\nsame_cli = true\n",
        )
        .unwrap();
    }

    #[tokio::test]
    async fn an_agent_turned_on_in_the_app_takes_the_next_turn() {
        let (mut parts, _) = TestCtx::new(CliOptions {
            prepare: Some(Box::new(codex_off)),
            ..CliOptions::default()
        });
        let ctx = &mut parts.ctx;
        // The directory the app builds at start, in place of the CLI's fake one.
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
            ctx.services.plan_usage.clone(),
            ctx.services.health.clone(),
        );
        ctx.services.registry = registry;
        let mut conversation = ConversationState::new(ctx).unwrap();

        let off = conversation::new_request(ctx, &mut conversation, "hi".to_owned())
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
        conversation::new_request(ctx, &mut conversation, "hi".to_owned())
            .await
            .unwrap();
        let mut started = None;
        while let Ok(event) = parts.events.try_recv() {
            if let AppEvent::Conversation {
                event: ConversationEvent::TurnStarted { worker, .. },
            } = event
            {
                started = Some(worker);
            }
        }
        assert_eq!(started, Some(ModelRef::new("codex", None)));
        let done = tokio::time::timeout(WAIT, async {
            loop {
                match parts.internal.recv().await {
                    Some(Internal::Conversation(update @ TurnUpdate::Done { .. })) => {
                        return update;
                    }
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
        conversation::on_update(&mut parts.ctx, &mut conversation, done)
            .await
            .unwrap();
    }

    /// The sandbox mode a conversation turn on `agent` gets under `[sandbox] mode = <mode>`,
    /// through the directory the app builds at start.
    fn conversation_sandbox_for(mode: &'static str, agent: &str) -> SandboxMode {
        let (mut parts, _) = TestCtx::new(CliOptions {
            prepare: Some(Box::new(move |repo: &TempRepo| {
                let config = format!("[sandbox]\nmode = \"{mode}\"\n");
                std::fs::write(repo.global_config(), config).unwrap();
            })),
            ..CliOptions::default()
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
            ctx.services.plan_usage.clone(),
            ctx.services.health.clone(),
        );
        let conversation = ConversationId::numbered(1);
        let turn = TurnId::numbered(1);
        let step = TurnStep::Ask { run_status: None };
        let launch = Launch {
            conversation: &conversation,
            turn: &turn,
            step: &step,
            text: "what does main do?",
            pictures: Vec::new(),
            worker: agent.parse().unwrap(),
            handle: Arc::new(argonauts_testkit::runner::FakeRunner::cli(agent, "test")),
            resume: None,
            fork: false,
            excerpt: None,
            notes: Vec::new(),
            shell: None,
            fast: false,
            workdir: ctx.env.repo_root.clone(),
        };
        let key = SessionKey::numbered(1);
        let dir = ctx.env.state_dir.join("conversation-test");
        session_spec(ctx, &launch, key, dir).sandbox.mode
    }

    #[test]
    fn with_the_tools_own_sandbox_a_conversation_turn_leaves_ours_only_for_a_cli_that_keeps_it_read_only()
     {
        assert_eq!(
            conversation_sandbox_for("tool", "claude-code"),
            SandboxMode::None
        );
        assert_eq!(conversation_sandbox_for("tool", "codex"), SandboxMode::None);
        assert_eq!(
            conversation_sandbox_for("tool", "antigravity-cli"),
            SandboxMode::Os
        );
        assert_eq!(
            conversation_sandbox_for("tool", "opencode"),
            SandboxMode::Os
        );
        assert_eq!(
            conversation_sandbox_for("os", "claude-code"),
            SandboxMode::Os
        );
        assert_eq!(
            conversation_sandbox_for("container", "claude-code"),
            SandboxMode::Container
        );
    }

    #[test]
    fn the_conversation_runs_the_agents_model_and_the_default_roles_effort() {
        let mut settings = Settings::default();
        let agent = settings.agents.entry("codex".to_owned()).or_default();
        agent.model = Some("gpt-5.6-luna".to_owned());
        agent.effort = Some("low".to_owned());
        let codex: ModelRef = "codex".parse().unwrap();
        let (worker, effort) = conversation_agent(&settings, &codex);
        assert_eq!(worker.to_string(), "codex/gpt-5.6-luna");
        assert_eq!(effort.as_deref(), Some("low"));

        let mut role = settings
            .roles
            .get(&RoleName::Main)
            .cloned()
            .unwrap_or_default();
        role.effort = Some("high".to_owned());
        settings.roles.insert(RoleName::Main, role);
        let named: ModelRef = "codex/gpt-5.6".parse().unwrap();
        let (worker, effort) = conversation_agent(&settings, &named);
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
