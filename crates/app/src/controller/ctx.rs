//! What every handler shares: the environment, the settings, the services, the app mode, and
//! the way out to the TUI.

use std::path::PathBuf;
use std::sync::Arc;

use argonauts_core::NoteLevel;
use argonauts_core::api::{Approvals, ConductorApi, StartRun};
use argonauts_core::app::{AppEvent, AppMode, UiCommand};
use argonauts_core::clock::Clock;
use argonauts_core::limits::{HealthBoard, QuotaBoard};
use argonauts_core::names::ChoiceId;
use argonauts_core::registry::ModelRegistry;
use argonauts_core::settings::{SettingScope, Settings};
use argonauts_core::steps::StepRunner;
use argonauts_core::worker::WorkerDirectory;
use tokio::sync::mpsc;

use super::chat::TurnUpdate;
use super::runs::RunUpdate;
use crate::app_settings::AppSettings;
use crate::env::AppEnv;
use crate::setup::SetupUpdate;
use crate::trace::Trace;

pub(crate) struct AppServices {
    pub conductor: Arc<dyn ConductorApi>,
    /// `None` without a terminal or inside an agent, so nothing can be approved from here.
    pub approvals: Option<Arc<dyn Approvals>>,
    /// For the app's own sessions: chat, triage, and the narrator.
    pub workers: Arc<dyn WorkerDirectory>,
    pub steps: Arc<dyn StepRunner>,
    pub registry: Arc<dyn ModelRegistry>,
    pub quota: Arc<dyn QuotaBoard>,
    pub health: Arc<dyn HealthBoard>,
    pub clock: Arc<dyn Clock>,
}

/// Every `AppEvent` goes out through here, so the trace sees each one.
#[derive(Clone)]
pub(crate) struct Outbox {
    events: mpsc::UnboundedSender<AppEvent>,
    trace: Arc<Trace>,
}

impl Outbox {
    pub(crate) fn new(events: mpsc::UnboundedSender<AppEvent>, trace: Arc<Trace>) -> Self {
        Self { events, trace }
    }

    /// Never blocks. After the screen closed, events have nowhere to go and are dropped.
    pub(crate) fn send(&self, event: AppEvent) {
        self.trace.event(&event);
        let _ = self.events.send(event);
    }

    pub(crate) fn trace(&self) -> &Trace {
        &self.trace
    }
}

/// Messages from the tasks the handlers start: chat turns, run streams, setup probes, and model
/// list refreshes.
pub(crate) enum Internal {
    Chat(TurnUpdate),
    Run(RunUpdate),
    Setup(SetupUpdate),
    /// A `!` command printed or ended.
    Shell(super::shell::ShellUpdate),
    /// The CLIs' model lists were read again.
    ModelsRefreshed,
}

/// What one part of the controller asks of another. The loop applies them in order.
#[derive(Debug)]
pub(crate) enum Effect {
    /// Start this run and attach to it. The detected gates are offered first, once.
    StartRun(Box<StartRun>),
    /// The default agent changed: start a fresh chat session primed with an excerpt.
    FreshChat,
    /// Settings in effect changed: the narrator hears of it and a new status line goes out.
    SettingsApplied { keys: Vec<String> },
    /// Ask where to save a setting, as an argument like `/effort high` does.
    AskScope(Box<ScopeRequest>),
    /// Save these `(key, TOML value)` settings where you picked, through the same confirming
    /// and trust steps as `/config`.
    SaveSettings {
        pairs: Vec<(String, String)>,
        scope: SettingScope,
    },
    /// Send a new status line.
    Status,
    /// Open the guided setup again (`/setup`).
    OpenSetup,
    /// The guided setup finished or was set aside.
    SetupDone,
    /// Input to handle as if you had just sent it.
    Input(Box<UiCommand>),
    /// Input typed while a chat turn ran, handled now as if you had just sent it. It hears of
    /// the `!` commands up to `typed_after`, the last that had ended when you typed it.
    Queued {
        command: Box<UiCommand>,
        typed_after: u32,
    },
    /// Close the screen. Runs keep going.
    Exit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopeRequest {
    pub key: String,
    /// TOML text.
    pub value: String,
    /// The settings command whose default scope applies, such as `handoff`.
    pub command: String,
}

pub(crate) struct Ctx {
    pub env: Arc<AppEnv>,
    pub settings: AppSettings,
    pub services: AppServices,
    pub mode: AppMode,
    pub out: Outbox,
    pub internal: mpsc::UnboundedSender<Internal>,
    next_choice: u32,
    /// Where `/cd` asked the app to open next, read once the screen has closed.
    reopen: Reopen,
    /// What the screen was last told to call each CLI and model.
    pub names: super::names::NamesSent,
    /// The runs' agents' sessions as their journals were last read, for `/resume`.
    pub agent_sessions: Arc<super::chat::SessionCache>,
}

/// Shared with whoever started the app, which outlives the controller.
pub(crate) type Reopen = Arc<std::sync::Mutex<Option<PathBuf>>>;

impl Ctx {
    pub(crate) fn new(
        env: Arc<AppEnv>,
        settings: AppSettings,
        services: AppServices,
        out: Outbox,
        internal: mpsc::UnboundedSender<Internal>,
    ) -> Self {
        Self {
            env,
            settings,
            services,
            mode: AppMode::Idle,
            out,
            internal,
            next_choice: 0,
            reopen: Reopen::default(),
            names: super::names::NamesSent::default(),
            agent_sessions: Arc::default(),
        }
    }

    /// The app opens again in `repo` once this screen closes.
    pub(crate) fn reopen_in(&self, repo: PathBuf) {
        *self
            .reopen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(repo);
    }

    pub(crate) fn reopen(&self) -> Reopen {
        Arc::clone(&self.reopen)
    }

    pub(crate) fn settings(&self) -> &Settings {
        self.settings.settings()
    }

    pub(crate) fn send(&self, event: AppEvent) {
        self.out.send(event);
    }

    pub(crate) fn notice(&self, level: NoteLevel, text: impl Into<String>) {
        self.send(AppEvent::Notice {
            level,
            text: text.into(),
        });
    }

    /// Every error a handler returns reaches you as a notice, naming files in the repo by
    /// their path from it.
    pub(crate) fn error(&self, err: &anyhow::Error) {
        self.notice(NoteLevel::Error, self.env.shown_in(&format!("{err:#}")));
    }

    /// Sends `Mode` only when it changed.
    pub(crate) fn set_mode(&mut self, mode: AppMode) {
        if self.mode != mode {
            self.mode = mode.clone();
            self.send(AppEvent::Mode { mode });
        }
    }

    pub(crate) fn next_choice_id(&mut self) -> ChoiceId {
        self.next_choice += 1;
        ChoiceId::numbered(self.next_choice)
    }
}
