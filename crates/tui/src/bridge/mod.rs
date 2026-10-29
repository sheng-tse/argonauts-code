//! Connects the screen to the app controller. Every `AppEvent` is folded into [`AppState`] and
//! then offered to each part in turn, and every `UiAction` the screen reports is offered until
//! one part takes it. Parts never draw directly: they change the view model and ask for
//! commands, views, and the bell through [`Effects`].

pub mod agents;
mod band;
pub mod bottom_line;
pub mod cli_workflow_run;
pub mod council;
#[cfg(test)]
mod council_golden;
mod ended;
pub mod failure;
mod gave;
pub mod input;
pub mod keys;
pub mod panel;
pub mod plugin_output;
mod pull_request;
pub mod refusal;
#[cfg(test)]
mod resumed_golden;
mod reviewed;
#[cfg(test)]
mod reviewed_golden;
pub mod settings;
#[cfg(test)]
mod shape_golden;
pub mod status;
pub mod tools;
pub mod transcript;
#[cfg(test)]
mod workflow_lines_golden;

use std::path::PathBuf;
use std::time::Instant;

use argonauts_core::RunId;
use argonauts_core::Verbosity;
use argonauts_core::api::TakeOverTicket;
use argonauts_core::app::{AppEvent, AppMode, UiCommand, ViewData};
use argonauts_core::keywords::Keyword;
use argonauts_core::names::ModelRef;
use argonauts_core::settings::ui::{TakeOverWhere, UiSettings};

use crate::UiOptions;
use crate::app::UiAction;
use crate::model::ViewModel;
use crate::overview::{OverviewView, Tab};
use crate::questions::Questions;
use crate::state::AppState;
use crate::theme::Theme;
use crate::views::follow::Refresh;
use crate::views::{self, View};

use agents::AgentBlocks;
use input::InputRouter;
use keys::KeyActions;
use transcript::Transcript;

/// What a part may touch while it handles one event or action.
pub struct Cx<'a> {
    pub model: &'a mut ViewModel,
    pub state: &'a AppState,
    pub settings: &'a UiSettings,
    pub now: Instant,
    pub out: &'a mut Effects,
}

impl Cx<'_> {
    pub fn verbosity(&self) -> Verbosity {
        self.settings.verbosity
    }

    /// Nothing the event says happens again: it is history, replayed or drawn from the record.
    pub fn replaying(&self) -> bool {
        self.state.replaying()
    }

    /// The event's items join the Ctrl+O history without printing.
    pub fn history_only(&self) -> bool {
        self.state.history_only()
    }

    /// The event is part of a past drawn again, which prints at once.
    pub fn drawing_past(&self) -> bool {
        self.state.drawing_past()
    }
}

impl<'a> Cx<'a> {
    /// The main agent CLI's keywords.
    pub fn keywords(&self) -> &'a [Keyword] {
        let state: &'a AppState = self.state;
        state
            .cli_commands
            .as_ref()
            .map_or(&[], |set| set.keywords.as_slice())
    }

    /// `worker`'s keywords when it runs the main agent's CLI, the one whose table the screen
    /// has; none otherwise.
    pub fn keywords_of(&self, worker: &ModelRef) -> &'a [Keyword] {
        let state: &'a AppState = self.state;
        match &state.cli_commands {
            Some(set) if set.agent_id.0 == worker.agent => &set.keywords,
            _ => &[],
        }
    }
}

/// What the runtime does after an event or action: send commands, ring the bell, open views,
/// hand the terminal over, or leave.
#[derive(Debug, Default)]
pub struct Effects {
    pub commands: Vec<UiCommand>,
    /// A slash command as you typed it, which the runtime sends ahead of the commands so the
    /// conversation's record keeps it.
    pub typed: Option<String>,
    /// Lines a command you typed printed back, which the runtime sends after the commands so
    /// the conversation's record keeps them after the command.
    pub replies: Vec<argonauts_core::app::CommandReply>,
    pub bell: bool,
    /// Pushed on the view stack in this order.
    pub views: Vec<Box<dyn View>>,
    /// The views answer a command typed while an answer ran, so they open as a panel under
    /// the working line and the answer keeps running above.
    pub views_as_panel: bool,
    /// Close every open view first, such as when a question from the agent panel gets the keys.
    pub close_views: bool,
    /// Close the views a question would hide, down to one that stays up over it, as for the
    /// question a setting change from a view asks.
    pub views_make_way: bool,
    pub take_over: Option<TakeOverJob>,
    /// An agent CLI's own command, such as its login, to run with the terminal handed over.
    pub run_in_terminal: Option<TerminalJob>,
    /// Text for the system clipboard, from `/copy` or a selection.
    pub clipboard: Option<String>,
    /// A claude.ai page to open in the browser, such as an artifact.
    pub open_link: Option<String>,
    /// Open `$VISUAL` or `$EDITOR` on the draft.
    pub edit_draft: bool,
    /// A new palette to use from the next frame.
    pub theme: Option<Theme>,
    /// Applied at once by a `Both` command, before the controller confirms it.
    pub verbosity: Option<Verbosity>,
    pub vim: Option<bool>,
    pub exit: bool,
    /// Dim lines left in scrollback on exit, such as `Reattach with: argonauts`.
    pub exit_lines: Vec<String>,
}

impl Effects {
    pub fn send(&mut self, command: UiCommand) {
        self.commands.push(command);
    }

    pub fn open(&mut self, view: impl View + 'static) {
        self.views.push(Box::new(view));
    }

    pub fn ring(&mut self) {
        self.bell = true;
    }

    /// History is being replayed: nothing rings and nothing is sent.
    fn quiet_for_replay(&mut self) {
        self.bell = false;
        self.commands.clear();
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TakeOverJob {
    pub ticket: TakeOverTicket,
    pub place: TakeOverWhere,
    pub run: RunId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalJob {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env_remove: Vec<String>,
    pub env_set: Vec<(String, String)>,
    /// The app says what came back, so only a failure is the screen's to say.
    pub quiet: bool,
}

#[derive(Debug)]
pub struct Bridge {
    pub state: AppState,
    settings: UiSettings,
    pub transcript: Transcript,
    pub agents: AgentBlocks,
    pub council: council::CouncilBridge,
    pub questions: Questions,
    pub input: InputRouter,
    pub keys: KeyActions,
    refresh: Refresh,
}

impl Bridge {
    pub fn new(options: &UiOptions) -> Self {
        // Views read `[ui]` from the state, such as vim keys in lists, and the controller only
        // sends it again when it changes.
        let mut ui = options.settings.clone();
        ui.keys = options.keys;
        let mut state = AppState::default();
        state.ui = Some(ui);
        Self {
            state,
            settings: options.settings.clone(),
            transcript: Transcript::default(),
            agents: AgentBlocks::default(),
            council: council::CouncilBridge::default(),
            questions: Questions::default(),
            input: InputRouter::new(options),
            keys: KeyActions::default(),
            refresh: Refresh::default(),
        }
    }

    pub fn settings(&self) -> &UiSettings {
        &self.settings
    }

    pub fn on_event(&mut self, model: &mut ViewModel, event: &AppEvent, now: Instant) -> Effects {
        // A stream opened again repeats what this screen took in already.
        if let AppEvent::Run { event } = event
            && self.state.repeats(event)
        {
            return Effects::default();
        }
        self.state.apply(event);
        if let AppEvent::UiSettings { ui: Some(ui), .. } = event {
            self.settings = (**ui).clone();
        }
        let mut out = Effects::default();
        // A refresh's answer that no view took is for a view that closed or is covered.
        let unclaimed = self.refresh.unclaimed(event, now);
        // So is the answer to a refresh of the usage panel's open tab, once the panel has
        // closed: it opens nothing and prints nothing.
        let stale_tab = matches!(event, AppEvent::View { data }
            if model.bottom.band.watch.answered(data, now));
        let status_view = match event {
            AppEvent::View { data } if !self.state.replaying() && !stale_tab => {
                match data.as_ref() {
                    ViewData::StatusTab { view } if std::mem::take(&mut self.input.status_view) => {
                        Some(view)
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        // `/context all` opens the Context tab with every list open.
        let every_list = match event {
            AppEvent::View { data } if !self.state.replaying() && !stale_tab && !unclaimed => {
                match data.as_ref() {
                    ViewData::Context { view } if std::mem::take(&mut self.input.context_all) => {
                        Some(view)
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        {
            let mut cx = Cx {
                model,
                state: &self.state,
                settings: &self.settings,
                now,
                out: &mut out,
            };
            settings::on_event(event, &mut cx);
            self.input.answers_mid_answer_command(event, &mut cx);
            if let Some(view) = every_list {
                cx.out.open(
                    OverviewView::new(Tab::Context, cx.model.bottom.band.watch.clone())
                        .with_context(view.clone())
                        .with_every_list(),
                );
            }
            match status_view {
                // Under its tab of the usage panel, beside Usage and Context.
                Some(view) => cx.out.open(
                    OverviewView::new(Tab::Status, cx.model.bottom.band.watch.clone())
                        .with_status_tab((**view).clone()),
                ),
                None if stale_tab => {}
                None => self.transcript.on_event(event, &mut cx),
            }
            self.agents.on_event(event, &mut cx);
            self.council.on_event(event, &mut cx);
            self.questions.on_event(event, &mut cx);
            self.input.on_event(event, &mut cx);
            pull_request::on_event(event, &mut cx);
            if !unclaimed && !stale_tab && every_list.is_none() {
                views::on_event(event, &mut cx, &self.refresh);
            }
            status::sync(&mut cx);
            self.council.dress_working_line(&mut cx);
            panel::sync(&mut cx);
            band::sync(&mut cx);
        }
        if !self.state.replaying()
            && let Some(refresh) = self.refresh.on_event(event, now)
        {
            out.send(refresh);
        }
        // A take-over from history already ended; only a live one hands the terminal over.
        if let AppEvent::TakeOver { ticket, place, run } = event
            && !self.state.replaying()
        {
            out.take_over = Some(TakeOverJob {
                ticket: ticket.clone(),
                place: *place,
                run: run.clone(),
            });
        }
        if let AppEvent::RunInTerminal {
            argv,
            cwd,
            env_remove,
            env_set,
            quiet,
        } = event
            && !self.state.replaying()
        {
            out.run_in_terminal = Some(TerminalJob {
                argv: argv.clone(),
                cwd: cwd.clone(),
                env_remove: env_remove.clone(),
                env_set: env_set.clone(),
                quiet: *quiet,
            });
        }
        if self.state.replaying() {
            out.quiet_for_replay();
        }
        self.apply_local(&out);
        // Keys pressed until the next event meet the replay as it stands.
        self.state.beside_replay = false;
        out
    }

    pub fn on_action(&mut self, model: &mut ViewModel, action: &UiAction, now: Instant) -> Effects {
        let mut out = Effects::default();
        {
            let mut cx = Cx {
                model,
                state: &self.state,
                settings: &self.settings,
                now,
                out: &mut out,
            };
            if !matches!(action, UiAction::Focus(_)) {
                self.transcript.print_header(&mut cx);
            }
            let _taken = self.questions.on_action(action, &mut cx)
                || self.council.on_action(action, &mut cx)
                || self.input.on_action(action, &mut cx)
                || self.keys.on_action(action, &mut cx);
            status::sync(&mut cx);
            self.council.dress_working_line(&mut cx);
            panel::sync(&mut cx);
        }
        if let Some(refresh) = self.refresh.on_commands(&out.commands, now) {
            out.send(refresh);
        }
        self.apply_local(&out);
        out
    }

    fn apply_local(&mut self, out: &Effects) {
        for command in &out.commands {
            self.state.sent(command);
        }
        // Conversation text sent while a turn runs waits in grey until its own turn starts, and
        // `AppState::apply` drops it again at that `TurnStarted`.
        if matches!(self.state.mode, Some(AppMode::Answering { .. })) {
            for command in &out.commands {
                if let UiCommand::Submit { text, .. } = command {
                    self.state.conversation.queued.push(text.clone());
                }
            }
        }
        if let Some(level) = out.verbosity {
            self.settings.verbosity = level;
        }
        if let Some(on) = out.vim {
            self.settings.keys = if on {
                argonauts_core::KeyScheme::Vim
            } else {
                argonauts_core::KeyScheme::Default
            };
        }
    }
}

#[cfg(test)]
mod view_answer_tests {
    use argonauts_core::api::StatusSummary;
    use argonauts_core::input::SlashCommand;

    use super::*;

    fn status() -> AppEvent {
        AppEvent::View {
            data: Box::new(ViewData::Status {
                summary: StatusSummary::default(),
            }),
        }
    }

    /// The Status tab's own read, which `/status` typed opens the tab with.
    fn status_tab() -> AppEvent {
        AppEvent::View {
            data: Box::new(ViewData::StatusTab {
                view: Box::default(),
            }),
        }
    }

    fn submit(bridge: &mut Bridge, model: &mut ViewModel, text: &str) -> Effects {
        let action = UiAction::Submit {
            text: text.to_owned(),
            shown: text.to_owned(),
        };
        bridge.on_action(model, &action, Instant::now())
    }

    #[test]
    fn status_opens_its_view_and_where_are_we_prints_in_the_conversation() {
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let sent = submit(&mut bridge, &mut model, "/status");
        assert_eq!(
            sent.commands,
            [UiCommand::Slash {
                command: SlashCommand::Status {
                    action: argonauts_core::input::ViewAction::Open
                }
            }]
        );
        let printed = model.history.cells().len();
        let out = bridge.on_event(&mut model, &status_tab(), Instant::now());
        assert_eq!(out.views.len(), 1, "/status opens the status view");
        assert_eq!(model.history.cells().len(), printed);

        // The same answer to anything else, such as "where are we?", prints in the conversation.
        submit(&mut bridge, &mut model, "/ask where are we?");
        let out = bridge.on_event(&mut model, &status(), Instant::now());
        assert!(out.views.is_empty());
        assert!(model.history.cells().len() > printed);
    }

    #[test]
    fn status_print_prints_in_the_conversation_and_opens_nothing() {
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let sent = submit(&mut bridge, &mut model, "/status print");
        assert_eq!(
            sent.commands,
            [UiCommand::Slash {
                command: SlashCommand::Status {
                    action: argonauts_core::input::ViewAction::Print
                }
            }]
        );
        let printed = model.history.cells().len();
        let out = bridge.on_event(&mut model, &status(), Instant::now());
        assert!(out.views.is_empty());
        assert!(model.history.cells().len() > printed);
    }

    fn doctor_report() -> AppEvent {
        AppEvent::Report {
            title: "Doctor".into(),
            lines: vec!["claude 2.1.292".into()],
        }
    }

    /// An answer starts running, as the controller says it.
    fn answering(bridge: &mut Bridge, model: &mut ViewModel, now: Instant) {
        let turn = argonauts_core::names::TurnId::numbered(1);
        let mode = AppEvent::Mode {
            mode: AppMode::Answering { turn },
        };
        bridge.on_event(model, &mode, now);
    }

    fn command_rows(model: &ViewModel) -> Vec<String> {
        model
            .history
            .cells()
            .iter()
            .filter_map(|cell| match cell {
                crate::cell::Cell::User(message) if message.command => Some(message.text.clone()),
                _ => None,
            })
            .collect()
    }

    /// The rows `model` has printed since last asked, as plain text, and the colour of each
    /// row's last character.
    fn printed(model: &mut ViewModel) -> Vec<(String, Option<ratatui::style::Color>)> {
        let theme = crate::theme::Theme::dark();
        let ctx =
            crate::render::RenderCtx::new(100, &theme, crate::render::Clock::still(Instant::now()));
        model
            .history
            .drain_printable(&ctx)
            .iter()
            .map(|line| {
                let text = crate::text::plain_text(std::slice::from_ref(line));
                let last = line
                    .spans
                    .iter()
                    .rev()
                    .find(|span| !span.content.trim().is_empty());
                (
                    text.trim_end().to_owned(),
                    last.and_then(|span| span.style.fg),
                )
            })
            .filter(|(text, _)| !text.is_empty())
            .collect()
    }

    /// A `/config key=value` the app can't save prints why under the command, in the error
    /// colour, where a saved one prints where it went. The conversation's record keeps that line
    /// after the command, so a resumed conversation draws the two rows as they were.
    #[test]
    fn a_config_value_left_as_it_was_says_why_under_the_command() {
        use argonauts_core::app::ConversationEvent;

        let why = "ui.tips takes true, false, not \"maybe\"";
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let sent = submit(&mut bridge, &mut model, "/config ui.tips=maybe");
        assert_eq!(sent.typed.as_deref(), Some("/config ui.tips=maybe"));
        let refused = AppEvent::SettingRefused {
            key: "ui.tips".to_owned(),
            why: why.to_owned(),
        };
        let out = bridge.on_event(&mut model, &refused, Instant::now());
        let red = crate::theme::Theme::dark()
            .style(crate::theme::Role::Danger)
            .fg;
        let live = printed(&mut model);
        let texts: Vec<&str> = live.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(
            texts,
            ["❯ /config ui.tips=maybe", &format!("  ⎿  {why}")],
            "{live:?}"
        );
        assert_eq!(live[1].1, red, "{live:?}");
        assert_eq!(out.replies.len(), 1);
        assert!(out.replies[0].error, "{:?}", out.replies);

        let mut resumed = Bridge::new(&UiOptions::default());
        let mut past = ViewModel::default();
        let now = Instant::now();
        resumed.on_event(&mut past, &AppEvent::PastBegins, now);
        for event in [
            ConversationEvent::CommandTyped {
                text: "/config ui.tips=maybe".to_owned(),
                at: None,
            },
            ConversationEvent::CommandReplied {
                reply: out.replies[0].clone(),
            },
        ] {
            resumed.on_event(&mut past, &AppEvent::Conversation { event }, now);
        }
        resumed.on_event(&mut past, &AppEvent::PastDrawn, now);
        assert_eq!(printed(&mut past), live);
    }

    /// What a command typed mid-answer prints starts with a row of its own, so it never reads
    /// as part of the answer.
    #[test]
    fn a_command_typed_mid_answer_prints_under_its_own_row() {
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let now = Instant::now();
        answering(&mut bridge, &mut model, now);
        submit(&mut bridge, &mut model, "/doctor");
        bridge.on_event(&mut model, &doctor_report(), now);
        assert_eq!(command_rows(&model), ["/doctor"]);

        // The row is for what the command printed, once.
        bridge.on_event(&mut model, &doctor_report(), now);
        assert_eq!(command_rows(&model), ["/doctor"]);
    }

    #[test]
    fn a_command_typed_while_idle_or_one_that_opened_a_view_gets_no_row() {
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let now = Instant::now();
        submit(&mut bridge, &mut model, "/doctor");
        bridge.on_event(&mut model, &doctor_report(), now);
        assert!(command_rows(&model).is_empty(), "idle, it prints as before");

        answering(&mut bridge, &mut model, now);
        submit(&mut bridge, &mut model, "/status");
        let out = bridge.on_event(&mut model, &status_tab(), now);
        assert_eq!(out.views.len(), 1);
        assert!(out.views_as_panel, "it opens under the working line");
        bridge.on_event(&mut model, &doctor_report(), now);
        assert!(
            command_rows(&model).is_empty(),
            "/status opened its view, so a later report isn't its"
        );
    }

    /// `/usage` opens its tab at once, here, for the period it names, and the controller is
    /// asked for the numbers. A word it will refuse opens nothing.
    #[test]
    fn usage_opens_its_tab_of_the_overview_at_once() {
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let sent = submit(&mut bridge, &mut model, "/usage weekly");
        assert_eq!(sent.views.len(), 1);
        assert_eq!(
            sent.commands,
            [UiCommand::Slash {
                command: SlashCommand::Usage {
                    period: Some("weekly".into()),
                    action: argonauts_core::input::ViewAction::Open
                }
            }]
        );
        let refused = submit(&mut bridge, &mut model, "/usage yearly");
        assert!(refused.views.is_empty());
        assert_eq!(refused.commands.len(), 1, "the controller says why");

        // The numbers land in the state, for the tab and the cards above the input, and
        // open nothing by themselves.
        let usage = AppEvent::View {
            data: Box::new(ViewData::Usage {
                summary: Box::default(),
            }),
        };
        let out = bridge.on_event(&mut model, &usage, Instant::now());
        assert!(out.views.is_empty());
        assert!(bridge.state.usage.is_some());
        assert!(model.bottom.band.usage.is_some());
    }

    /// `/limits` alone opens its tab, with the usage numbers asked for. With a key and a
    /// value it goes to the controller, which sets that limit.
    #[test]
    fn limits_alone_opens_its_tab_and_a_limit_to_set_goes_to_the_controller() {
        use argonauts_core::input::ViewAction;

        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let sent = submit(&mut bridge, &mut model, "/limits");
        assert_eq!(sent.views.len(), 1);
        assert_eq!(
            sent.commands,
            [UiCommand::Slash {
                command: SlashCommand::Usage {
                    period: Some("all".to_owned()),
                    action: ViewAction::Open
                }
            }]
        );
        let keyed = submit(&mut bridge, &mut model, "/limits per_task_usd 5");
        assert!(keyed.views.is_empty());
        assert!(
            matches!(
                keyed.commands[..],
                [UiCommand::Slash {
                    command: SlashCommand::Limits { key: Some(_), .. }
                }]
            ),
            "{:?}",
            keyed.commands
        );
    }

    /// `/stats` opens the Stats tab at once and asks for the numbers it draws; pinned, it
    /// goes to the controller, which pins its card above the input.
    #[test]
    fn stats_opens_its_tab_and_pin_goes_to_the_controller() {
        use argonauts_core::input::ViewAction;

        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let sent = submit(&mut bridge, &mut model, "/stats week");
        assert_eq!(sent.views.len(), 1);
        assert_eq!(
            sent.commands,
            [UiCommand::Slash {
                command: SlashCommand::Stats {
                    period: Some("week".into()),
                    action: ViewAction::Open
                }
            }]
        );
        let pinned = submit(&mut bridge, &mut model, "/stats pin");
        assert!(pinned.views.is_empty());
        assert_eq!(
            pinned.commands,
            [UiCommand::Slash {
                command: SlashCommand::Stats {
                    period: None,
                    action: ViewAction::Pin { scope: None }
                }
            }]
        );
    }

    /// `/mods stats` opens the stats mod's page over the Stats tab, which asks for the
    /// numbers its preview draws. Another mod's page is still the controller's.
    #[test]
    fn mods_stats_opens_the_page_over_the_stats_tab() {
        use argonauts_core::input::{ModsAction, ViewAction};
        use argonauts_core::settings::ui::ModName;

        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let sent = submit(&mut bridge, &mut model, "/mods stats");
        assert_eq!(sent.views.len(), 1);
        assert_eq!(
            sent.commands,
            [UiCommand::Slash {
                command: SlashCommand::Stats {
                    period: None,
                    action: ViewAction::Open
                }
            }]
        );
        let other = submit(&mut bridge, &mut model, "/mods context");
        assert!(other.views.is_empty());
        assert_eq!(
            other.commands,
            [UiCommand::Slash {
                command: SlashCommand::Mods {
                    action: ModsAction::Page {
                        the_mod: ModName::Context
                    },
                    scope: None
                }
            }]
        );
    }

    /// A tab set to print (`ui.views.<tab>.open = "print"`) prints when its command is
    /// typed alone, as the command's `print` does, and opens nothing.
    #[test]
    fn a_tab_set_to_print_prints_when_its_command_is_typed() {
        use argonauts_core::input::ViewAction;
        use argonauts_core::settings::ui::{UiSettings, ViewOpen};

        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let mut ui = UiSettings::default();
        ui.views.status.open = ViewOpen::Print;
        ui.views.usage.open = ViewOpen::Print;
        ui.views.limits.open = ViewOpen::Print;
        let settings = AppEvent::UiSettings {
            vim: false,
            verbosity: argonauts_core::Verbosity::Full,
            ui: Some(Box::new(ui)),
        };
        bridge.on_event(&mut model, &settings, Instant::now());
        let sent = |bridge: &mut Bridge, model: &mut ViewModel, typed: &str| {
            let out = submit(bridge, model, typed);
            assert!(out.views.is_empty(), "{typed} opens nothing here");
            match &out.commands[..] {
                [UiCommand::Slash { command }] => command.clone(),
                other => panic!("{typed}: {other:?}"),
            }
        };
        let print = ViewAction::Print;
        assert_eq!(
            sent(&mut bridge, &mut model, "/status"),
            SlashCommand::Status { action: print }
        );
        // Its answer is a report, so no view waits for a status answer.
        assert!(!bridge.input.status_view);
        assert_eq!(
            sent(&mut bridge, &mut model, "/usage week"),
            SlashCommand::Usage {
                period: Some("week".into()),
                action: print
            }
        );
        assert_eq!(
            sent(&mut bridge, &mut model, "/limits"),
            SlashCommand::Limits {
                key: None,
                value: None,
                action: print
            }
        );
        // A tab left as it was opens its panel.
        assert_eq!(
            sent(&mut bridge, &mut model, "/context"),
            SlashCommand::Context {
                all: false,
                action: ViewAction::Open
            }
        );
    }

    /// `/context all` opens the Context tab with every list open, where a plain `/context`
    /// opens it folded.
    #[test]
    fn context_all_opens_every_list() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        use crate::overview::sample;
        use crate::views::ViewCtx;

        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        bridge.state.conversation.context = Some(sample::counted());
        let answer = AppEvent::View {
            data: Box::new(ViewData::Context {
                view: sample::context(),
            }),
        };
        let mut shown = |bridge: &mut Bridge, typed: &str| {
            submit(bridge, &mut model, typed);
            let mut out = bridge.on_event(&mut model, &answer, Instant::now());
            assert_eq!(out.views.len(), 1, "{typed} opens one tab");
            let theme = crate::Theme::dark();
            let ctx = ViewCtx {
                theme: &theme,
                state: &bridge.state,
                now: Instant::now(),
            };
            let area = Rect::new(0, 0, 118, 90);
            let mut buf = Buffer::empty(area);
            out.views[0].render(area, &mut buf, &ctx);
            let rows = crate::views::dock::lines(&buf);
            let rows = rows.iter().map(ToString::to_string);
            // The count's age is the only thing a later frame draws differently.
            rows.filter(|row| !row.contains("counted "))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let all = shown(&mut bridge, "/context all");
        assert!(all.contains("Skills · /skills"), "{all}");
        assert!(!all.contains("/context all to expand"), "{all}");
        let folded = shown(&mut bridge, "/context");
        assert!(folded.contains("/context all to expand"), "{folded}");
        assert_ne!(folded, all);
    }

    /// `/context print` answers with the sessions, and the conversation prints them under the
    /// command's own row, with the main session's count as the screen holds it. Nothing opens.
    #[test]
    fn context_print_goes_into_the_conversation() {
        use crate::cell::Cell;
        use crate::overview::sample;

        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        bridge.state.conversation.context = Some(sample::counted());
        let printed = model.history.cells().len();
        let answer = AppEvent::View {
            data: Box::new(ViewData::ContextPrint {
                view: sample::context(),
            }),
        };
        let out = bridge.on_event(&mut model, &answer, Instant::now());
        assert!(out.views.is_empty());
        let cells = &model.history.cells()[printed..];
        let [Cell::User(command), Cell::Custom(rows)] = cells else {
            panic!("{cells:?}");
        };
        assert!(command.command);
        assert_eq!(command.text, "/context print");
        let theme = crate::Theme::dark();
        let clock = crate::render::Clock::still(Instant::now());
        let ctx = crate::render::RenderCtx::new(118, &theme, clock);
        let text = crate::text::plain_text(&rows.lines(&ctx));
        assert!(text.starts_with("  ⎿  Context Usage\n"), "{text}");
        assert!(text.contains("212.2k/1m tokens (21%)"), "{text}");
        assert!(text.contains("Crew agents \u{b7} "), "{text}");
    }

    /// The usage panel asks again for what its open tab shows. Once the panel has closed, the
    /// answer still on its way opens nothing and prints nothing, and the next `/status`
    /// opens its tab as ever.
    #[test]
    fn a_refresh_answered_after_the_panel_closed_shows_nothing() {
        use std::time::Duration;

        use argonauts_core::app::ViewRequest;

        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let t0 = Instant::now();
        let watch = model.bottom.band.watch.clone();
        let every = Duration::from_secs(1);
        watch.drawn_tab(ViewRequest::StatusTab, every, t0);
        assert_eq!(watch.due(t0 + every), Some(ViewRequest::StatusTab));
        watch.drawn_tab(ViewRequest::Context, every, t0);
        assert_eq!(watch.due(t0 + every), Some(ViewRequest::Context));

        let printed = model.history.cells().len();
        let out = bridge.on_event(&mut model, &status_tab(), t0 + every);
        assert!(out.views.is_empty());
        assert_eq!(model.history.cells().len(), printed);
        let context = AppEvent::View {
            data: Box::new(ViewData::Context {
                view: argonauts_core::views::ContextView::default(),
            }),
        };
        let out = bridge.on_event(&mut model, &context, t0 + every);
        assert!(out.views.is_empty(), "no Context tab opens by itself");

        submit(&mut bridge, &mut model, "/status");
        let out = bridge.on_event(&mut model, &status_tab(), t0 + every);
        assert_eq!(out.views.len(), 1);
        let out = bridge.on_event(&mut model, &context, t0 + every);
        assert_eq!(out.views.len(), 1, "an answer you asked for opens its tab");
    }

    /// Whatever a tab of the usage panel is answered with, nothing of it reaches the conversation,
    /// even with nothing to show: no run, no readings.
    #[test]
    fn no_tab_of_the_panel_prints_into_the_conversation() {
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let now = Instant::now();
        let printed = model.history.cells().len();
        let usage = |period| AppEvent::View {
            data: Box::new(ViewData::Usage {
                summary: Box::new(argonauts_core::api::UsageSummary {
                    period,
                    ..Default::default()
                }),
            }),
        };
        let context = AppEvent::View {
            data: Box::new(ViewData::Context {
                view: argonauts_core::views::ContextView::default(),
            }),
        };
        let mut answers = vec![status_tab(), context];
        answers.extend(
            [
                argonauts_core::api::UsagePeriod::Today,
                argonauts_core::api::UsagePeriod::Week,
                argonauts_core::api::UsagePeriod::All,
            ]
            .map(usage),
        );
        for answer in answers.iter().chain(&answers) {
            bridge.on_event(&mut model, answer, now);
        }
        assert_eq!(model.history.cells().len(), printed);
        // `/status` typed opens the tab with the same answer, and prints nothing either.
        submit(&mut bridge, &mut model, "/status");
        let typed = model.history.cells().len();
        let out = bridge.on_event(&mut model, &status_tab(), now);
        assert_eq!(out.views.len(), 1);
        assert_eq!(model.history.cells().len(), typed);
    }

    /// A view opened while nothing runs, or by anything but a command typed mid-answer,
    /// takes the screen.
    #[test]
    fn only_a_command_typed_mid_answer_opens_a_panel() {
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let now = Instant::now();
        submit(&mut bridge, &mut model, "/status");
        assert!(
            !bridge
                .on_event(&mut model, &status_tab(), now)
                .views_as_panel
        );

        answering(&mut bridge, &mut model, now);
        let library = AppEvent::View {
            data: Box::new(ViewData::WorkflowLibrary {
                workflows: Vec::new(),
                refresh: false,
            }),
        };
        let out = bridge.on_event(&mut model, &library, now);
        assert_eq!(out.views.len(), 1);
        assert!(!out.views_as_panel, "nothing was typed for it");

        assert!(submit(&mut bridge, &mut model, "/help").views_as_panel);
    }

    #[test]
    fn ctrl_c_knows_while_a_shell_command_runs() {
        use argonauts_core::app::ShellEvent;

        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let shell = |event| AppEvent::Shell { event };
        bridge.on_event(
            &mut model,
            &shell(ShellEvent::Started {
                id: 3,
                command: "sleep 25".into(),
            }),
            Instant::now(),
        );
        assert_eq!(bridge.state.shell, Some(3));
        assert!(model.bottom.shell_running);
        bridge.on_event(
            &mut model,
            &shell(ShellEvent::Finished {
                id: 3,
                exit_code: None,
                stopped: true,
            }),
            Instant::now(),
        );
        assert_eq!(bridge.state.shell, None);
        assert!(!model.bottom.shell_running);
    }

    #[test]
    fn a_library_refresh_opens_nothing_once_its_tab_closed() {
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let library = |refresh| AppEvent::View {
            data: Box::new(ViewData::WorkflowLibrary {
                workflows: Vec::new(),
                refresh,
            }),
        };
        let out = bridge.on_event(&mut model, &library(true), Instant::now());
        assert!(out.views.is_empty());
        let out = bridge.on_event(&mut model, &library(false), Instant::now());
        assert_eq!(out.views.len(), 1);
    }

    #[test]
    fn an_agent_clis_own_command_gets_the_terminal() {
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let login = AppEvent::RunInTerminal {
            argv: vec!["codex".to_owned(), "login".to_owned()],
            cwd: PathBuf::from("/repo"),
            env_remove: vec!["CLAUDECODE".to_owned()],
            env_set: Vec::new(),
            quiet: false,
        };
        let out = bridge.on_event(&mut model, &login, Instant::now());
        assert_eq!(
            out.run_in_terminal,
            Some(TerminalJob {
                argv: vec!["codex".to_owned(), "login".to_owned()],
                cwd: PathBuf::from("/repo"),
                env_remove: vec!["CLAUDECODE".to_owned()],
                env_set: Vec::new(),
                quiet: false,
            })
        );
        assert!(out.commands.is_empty(), "nothing goes back when it ends");
    }
}

#[cfg(test)]
mod awake_tests {
    use argonauts_core::app::{AwakeHold, HeaderAndBottomLine};
    use argonauts_core::settings::approvals::ApprovalMode;
    use argonauts_core::settings::bottom_line::AWAKE;
    use argonauts_core::settings::ui::NarratorMode;

    use super::*;
    use crate::bottom::{AddedPart, LinePart, PartBody};
    use crate::cell::Tone;

    fn status(awake: Option<AwakeHold>) -> AppEvent {
        AppEvent::Status {
            status: HeaderAndBottomLine {
                title: String::new(),
                repo: "~/code".into(),
                workflow: "council".into(),
                ultracode: false,
                ultra: None,
                run: None,
                main_agent: "claude-code".parse().unwrap(),
                plan_chips: Vec::new(),
                line: Default::default(),
                spent: None,
                limit: None,
                narrator: NarratorMode::Off,
                approval_mode: ApprovalMode::Supervised,
                awake,
            },
        }
    }

    fn awake_part(model: &ViewModel) -> Option<PartBody> {
        let line = &model.bottom.line;
        line.left
            .iter()
            .chain(&line.right)
            .find_map(|part| match part {
                LinePart::Added(AddedPart { name, body }) if name == AWAKE => Some(body.clone()),
                _ => None,
            })
    }

    /// `/awake` holds the computer's sleep off, and the bottom line names the hold through
    /// its `awake` part while it lasts.
    #[test]
    fn a_hold_on_sleep_reaches_the_bottom_line_while_it_lasts() {
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        bridge.on_event(
            &mut model,
            &status(Some(AwakeHold::UntilQuit)),
            Instant::now(),
        );
        assert_eq!(
            awake_part(&model),
            Some(PartBody::Words {
                text: "☕ awake until you quit".into(),
                tone: Tone::Code,
            })
        );
        bridge.on_event(&mut model, &status(None), Instant::now());
        assert_eq!(awake_part(&model), None);
    }
}
