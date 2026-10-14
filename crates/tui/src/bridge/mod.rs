//! Connects the screen to the app controller. Every `AppEvent` is folded into [`AppState`] and
//! then offered to each part in turn, and every `UiAction` the screen reports is offered until
//! one part takes it. Parts never draw directly: they change the view model and ask for
//! commands, views, and the bell through [`Effects`].

pub mod agents;
mod band;
pub mod bottom_line;
pub mod failure;
pub mod input;
pub mod keys;
pub mod mods;
pub mod panel;
pub mod settings;
pub mod status;
pub mod tools;
pub mod transcript;
pub mod vendor_workflow;

use std::path::PathBuf;
use std::time::Instant;

use argonauts_core::RunId;
use argonauts_core::Verbosity;
use argonauts_core::api::TakeOverTicket;
use argonauts_core::app::{AppEvent, AppMode, UiCommand, ViewData};
use argonauts_core::keywords::Keyword;
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::ui::{TakeoverWhere, UiSettings};

use crate::UiOptions;
use crate::app::UiAction;
use crate::cards::Cards;
use crate::model::ViewModel;
use crate::overview::{OverviewView, Tab};
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

    pub fn replaying(&self) -> bool {
        self.state.replaying()
    }
}

impl<'a> Cx<'a> {
    /// The main agent CLI's keywords.
    pub fn keywords(&self) -> &'a [Keyword] {
        let state: &'a AppState = self.state;
        state
            .vendor_commands
            .as_ref()
            .map_or(&[], |set| set.keywords.as_slice())
    }

    /// `worker`'s keywords when it runs the main agent's CLI, the one whose table the screen
    /// has; none otherwise.
    pub fn keywords_of(&self, worker: &WorkerRef) -> &'a [Keyword] {
        let state: &'a AppState = self.state;
        match &state.vendor_commands {
            Some(set) if set.vendor.0 == worker.worker => &set.keywords,
            _ => &[],
        }
    }
}

/// What the runtime does after an event or action: send commands, ring the bell, open views,
/// hand the terminal over, or leave.
#[derive(Debug, Default)]
pub struct Effects {
    pub commands: Vec<UiCommand>,
    pub bell: bool,
    /// Pushed on the view stack in this order.
    pub views: Vec<Box<dyn View>>,
    /// The views answer a command typed while an answer ran, so they open as a panel under
    /// the working line and the answer keeps running above.
    pub views_as_panel: bool,
    /// Close every open view first, such as when a card from the crew view gets the keys.
    pub close_views: bool,
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
    pub place: TakeoverWhere,
    pub run: RunId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalJob {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
}

#[derive(Debug)]
pub struct Bridge {
    pub state: AppState,
    settings: UiSettings,
    pub transcript: Transcript,
    pub agents: AgentBlocks,
    pub cards: Cards,
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
            cards: Cards::default(),
            input: InputRouter::new(options),
            keys: KeyActions::default(),
            refresh: Refresh::default(),
        }
    }

    pub fn settings(&self) -> &UiSettings {
        &self.settings
    }

    pub fn on_event(&mut self, model: &mut ViewModel, event: &AppEvent, now: Instant) -> Effects {
        self.state.apply(event);
        if let AppEvent::UiSettings { ui: Some(ui), .. } = event {
            self.settings = (**ui).clone();
        }
        let mut out = Effects::default();
        // A refresh's answer that no view took is for a view that closed or is covered.
        let unclaimed = self.refresh.unclaimed(event, now);
        // So is the answer to a refresh of the Overview's open tab, once the panel has
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
        // `/context all` asked for the tab its answer opens to list every item.
        let context_all = match event {
            AppEvent::View { data } if !self.state.replaying() && !stale_tab => {
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
            match status_view {
                // Under its tab of the Overview, beside Usage and Context.
                Some(view) => cx.out.open(
                    OverviewView::new(Tab::Status, cx.model.bottom.band.watch.clone())
                        .with_status_tab((**view).clone()),
                ),
                None if stale_tab => {}
                None => self.transcript.on_event(event, &mut cx),
            }
            self.agents.on_event(event, &mut cx);
            self.cards.on_event(event, &mut cx);
            self.input.on_event(event, &mut cx);
            match context_all {
                Some(view) => cx.out.open(
                    OverviewView::new(Tab::Context, cx.model.bottom.band.watch.clone())
                        .with_context(view.clone())
                        .listing_all(true),
                ),
                None if !unclaimed && !stale_tab => views::on_event(event, &mut cx, &self.refresh),
                None => {}
            }
            status::sync(&mut cx);
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
        if let AppEvent::RunInTerminal { argv, cwd } = event
            && !self.state.replaying()
        {
            out.run_in_terminal = Some(TerminalJob {
                argv: argv.clone(),
                cwd: cwd.clone(),
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
            let _taken = self.cards.on_action(action, &mut cx)
                || self.input.on_action(action, &mut cx)
                || self.keys.on_action(action, &mut cx);
            status::sync(&mut cx);
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
        // Chat text sent while a turn runs waits in grey until its own turn starts, and
        // `AppState::apply` drops it again at that `TurnStarted`.
        if matches!(self.state.mode, Some(AppMode::Answering { .. })) {
            for command in &out.commands {
                if let UiCommand::Submit { text, .. } = command {
                    self.state.chat.queued.push(text.clone());
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
                argonauts_core::KeyScheme::Standard
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
    fn status_opens_its_view_and_where_are_we_prints_in_the_chat() {
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

        // The same answer to anything else, such as "where are we?", prints in the chat.
        submit(&mut bridge, &mut model, "/ask where are we?");
        let out = bridge.on_event(&mut model, &status(), Instant::now());
        assert!(out.views.is_empty());
        assert!(model.history.cells().len() > printed);
    }

    #[test]
    fn status_print_prints_in_the_chat_and_opens_nothing() {
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
                    period: None,
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
                    command: SlashCommand::Budget { key: Some(_), .. }
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
        let sent = submit(&mut bridge, &mut model, "/stats 7d");
        assert_eq!(sent.views.len(), 1);
        assert_eq!(
            sent.commands,
            [UiCommand::Slash {
                command: SlashCommand::Stats {
                    period: Some("7d".into()),
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
        use argonauts_core::input::{ViewAction, WidgetsAction};
        use argonauts_core::settings::ui::WidgetName;

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
                command: SlashCommand::Widgets {
                    action: WidgetsAction::Page {
                        widget: WidgetName::Context
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
        ui.views.context.open = ViewOpen::Dialog;
        ui.views.budget.open = ViewOpen::Print;
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
            SlashCommand::Budget {
                key: None,
                value: None,
                action: print
            }
        );
        // A dialog is the panel, as a view is.
        assert_eq!(
            sent(&mut bridge, &mut model, "/context"),
            SlashCommand::Context {
                all: false,
                action: ViewAction::Open
            }
        );
    }

    /// `/context all` opens the Context tab listing every item of every category, and a
    /// plain `/context` after it opens the tab with the largest few as ever.
    #[test]
    fn context_all_opens_the_tab_listing_every_item() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        use crate::overview::sample;
        use crate::views::ViewCtx;

        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        bridge.state.chat.context = Some(sample::breakdown());
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
            let area = Rect::new(0, 0, 110, 60);
            let mut buf = Buffer::empty(area);
            out.views[0].render(area, &mut buf, &ctx);
            crate::views::dock::lines(&buf)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let all = shown(&mut bridge, "/context all");
        assert!(all.contains("Memory files · 2 · 4.9k"), "{all}");
        assert!(all.contains("a the largest items"), "{all}");
        let few = shown(&mut bridge, "/context");
        assert!(!few.contains("Memory files · 2 · 4.9k"), "{few}");
        assert!(few.contains("a every item"), "{few}");
    }

    /// The Overview asks again for what its open tab shows. Once the panel has closed, the
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
                view: argonauts_core::views::ContextView { rows: Vec::new() },
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

    /// Whatever a tab of the Overview is answered with, nothing of it reaches the chat, even
    /// with nothing to show: no run, no readings.
    #[test]
    fn no_tab_of_the_panel_prints_into_the_chat() {
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
                view: argonauts_core::views::ContextView { rows: Vec::new() },
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
        };
        let out = bridge.on_event(&mut model, &login, Instant::now());
        assert_eq!(
            out.run_in_terminal,
            Some(TerminalJob {
                argv: vec!["codex".to_owned(), "login".to_owned()],
                cwd: PathBuf::from("/repo"),
            })
        );
        assert!(out.commands.is_empty(), "nothing goes back when it ends");
    }
}

#[cfg(test)]
mod awake_tests {
    use argonauts_core::app::{AwakeHold, StatusLine};
    use argonauts_core::settings::approvals::ApprovalMode;
    use argonauts_core::settings::bottom_line::AWAKE;
    use argonauts_core::settings::ui::NarratorMode;

    use super::*;
    use crate::bottom::{AddedPart, LinePart, PartBody};
    use crate::cell::Tone;

    fn status(awake: Option<AwakeHold>) -> AppEvent {
        AppEvent::Status {
            status: StatusLine {
                title: String::new(),
                repo: "~/code".into(),
                workflow: "council".into(),
                ultracode: false,
                run: None,
                main_agent: "claude-code".parse().unwrap(),
                quotas: Vec::new(),
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
