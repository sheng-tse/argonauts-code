//! What the app has told the screen so far, folded from `AppEvent`s. Plain data: the live
//! region, the panel, the cards, and the overlays read it, and nothing here draws.

mod artifacts;
mod chat;
mod decisions;
mod goals;
mod quota;
mod reported;
mod run;
mod session;
mod talk_time;

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::api::RunSummary;
use argonauts_core::app::{
    AgentReady, AppChoice, AppEvent, AppMode, ChatEvent, ShellEvent, StatusLine, UiCommand,
    ViewData,
};
use argonauts_core::crew_panel::CrewPanel;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::vendor::VendorEvent;
use argonauts_core::limits::{QuotaReading, VendorHealth};
use argonauts_core::names::{CheckpointId, ChoiceId, SessionKey, WorkerRef};
use argonauts_core::policy::{NarratorLine, NarratorStatus};
use argonauts_core::settings::ui::UiSettings;
use argonauts_core::vendor_commands::VendorCommandSet;
use argonauts_core::{AgentEvent, RunId};
use time::{OffsetDateTime, UtcOffset};

use crate::theme::ColorEnv;

pub use artifacts::{Artifacts, Published};
pub use chat::{ChatState, ContextBreakdown};
pub use decisions::DecisionRecord;
pub use goals::GoalBoard;
pub use quota::binding_reading;
pub use run::{
    CardRow, GateBrief, MergeTotals, OpenCheckpoint, RunState, RunTotals, StepState, VendorWait,
    WorkflowState,
};
pub use session::{Doing, Retrying, SessionState, SessionStatus, ToolRecord, task_label};

#[derive(Debug, Clone, Default)]
pub struct AppState {
    pub welcome: Option<Welcome>,
    pub status: Option<StatusLine>,
    /// The latest usage numbers, for the Usage tab and the cards pinned above the input.
    pub usage: Option<std::sync::Arc<argonauts_core::api::UsageSummary>>,
    /// `None` until the first `Mode` event, which reads as idle.
    pub mode: Option<AppMode>,
    /// The narrator's latest line, for the live line under `ui.narrator = "live"`.
    pub narrator: Option<NarratorLine>,
    pub narrator_status: Option<NarratorStatus>,
    pub runs: BTreeMap<RunId, RunState>,
    /// The attached run, the one Esc, `/stop`, and plain text act on.
    pub attached: Option<RunId>,
    /// This conversation's runs going or waiting on you that this screen doesn't hold, from the
    /// last `BackgroundRuns`. Each gets an agent panel row; its card waits until you open it.
    pub background: Vec<RunSummary>,
    /// Runs held here that went on off screen when another run took it, and that a listing has
    /// shown since. Their events stopped coming here, so the listing speaks for them.
    pub listed_off_screen: BTreeSet<RunId>,
    /// Runs held here that a later listing left out: they ended off screen, so their rows go.
    pub ended_off_screen: BTreeSet<RunId>,
    pub chat: ChatState,
    /// The artifacts the conversation and its agents have published.
    pub artifacts: Artifacts,
    /// How long the conversation has gone on, and how much of it its agent worked.
    pub talk: talk_time::TalkTime,
    /// Open app choices, oldest first.
    pub choices: Vec<AppChoice>,
    /// The `!` command that runs now, by its id, so Ctrl+C stops it.
    pub shell: Option<u32>,
    pub replay: Replay,
    /// The event at hand is no part of the replay going on: another run's, streaming beside
    /// the one replaying, or a recap, which no replay sends. It prints as it comes.
    pub(crate) beside_replay: bool,
    /// The last `[ui]` section the controller sent.
    pub ui: Option<UiSettings>,
    /// Notices already shown, so a warning the controller repeats shows once.
    pub notices_seen: BTreeSet<String>,
    /// The last `Notice` folded in had been shown before.
    pub notice_repeated: bool,
    /// The latest quota reading per vendor and window, from `Vendor::Quota`.
    pub quotas: BTreeMap<String, Vec<QuotaReading>>,
    pub health: BTreeMap<String, VendorHealth>,
    /// Vendors kept from new cards, with why.
    pub disabled: BTreeMap<String, String>,
    /// The newest time an event carried.
    pub latest: Option<OffsetDateTime>,
    /// What the runtime learned about the terminal at start.
    pub terminal: TerminalInfo,
    /// The main agent's own commands, from the last `VendorCommands` event.
    pub vendor_commands: Option<VendorCommandSet>,
    /// The crew panel's rows from the controller's last answer: conversations, the CLIs' own
    /// sessions, and runs this screen doesn't hold. Kept so a panel opens with them at once.
    pub crew_panel: Option<CrewPanel>,
    /// Every mod with its file and numbers, from the controller's last answer. What shows a
    /// mod above the input or on the bottom line draws from here.
    pub mods: Option<argonauts_core::settings::mods::ModsView>,
    /// How the model lists named each worker they offered, so a line about the one you picked
    /// names it the same way.
    pub model_words: BTreeMap<String, String>,
    /// What to call each CLI and model, from the last `Names` event.
    pub names: DisplayNames,
    /// The goals agents work toward, as last reported.
    pub goals: GoalBoard,
    /// Cards that wait on you, in the order they arrived.
    waiting: Vec<Waiting>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Welcome {
    pub version: String,
    pub repo: String,
    pub workflow: String,
    /// The agent the startup header names: the welcome's, or else the first status line's.
    pub main_agent: Option<WorkerRef>,
    pub agents: Vec<AgentReady>,
    pub runs_going: u32,
    pub runs_need_you: u32,
    /// The input's example until the first prompt, made from recent commit subjects.
    pub example_prompt: Option<String>,
}

/// Facts about the terminal and the machine that no event carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalInfo {
    /// `None` reads the process environment when a palette is built.
    pub color_env: Option<ColorEnv>,
    /// The terminal's OSC 11 answer, when it came in time, for `ui.theme = "auto"`.
    pub background_is_light: Option<bool>,
    /// For clock times such as `out until 3:45 PM`. UTC when unknown.
    pub utc_offset: Option<UtcOffset>,
}

/// Events between `Attached` and `ReplayDone` rebuild the screen quietly: no bell, no
/// commands, and only collapsed summaries in scrollback.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Replay {
    #[default]
    Live,
    Replaying {
        run: RunId,
    },
}

/// A card that waits on you, from a run's checkpoint or an app choice.
#[derive(Debug, Clone, PartialEq)]
pub enum NeedsYou {
    Checkpoint {
        run: RunId,
        id: CheckpointId,
        title: String,
        set_aside: bool,
    },
    Choice {
        id: ChoiceId,
        title: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Waiting {
    Checkpoint(RunId, CheckpointId),
    Choice(ChoiceId),
}

impl AppState {
    /// Folds one event in. Every event goes through here before anything draws it.
    pub fn apply(&mut self, event: &AppEvent) {
        self.beside_replay = match (&self.replay, event) {
            (Replay::Replaying { run }, AppEvent::Run { event }) => event.run_id != *run,
            (Replay::Replaying { .. }, AppEvent::Recap { .. }) => true,
            _ => false,
        };
        match event {
            AppEvent::Welcome {
                version,
                repo,
                workflow,
                agents,
                runs_going,
                runs_need_you,
                example_prompt,
                main_agent,
                ..
            } => {
                self.welcome = Some(Welcome {
                    version: version.clone(),
                    repo: repo.clone(),
                    workflow: workflow.clone(),
                    main_agent: main_agent.clone(),
                    agents: agents.clone(),
                    runs_going: *runs_going,
                    runs_need_you: *runs_need_you,
                    example_prompt: example_prompt.clone(),
                });
            }
            AppEvent::Attached { summary, .. } => {
                let run = self.run_mut(&summary.run);
                run.title.clone_from(&summary.title);
                run.mode = Some(summary.mode);
                run.workflow = Some(summary.workflow.clone());
                run.spent = summary.spent;
                self.attached = Some(summary.run.clone());
                self.listed_off_screen.remove(&summary.run);
                self.ended_off_screen.remove(&summary.run);
                self.replay = Replay::Replaying {
                    run: summary.run.clone(),
                };
            }
            AppEvent::Detached { run } => {
                if self.attached.as_ref() == Some(run) {
                    self.attached = None;
                }
                if self.replay == (Replay::Replaying { run: run.clone() }) {
                    self.replay = Replay::Live;
                }
            }
            AppEvent::BackgroundRuns { runs } => {
                self.background.clone_from(runs);
                self.follow_off_screen();
            }
            AppEvent::Run { event } => {
                self.apply_run(event);
                self.note_run_artifact(event);
            }
            AppEvent::Chat { event } => {
                // A chat turn's quota reading is as good as a run's, so the mode line shows it.
                if let ChatEvent::Agent {
                    event: AgentEvent::Quota { reading },
                    ..
                } = event
                {
                    quota::record(&mut self.quotas, reading);
                }
                self.talk.count_edit(event, &self.chat.tools);
                self.chat.apply(event);
                self.talk.apply(event, crate::bottom::wall_now());
                // A page the main agent just published, read off the call that finished.
                if let ChatEvent::Agent {
                    event: AgentEvent::ToolFinished { id, .. },
                    ..
                } = event
                    && let Some(call) = self.chat.tools.iter().rev().find(|call| call.id == *id)
                {
                    self.artifacts.note(call, None);
                }
            }
            AppEvent::Choice { choice } => {
                self.choices.retain(|known| known.id != choice.id);
                self.choices.push(choice.clone());
                self.wait_on(Waiting::Choice(choice.id.clone()));
            }
            AppEvent::ChoiceClosed { id, .. } => {
                self.choices.retain(|choice| choice.id != *id);
                self.waiting
                    .retain(|waiting| *waiting != Waiting::Choice(id.clone()));
            }
            AppEvent::Narrator { line } => {
                self.saw(line.ts);
                self.narrator = Some(line.clone());
            }
            AppEvent::NarratorStatus { status } => self.narrator_status = Some(status.clone()),
            AppEvent::Mode { mode } => self.apply_mode(mode),
            AppEvent::Status { status } => {
                if let Some(welcome) = self.welcome.as_mut() {
                    welcome
                        .main_agent
                        .get_or_insert_with(|| status.main_agent.clone());
                }
                self.status = Some(status.clone());
            }
            AppEvent::Notice { text, .. } => {
                self.notice_repeated = !self.notices_seen.insert(text.clone());
            }
            AppEvent::UiSettings { ui, .. } => {
                if let Some(ui) = ui {
                    self.ui = Some((**ui).clone());
                }
            }
            AppEvent::TakenBack { texts } => self.taken_back(texts),
            AppEvent::ChatCleared => {
                self.chat.heads_up = None;
                self.artifacts.clear();
            }
            // The conversation on screen changed: its own pages replace the last one's.
            AppEvent::ChatResumed { artifacts, .. } => self.artifacts.resumed(artifacts),
            AppEvent::VendorCommands { set } => self.vendor_commands = Some(set.clone()),
            AppEvent::Names { names } => self.names = (**names).clone(),
            AppEvent::ReplayDone { run, .. } => {
                if self.replay == (Replay::Replaying { run: run.clone() }) {
                    self.replay = Replay::Live;
                }
            }
            AppEvent::View { data } => match data.as_ref() {
                ViewData::Mods { view } => self.mods = Some((**view).clone()),
                ViewData::Crew { panel } => self.crew_panel = Some(panel.clone()),
                ViewData::Usage { summary } => {
                    self.usage = Some(std::sync::Arc::new((**summary).clone()));
                }
                ViewData::Models { choices, .. } => {
                    for choice in choices {
                        self.model_words.insert(
                            choice.worker.to_string(),
                            crate::format::choice_words(choice),
                        );
                    }
                }
                ViewData::Workflow { view } => self.goals.remember(&view.run, &view.goals),
                ViewData::Agent { view } => {
                    self.goals
                        .remember_agent(&view.run, &view.row, view.goal.as_deref());
                }
                _ => {}
            },
            AppEvent::Shell { event } => match event {
                ShellEvent::Started { id, .. } => self.shell = Some(*id),
                ShellEvent::Finished { id, .. } if self.shell == Some(*id) => self.shell = None,
                ShellEvent::Output { .. } | ShellEvent::Finished { .. } => {}
            },
            AppEvent::TakeOver { .. }
            | AppEvent::RunInTerminal { .. }
            | AppEvent::Recap { .. }
            | AppEvent::SettingChanged { .. }
            | AppEvent::RepoFiles { .. }
            | AppEvent::CondensedReplay { .. }
            | AppEvent::Report { .. }
            | AppEvent::Folded { .. }
            | AppEvent::Continued { .. }
            | AppEvent::Hint { .. }
            | AppEvent::Exit => {}
        }
    }

    pub fn replaying(&self) -> bool {
        matches!(self.replay, Replay::Replaying { .. }) && !self.beside_replay
    }

    pub fn attached_run(&self) -> Option<&RunState> {
        self.attached.as_ref().and_then(|id| self.runs.get(id))
    }

    /// The run the screen's own numbers are about, such as a pinned card's: the one the
    /// app's mode names, else the attached one.
    pub fn followed_run(&self) -> Option<&RunState> {
        let named = match &self.mode {
            Some(
                AppMode::Working { run } | AppMode::Pausing { run } | AppMode::Parked { run, .. },
            ) => self.runs.get(run),
            _ => None,
        };
        named.or_else(|| self.attached_run())
    }

    pub fn session(&self, run: &RunId, key: &SessionKey) -> Option<&SessionState> {
        self.runs.get(run).and_then(|run| run.sessions.get(key))
    }

    /// What the latest listing says of a run held here that goes on off screen, as one does
    /// once a second workflow takes the screen.
    pub fn listed_off_screen(&self, run: &RunId) -> Option<&RunSummary> {
        if self.attached.as_ref() == Some(run) {
            return None;
        }
        self.background.iter().find(|listed| &listed.run == run)
    }

    /// Notes which runs held here a listing shows off screen, and which it has left out since,
    /// as they ended there.
    fn follow_off_screen(&mut self) {
        let off_screen = self
            .runs
            .values()
            .filter(|run| run.finished.is_none() && self.attached.as_ref() != Some(&run.id));
        for run in off_screen {
            if self.background.iter().any(|listed| listed.run == run.id) {
                self.ended_off_screen.remove(&run.id);
                self.listed_off_screen.insert(run.id.clone());
            } else if self.listed_off_screen.remove(&run.id) {
                self.ended_off_screen.insert(run.id.clone());
            }
        }
    }

    /// Cards waiting on you in runs this screen doesn't hold, which only their panel rows show.
    pub fn background_waiting(&self) -> usize {
        self.background
            .iter()
            .filter(|run| run.needs_you() && !self.runs.contains_key(&run.run))
            .map(|run| run.waiting.len())
            .sum()
    }

    /// Cards waiting on you across every run and app choice, oldest first.
    pub fn needs_you(&self) -> Vec<NeedsYou> {
        // Cards set straight into the state, not folded from events, come after the rest.
        let mut order = self.waiting.clone();
        for run in self.runs.values() {
            for open in &run.checkpoints {
                let waiting = Waiting::Checkpoint(run.id.clone(), open.opened.id.clone());
                if !order.contains(&waiting) {
                    order.push(waiting);
                }
            }
        }
        for choice in &self.choices {
            let waiting = Waiting::Choice(choice.id.clone());
            if !order.contains(&waiting) {
                order.push(waiting);
            }
        }
        order
            .iter()
            .filter_map(|waiting| match waiting {
                Waiting::Checkpoint(run, id) => {
                    let open = self.runs.get(run)?.checkpoint(id)?;
                    open.opened.needs_you.then(|| NeedsYou::Checkpoint {
                        run: run.clone(),
                        id: id.clone(),
                        title: open.opened.title.clone(),
                        set_aside: open.set_aside,
                    })
                }
                Waiting::Choice(id) => {
                    let choice = self.choices.iter().find(|choice| choice.id == *id)?;
                    Some(NeedsYou::Choice {
                        id: id.clone(),
                        title: choice.title.clone(),
                    })
                }
            })
            .collect()
    }

    /// The session a follow key (1 to 9) points at in the attached run. When a key was used
    /// again, the latest session with it wins.
    pub fn follow(&self, key: u8) -> Option<(&RunId, &SessionState)> {
        let run = self.attached_run()?;
        run.sessions_in_order()
            .filter(|session| session.follow == Some(key))
            .max_by_key(|session| (session.is_running(), session.started))
            .map(|session| (&run.id, session))
    }

    /// Esc handed a checkpoint card on, or it came back.
    pub fn set_aside(&mut self, run: &RunId, id: &CheckpointId, aside: bool) {
        if let Some(open) = self.runs.get_mut(run).and_then(|run| {
            run.checkpoints
                .iter_mut()
                .find(|open| open.opened.id == *id)
        }) {
            open.set_aside = aside;
        }
    }

    /// The terminal's zone, for clock times. UTC when unknown.
    pub fn clock_offset(&self) -> UtcOffset {
        self.terminal.utc_offset.unwrap_or(UtcOffset::UTC)
    }

    /// Folds in what a command from this screen settles before the controller answers: a
    /// spot check you marked, from its card or `/approvals`, no longer waits.
    pub fn sent(&mut self, command: &UiCommand) {
        if let UiCommand::SpotCheck { run, id, .. } = command
            && let Some(run) = self.runs.get_mut(run)
        {
            run.spot_checked(id);
        }
    }

    /// Agents running in every run.
    pub fn agents_running(&self) -> usize {
        self.runs
            .values()
            .map(|run| run.running_sessions().count())
            .sum()
    }

    /// A page an agent inside a run just published, read off the call that finished.
    fn note_run_artifact(&mut self, event: &argonauts_core::events::RunEvent) {
        let RunEventKind::Agent(AgentEvent::ToolFinished { id, .. }) = &event.kind else {
            return;
        };
        let Some(key) = &event.session else {
            return;
        };
        let call = self
            .runs
            .get(&event.run_id)
            .and_then(|run| run.sessions.get(key))
            .and_then(|session| session.tools.iter().rev().find(|call| call.id == *id));
        if let Some(call) = call {
            self.artifacts
                .note(call, Some((event.run_id.clone(), key.clone())));
        }
    }

    fn apply_run(&mut self, event: &argonauts_core::events::RunEvent) {
        self.saw(event.ts);
        if let RunEventKind::Goal(goal) = &event.kind {
            self.goals.told(&event.run_id, goal);
        }
        match &event.kind {
            RunEventKind::Vendor(vendor) => self.apply_vendor(vendor),
            RunEventKind::Agent(AgentEvent::Quota { reading }) => {
                quota::record(&mut self.quotas, reading);
            }
            _ => {}
        }
        let replaying = self.replaying();
        let opened = self.run_mut(&event.run_id).apply(event, replaying);
        // A session that names no model runs the one its CLI's settings give, else its CLI's
        // default, and reads so from the first row that names it.
        if let RunEventKind::Session(SessionEvent::Started(_)) = &event.kind
            && let Some(key) = &event.session
            && let Some(session) = self
                .runs
                .get_mut(&event.run_id)
                .and_then(|run| run.sessions.get_mut(key))
        {
            session.model_assumed = self.names.by_default(&session.worker);
            session.worker = self.names.running(&session.worker);
        }
        if let Some(id) = opened {
            self.wait_on(Waiting::Checkpoint(event.run_id.clone(), id));
        }
        let open: Vec<CheckpointId> = self.runs[&event.run_id]
            .checkpoints
            .iter()
            .map(|open| open.opened.id.clone())
            .collect();
        let run_id = &event.run_id;
        self.waiting.retain(|waiting| match waiting {
            Waiting::Checkpoint(run, id) => run != run_id || open.contains(id),
            Waiting::Choice(_) => true,
        });
    }

    fn apply_vendor(&mut self, event: &VendorEvent) {
        match event {
            VendorEvent::Quota(reading) => quota::record(&mut self.quotas, reading),
            VendorEvent::Health(health) => {
                self.health.insert(health.vendor.0.clone(), health.clone());
            }
            VendorEvent::Disabled { vendor, reason } => {
                self.disabled.insert(vendor.0.clone(), reason.clone());
            }
        }
    }

    fn apply_mode(&mut self, mode: &AppMode) {
        match mode {
            AppMode::Pausing { run } => self.run_mut(run).pausing = true,
            AppMode::Working { run } => {
                let run = self.run_mut(run);
                run.pausing = false;
                run.parked = None;
            }
            AppMode::Parked { run, reason } => {
                let run = self.run_mut(run);
                run.pausing = false;
                run.parked = Some(reason.clone());
            }
            AppMode::Idle | AppMode::Answering { .. } => {}
        }
        // The run the app works on is the one on screen, also when it started here rather than
        // through `Attached`, so mentions, `/workflows`, follow keys, and queued text act on it.
        if let AppMode::Working { run } | AppMode::Pausing { run } | AppMode::Parked { run, .. } =
            mode
        {
            self.attached = Some(run.clone());
        }
        self.mode = Some(mode.clone());
    }

    /// Texts taken back into the input stop showing as queued.
    fn taken_back(&mut self, texts: &[String]) {
        for text in texts {
            if let Some(at) = self.chat.queued.iter().position(|queued| queued == text) {
                self.chat.queued.remove(at);
            }
            for run in self.runs.values_mut() {
                run.queued.retain(|queued| queued.text != *text);
                for session in run.sessions.values_mut() {
                    session.queued.retain(|queued| queued.text != *text);
                }
            }
        }
    }

    fn wait_on(&mut self, waiting: Waiting) {
        self.waiting.retain(|known| *known != waiting);
        self.waiting.push(waiting);
    }

    fn saw(&mut self, at: OffsetDateTime) {
        if self.latest.is_none_or(|latest| at > latest) {
            self.latest = Some(at);
        }
    }

    fn run_mut(&mut self, id: &RunId) -> &mut RunState {
        self.runs
            .entry(id.clone())
            .or_insert_with(|| RunState::new(id.clone()))
    }
}

#[cfg(test)]
mod tests;
