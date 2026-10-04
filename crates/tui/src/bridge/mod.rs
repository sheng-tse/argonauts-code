//! Connects the screen to the app controller. Every `AppEvent` is folded into [`AppState`] and
//! then offered to each part in turn, and every `UiAction` the screen reports is offered until
//! one part takes it. Parts never draw directly: they change the view model and ask for
//! commands, views, and the bell through [`Effects`].

pub mod agents;
pub mod input;
pub mod keys;
pub mod mods;
pub mod panel;
pub mod settings;
pub mod status;
pub mod tools;
pub mod transcript;

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
use crate::state::AppState;
use crate::theme::Theme;
use crate::views::follow::Refresh;
use crate::views::status::StatusView;
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
    /// Close every open view first, such as when a card from the crew view gets the keys.
    pub close_views: bool,
    pub take_over: Option<TakeOverJob>,
    /// An agent CLI's own command, such as its login, to run with the terminal handed over.
    pub run_in_terminal: Option<TerminalJob>,
    /// Text for the system clipboard, from `/copy` or a selection.
    pub clipboard: Option<String>,
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
        let status_view = match event {
            AppEvent::View { data } if !self.state.replaying() => match data.as_ref() {
                ViewData::Status { summary } if std::mem::take(&mut self.input.status_view) => {
                    Some(summary)
                }
                _ => None,
            },
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
            match status_view {
                Some(summary) => cx.out.open(StatusView::new(summary.clone())),
                None => self.transcript.on_event(event, &mut cx),
            }
            self.agents.on_event(event, &mut cx);
            self.cards.on_event(event, &mut cx);
            self.input.on_event(event, &mut cx);
            if !unclaimed {
                views::on_event(event, &mut cx, &self.refresh);
            }
            status::sync(&mut cx);
            panel::sync(&mut cx);
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
                if let UiCommand::Submit { text } = command {
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
                command: SlashCommand::Status
            }]
        );
        let printed = model.history.cells().len();
        let out = bridge.on_event(&mut model, &status(), Instant::now());
        assert_eq!(out.views.len(), 1, "/status opens the status view");
        assert_eq!(model.history.cells().len(), printed);

        // The same answer to anything else, such as "where are we?", prints in the chat.
        submit(&mut bridge, &mut model, "/ask where are we?");
        let out = bridge.on_event(&mut model, &status(), Instant::now());
        assert!(out.views.is_empty());
        assert!(model.history.cells().len() > printed);
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
