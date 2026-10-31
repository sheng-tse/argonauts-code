//! What every handler shares: the environment, the settings, the services, the app mode, and
//! the way out to the TUI.

use std::path::PathBuf;
use std::sync::Arc;

use argonauts_core::NoteLevel;
use argonauts_core::agents::session_runner::RunnerDirectory;
use argonauts_core::app::{AppEvent, AppMode, UiCommand};
use argonauts_core::base::clock::Clock;
use argonauts_core::base::names::ChoiceId;
use argonauts_core::models::limits::{HealthBoard, PlanUsageBoard};
use argonauts_core::models::registry::ModelRegistry;
use argonauts_core::runs::api::{Approvals, ConductorApi, StartRun};
use argonauts_core::settings::{SettingScope, Settings};
use argonauts_core::workflows::steps::StepRunner;
use tokio::sync::mpsc;

use crate::controller::conversation::TurnUpdate;
use crate::controller::runs::RunUpdate;
use crate::setup::SetupUpdate;
use crate::startup::app_settings::AppSettings;
use crate::startup::env::AppEnv;
use crate::testing::trace::Trace;

pub(crate) struct AppServices {
    pub conductor: Arc<dyn ConductorApi>,
    /// `None` without a terminal or inside an agent, so nothing can be approved from here.
    pub approvals: Option<Arc<dyn Approvals>>,
    /// For the app's own sessions: conversation, the estimate, and the narrator.
    pub workers: Arc<dyn RunnerDirectory>,
    pub steps: Arc<dyn StepRunner>,
    pub registry: Arc<dyn ModelRegistry>,
    pub plan_usage: Arc<dyn PlanUsageBoard>,
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

/// Messages from the tasks the handlers start: conversation turns, run streams, setup probes, and
/// model list refreshes.
pub(crate) enum Internal {
    Conversation(TurnUpdate),
    Run(RunUpdate),
    Setup(SetupUpdate),
    /// A mod file changed, or a mod's command printed something new.
    Mods,
    /// A `!` command printed or ended.
    Shell(crate::controller::shell::ShellUpdate),
    /// The CLIs' model lists were read again.
    ModelsRefreshed,
    /// One CLI's or endpoint's model list was read again.
    ModelSourceListed {
        cli: String,
    },
    /// While `/model`'s picker is open: time to ask the local servers again.
    ModelsTick,
    /// What this machine gives models and what the local ones take, or nothing where it
    /// couldn't be read.
    MemoryRead(Option<Box<crate::controller::settings::memory_facts::Reading>>),
    /// The workflow file `/workflow edit` watches was saved.
    WorkflowSaved {
        name: String,
    },
    /// The check of the goal the main agent works toward alone came back.
    Goal(crate::controller::conversation::GoalChecked),
    /// A CLI's plan usage was read live, so the bottom line shows the new reading.
    PlanUsageRead,
    /// The update of a CLI a run waits on has ended.
    CliUpdated(Box<crate::controller::needed_update::Updated>),
    /// What GitHub says of the pull requests Argonauts Code follows.
    Followed(Box<crate::issue::follow::Looked>),
    /// The deeper Behaviors map's update ended, so the map on screen is sent again: the
    /// deeper one when the tab started it.
    DeepMapMade {
        deeper: bool,
    },
    /// The logins a workflow switch read came back.
    WorkflowSwitch(crate::controller::workflow_switch::Probed),
    /// Settings worked out while handling something else, such as what a request said to keep
    /// "from now on": saved once that's done, as `Effect::SaveChanges` saves them.
    SaveChanges {
        pairs: Vec<(String, String)>,
    },
}

/// What one part of the controller asks of another. The loop applies them in order.
#[derive(Debug)]
pub(crate) enum Effect {
    /// Start this run and attach to it. A CLI it would use that must be updated is asked about
    /// first, then the detected checks are offered, once.
    StartRun(Box<StartRun>),
    /// The main agent changed: its session carries on, or another CLI starts from what carries
    /// over.
    FreshConversation,
    /// A workflow of yours was written or removed, so the `/` menu lists the commands again.
    CommandsChanged,
    /// Settings in effect changed: the narrator hears of it, and a new header and bottom line
    /// go out.
    SettingsApplied { keys: Vec<String> },
    /// Save these `(key, TOML value)` settings where `save_changes_to` says, as every change
    /// made in the app is, through the same confirming and trust steps as `/config`, with one
    /// line saying where they went.
    SaveChanges { pairs: Vec<(String, String)> },
    /// Save these settings in `scope`, a place named on purpose, as a flag names one.
    SaveSettings {
        pairs: Vec<(String, String)>,
        scope: SettingScope,
    },
    /// Save these settings where you picked on a setup's question. The pick confirmed them, so a
    /// safety key among them asks nothing more.
    SaveConfirmedSettings {
        pairs: Vec<(String, String)>,
        scope: SettingScope,
    },
    /// Save these settings in `scope` because your own words asked to keep them, as "keep these
    /// checks" does. Your words confirmed them, so nothing more asks, and the line after names
    /// each key alone.
    KeepSettings {
        pairs: Vec<(String, String)>,
        scope: SettingScope,
    },
    /// Send a new header and bottom line.
    Status,
    /// Open the guided setup again (`/setup`).
    OpenSetup,
    /// The guided setup finished or was set aside.
    SetupDone,
    /// Input to handle as if you had just sent it.
    Input(Box<UiCommand>),
    /// Input typed while a conversation turn ran, handled now as if you had just sent it. It hears
    /// of the `!` commands up to `typed_after`, the last that had ended when you typed it.
    Queued {
        command: Box<UiCommand>,
        typed_after: u32,
    },
    /// Close the screen. Runs keep going.
    Exit,
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
    pub names: crate::controller::clis::names::NamesSent,
    /// The runs' agents' sessions as their journals were last read, for `/resume`.
    pub agent_sessions: Arc<crate::controller::conversation::SessionCache>,
    /// Holds the computer's sleep off while anything works, and for `/awake`.
    pub awake: crate::host::keep_awake::KeepAwake,
    /// What the Status tab knows of the CLIs: Doctor's check, and who each is signed in as.
    pub cli_facts: Arc<crate::controller::panels::status_tab::CliFacts>,
    /// The deeper Behaviors map's update, while one runs.
    pub deep: crate::behaviors::deep::tab::DeepWork,
    /// Whether the workflow in use talks, as each turn's start asks.
    pub(crate) talking: crate::controller::workflows::Talking,
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
            names: crate::controller::clis::names::NamesSent::default(),
            agent_sessions: Arc::default(),
            // The app's own start gives it this computer's way to hold sleep off.
            awake: crate::host::keep_awake::KeepAwake::never(),
            cli_facts: Arc::default(),
            deep: Default::default(),
            talking: Default::default(),
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

    /// The answer to a key that can't do anything now, for the notice line.
    pub(crate) fn hint(&self, text: impl Into<String>) {
        self.send(AppEvent::Hint { text: text.into() });
    }

    /// Every error a handler returns reaches you as a notice, naming files in the repo by
    /// their path from it.
    pub(crate) fn error(&self, err: &anyhow::Error) {
        self.notice(NoteLevel::Error, self.env.shown_in(&format!("{err:#}")));
    }

    /// Sends `Mode` only when it changed.
    pub(crate) fn set_mode(&mut self, mode: AppMode) {
        if self.mode != mode {
            // An answer or a run you watch is under way, so the computer stays awake.
            let working = matches!(
                mode,
                AppMode::Answering { .. } | AppMode::Working { .. } | AppMode::Pausing { .. }
            );
            let settings = self.settings().awake.clone();
            self.awake.working(working, &settings);
            self.mode = mode.clone();
            self.send(AppEvent::Mode { mode });
        }
    }

    pub(crate) fn next_choice_id(&mut self) -> ChoiceId {
        self.next_choice += 1;
        ChoiceId::numbered(self.next_choice)
    }
}
