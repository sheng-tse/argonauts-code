//! What you type: plain text, `/commands`, and `@mentions`, routed by the app mode. Text typed
//! at the prompt never answers a card; see [`typed_text_answers_card`].

use std::time::Duration;

use argonauts_core::app::{AppEvent, AppMode, UiCommand, ViewRequest};
use argonauts_core::events::run::Party;
use argonauts_core::input::{Handler, MentionTarget, ParsedInput, SlashCommand, parse_input_with};
use argonauts_core::{KeyScheme, Verbosity};

use crate::UiOptions;
use crate::app::UiAction;
use crate::bottom::{BottomState, HistorySearch, ModeNotice, VimMode};
use crate::cell::{Cell, Tone, UserMessage};
use crate::commands::{self, command_items};
use crate::keymap::Keymap;
use crate::mentions::{self, MentionPicker, Resolved};
use crate::prompt_history::{PromptHistory, unix_now};
use crate::views::help::HelpView;

use super::Cx;

/// How long a notice under the input stays up.
const NOTICE_FOR: Duration = Duration::from_secs(6);

#[derive(Debug, Default)]
pub struct InputRouter {
    pub history: PromptHistory,
    /// Why `history.jsonl` couldn't be read, shown once under the input.
    load_error: Option<String>,
    /// The composer has the saved history and the `/` menu has its commands.
    prepared: bool,
    /// `/status` ran, so its answer opens the status view rather than printing in the chat as
    /// "where are we?" does. The next thing you send clears it.
    pub status_view: bool,
}

impl InputRouter {
    /// Reads the prompt history now; a file that can't be read leaves history in memory and
    /// says why on the first event.
    pub fn new(options: &UiOptions) -> Self {
        let Some(file) = &options.history_file else {
            return Self::default();
        };
        match PromptHistory::load(file) {
            Ok(history) => Self {
                history,
                ..Self::default()
            },
            Err(error) => Self {
                history: PromptHistory::unread(file),
                load_error: Some(error.to_string()),
                prepared: false,
                status_view: false,
            },
        }
    }

    /// Gives the composer the saved history, ahead of anything typed already, and the `/`
    /// menu its commands. The first event or action does it too; call it at start so ↑ works
    /// before either.
    pub fn prepare(&mut self, bottom: &mut BottomState) {
        if self.prepared {
            return;
        }
        self.prepared = true;
        let typed = bottom.composer.history().to_vec();
        bottom
            .composer
            .set_history(self.history.texts().map(str::to_owned).chain(typed));
        if bottom.menus.commands.is_empty() {
            bottom.menus.commands = command_items(None);
        }
    }

    /// `TakenBack` refills the input, and run events refresh the `@` list.
    pub fn on_event(&mut self, event: &AppEvent, cx: &mut Cx<'_>) {
        self.prepare_with(cx);
        match event {
            // The composer drops it again at the first prompt.
            AppEvent::Welcome { example_prompt, .. } => {
                cx.model.bottom.composer.example.clone_from(example_prompt);
            }
            AppEvent::TakenBack { texts } => {
                cx.model.bottom.composer.restore(texts);
                cx.model.bottom.menus.text_changed();
            }
            AppEvent::VendorCommands { set } => {
                cx.model.bottom.menus.commands = command_items(Some(set));
                cx.model.bottom.composer.keywords.clone_from(&set.keywords);
            }
            AppEvent::RepoFiles { paths } => cx.model.bottom.menus.set_files(paths),
            AppEvent::Attached { .. } | AppEvent::Detached { .. } | AppEvent::Run { .. } => {
                let mentions = mentions::candidates(cx.state);
                if cx.model.bottom.menus.mentions != mentions {
                    cx.model.bottom.menus.mentions = mentions;
                }
            }
            _ => {}
        }
    }

    /// Takes `Submit` (parsed with `parse_input_with` and the main agent's commands into `Submit`, `Slash`, or `Mention`, or run
    /// here for `Ui` and `Both` commands), `TakeBack`, `OpenHelp`, and `SearchHistory`.
    pub fn on_action(&mut self, action: &UiAction, cx: &mut Cx<'_>) -> bool {
        self.prepare_with(cx);
        match action {
            UiAction::Submit { text, shown } => self.submit(text, shown, cx),
            UiAction::Shell { command } => self.shell(command, cx),
            UiAction::TakeBack(_) => take_back(cx),
            UiAction::OpenHelp => open_help(cx),
            UiAction::SearchHistory => self.open_search(cx),
            _ => return false,
        }
        true
    }

    /// This repo's prompts and every repo's, read now, so a prompt sent in another window
    /// since start is there too.
    fn open_search(&mut self, cx: &mut Cx<'_>) {
        let bottom = &mut cx.model.bottom;
        let search = HistorySearch::new(
            self.history.this_project(),
            self.history.everywhere(),
            unix_now(),
        );
        // Esc goes back to the mode the search opened from, and only NORMAL reaches the `/`
        // commands by Esc i /.
        let from_normal = bottom.mode.vim == Some(VimMode::Normal);
        bottom.history_search = Some(search.with_vim(from_normal));
    }

    fn prepare_with(&mut self, cx: &mut Cx<'_>) {
        if self.prepared {
            return;
        }
        self.prepare(&mut cx.model.bottom);
        if let Some(error) = self.load_error.take() {
            notice(cx, error, Tone::Error);
        }
    }

    fn submit(&mut self, text: &str, shown: &str, cx: &mut Cx<'_>) {
        // Typed alone, these leave as `/exit` does, as in Claude Code, instead of going to the
        // agent as a message.
        if matches!(text.trim(), "exit" | "quit") {
            cx.out.exit = true;
            return;
        }
        if let Err(error) = self.history.append(text) {
            notice(cx, error.to_string(), Tone::Error);
        }
        let parsed = parse_input_with(text, cx.state.vendor_commands.as_ref());
        self.status_view = matches!(
            parsed,
            Ok(ParsedInput::Slash {
                command: SlashCommand::Status
            })
        );
        match parsed {
            Ok(ParsedInput::Empty) => empty_enter(cx),
            Ok(ParsedInput::Plain { text }) => plain(text, shown, cx),
            Ok(ParsedInput::Mention { .. }) if cx.model.bottom.menus.leads_with_file(text) => {
                plain(text.trim().to_owned(), shown, cx);
            }
            Ok(ParsedInput::Mention {
                target,
                text: message,
            }) => mention(target, message, text, cx),
            Ok(ParsedInput::Slash { command }) => slash(command, text, cx),
            Err(error) => {
                notice(cx, error.to_string(), Tone::Error);
                give_back(text, cx);
            }
        }
    }

    /// Saved with its `!`, so ↑ in a later session brings it back in shell mode.
    fn shell(&mut self, command: &str, cx: &mut Cx<'_>) {
        if let Err(error) = self.history.append(&format!("!{command}")) {
            notice(cx, error.to_string(), Tone::Error);
        }
        cx.out.send(UiCommand::Shell {
            command: command.to_owned(),
        });
    }
}

/// The one place that decides whether text typed at the prompt can answer a focused card.
/// It never can; notes go through Tab. Flip this to change the rule.
pub fn typed_text_answers_card() -> bool {
    false
}

/// Enter on an empty prompt cancels a pause or resumes a parked run; the controller decides.
fn empty_enter(cx: &mut Cx<'_>) {
    if matches!(
        cx.state.mode,
        Some(AppMode::Pausing { .. } | AppMode::Parked { .. })
    ) {
        cx.out.send(UiCommand::Submit {
            text: String::new(),
        });
    }
}

/// Plain text goes by the app mode, whatever card waits (see [`typed_text_answers_card`]).
/// With nothing going, it shows as your message at once, grey until its turn starts and prints
/// it; otherwise it shows grey from state until it's delivered.
fn plain(text: String, shown: &str, cx: &mut Cx<'_>) {
    let idle = matches!(cx.state.mode, None | Some(AppMode::Idle)) && !cx.model.is_working();
    if idle {
        let keywords = cx.keywords();
        let history = &mut cx.model.history;
        // One the controller took for something other than a turn still prints, in order.
        let sent: Vec<_> = history
            .active_cells()
            .filter(|(_, cell)| matches!(cell, Cell::User(message) if message.pending))
            .map(|(id, _)| id)
            .collect();
        for id in sent {
            if let Some(message) = history.active_mut(id).and_then(Cell::as_user_mut) {
                message.start();
            }
            history.finish_active(id);
        }
        let echo = UserMessage::pending(shown.trim())
            .with_full(text.clone())
            .with_keywords(keywords);
        history.push_active(echo);
    }
    cx.out.send(UiCommand::Submit { text });
}

fn mention(target: MentionTarget, message: String, typed: &str, cx: &mut Cx<'_>) {
    let name = typed
        .trim_start()
        .strip_prefix('@')
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_default()
        .to_owned();
    let run_going = matches!(
        cx.state.mode,
        Some(AppMode::Working { .. } | AppMode::Pausing { .. } | AppMode::Parked { .. })
    );
    if !run_going {
        notice(
            cx,
            "No run is going, so nothing was sent.".into(),
            Tone::Warning,
        );
        give_back(typed, cx);
        return;
    }
    if message.trim().is_empty() {
        notice(cx, format!("Add a message after @{name}."), Tone::Warning);
        give_back(typed, cx);
        return;
    }
    match mentions::resolve_target(&target, cx.state) {
        Resolved::One(target) => cx.out.send(UiCommand::Mention {
            target,
            text: message,
        }),
        Resolved::Ambiguous(choices) => cx.out.open(MentionPicker::new(name, message, choices)),
        Resolved::Unknown => {
            notice(
                cx,
                format!("No one in this run answers to @{name}, so nothing was sent."),
                Tone::Warning,
            );
            give_back(typed, cx);
        }
    }
}

fn slash(command: SlashCommand, typed: &str, cx: &mut Cx<'_>) {
    let name = typed
        .trim_start()
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or_default();
    let handler = commands::handler(name, cx.state.vendor_commands.as_ref());
    match (handler, command) {
        (Handler::Ui, SlashCommand::Workflows) => super::keys::open_tree(cx),
        (Handler::Ui, SlashCommand::Copy { nth }) => copy_answer(nth, cx),
        (Handler::Ui, SlashCommand::Help) => open_help(cx),
        (Handler::Ui, SlashCommand::Exit) => cx.out.exit = true,
        (Handler::Both, SlashCommand::Verbosity { level: Some(level) }) => {
            set_verbosity(level, cx);
        }
        (Handler::Both, SlashCommand::Vim) => {
            let on = cx.settings.keys != KeyScheme::Vim;
            cx.out.vim = Some(on);
            cx.out.send(UiCommand::Vim { on });
            cx.model.bottom.composer.set_vim(on);
            cx.model.bottom.mode.vim = cx.model.bottom.composer.vim_mode();
        }
        (Handler::Both, SlashCommand::Theme { name: None }) => cx.out.send(UiCommand::Request {
            view: ViewRequest::Settings {
                section: Some("ui".into()),
            },
        }),
        (_, command) => cx.out.send(UiCommand::Slash { command }),
    }
}

/// `/copy [n]`: the last answer on screen, or the nth counting back, as you read it.
fn copy_answer(nth: Option<u32>, cx: &mut Cx<'_>) {
    let nth = usize::try_from(nth.unwrap_or(1)).unwrap_or(usize::MAX);
    let answers: Vec<&str> = cx
        .model
        .history
        .cells()
        .iter()
        .filter_map(|cell| match cell {
            Cell::Assistant(message) => Some(message.markdown.as_str()),
            _ => None,
        })
        .collect();
    let Some(answer) = answers.iter().rev().nth(nth.saturating_sub(1)) else {
        let text = match answers.len() {
            0 => "No answer to copy yet.".to_owned(),
            1 => "There's only 1 answer on screen.".to_owned(),
            n => format!("There are only {n} answers on screen."),
        };
        notice(cx, text, Tone::Warning);
        return;
    };
    let text = answer.trim_end().to_owned();
    let lines = text.lines().count();
    let which = if nth == 1 {
        "the last answer".to_owned()
    } else {
        format!("answer {nth} from the end")
    };
    let size = if lines == 1 {
        "1 line".to_owned()
    } else {
        format!("{lines} lines")
    };
    notice(cx, format!("Copied {which} ({size})."), Tone::Dim);
    cx.out.clipboard = Some(text);
}

fn set_verbosity(level: Verbosity, cx: &mut Cx<'_>) {
    cx.out.verbosity = Some(level);
    cx.out.send(UiCommand::Verbosity { level });
}

fn open_help(cx: &mut Cx<'_>) {
    // A bad `ui.keymap` is reported where the settings load; help still shows the defaults.
    let keymap = Keymap::from_settings(&cx.settings.keymap).unwrap_or_default();
    cx.out
        .open(HelpView::new(keymap).with_commands(cx.state.vendor_commands.clone()));
}

/// ↑ takes back every message of yours the attached run hasn't delivered yet. The texts come
/// back with `AppEvent::TakenBack` once the controller has pulled them.
fn take_back(cx: &mut Cx<'_>) {
    let messages: Vec<_> = cx
        .state
        .attached_run()
        .into_iter()
        .flat_map(|run| &run.queued)
        .filter(|message| message.from == Party::User)
        .map(|message| message.id.clone())
        .collect();
    // Chat text waiting for its turn, or in a session that hasn't read it yet.
    if messages.is_empty() {
        if !cx.state.chat.queued.is_empty() {
            cx.out.send(UiCommand::TakeBackChat);
        }
        return;
    }
    let Some(run) = cx.state.attached_run() else {
        return;
    };
    cx.out.send(UiCommand::TakeBack {
        run: run.id.clone(),
        messages,
    });
}

/// What wasn't sent goes back in the input, unless you've started typing something else.
fn give_back(text: &str, cx: &mut Cx<'_>) {
    let composer = &mut cx.model.bottom.composer;
    if composer.is_empty() {
        composer.set_text(text);
    }
}

fn notice(cx: &mut Cx<'_>, text: String, tone: Tone) {
    cx.model.bottom.mode.notice = Some(ModeNotice {
        text,
        tone,
        until: cx.now + NOTICE_FOR,
    });
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::events::session::{Delivery, QueuedMessage};
    use argonauts_core::names::MessageId;
    use argonauts_core::names::WorkerRef;
    use argonauts_core::settings::ui::UiSettings;
    use time::OffsetDateTime;

    use super::*;
    use crate::bottom::{CardOption, CardPrompt, VimMode};
    use crate::bridge::Effects;
    use crate::cell::Cell;
    use crate::mentions::fixture;
    use crate::model::ViewModel;
    use crate::state::AppState;

    struct Screen {
        router: InputRouter,
        model: ViewModel,
        state: AppState,
        settings: UiSettings,
    }

    impl Screen {
        fn new(state: AppState) -> Self {
            Self {
                router: InputRouter::default(),
                model: ViewModel::default(),
                state,
                settings: UiSettings::default(),
            }
        }

        fn idle() -> Self {
            let mut state = AppState::default();
            state.mode = Some(AppMode::Idle);
            Self::new(state)
        }

        fn act(&mut self, action: UiAction) -> Effects {
            let mut out = Effects::default();
            let mut cx = Cx {
                model: &mut self.model,
                state: &self.state,
                settings: &self.settings,
                now: Instant::now(),
                out: &mut out,
            };
            assert!(self.router.on_action(&action, &mut cx));
            out
        }

        fn event(&mut self, event: AppEvent) -> Effects {
            let mut out = Effects::default();
            let mut cx = Cx {
                model: &mut self.model,
                state: &self.state,
                settings: &self.settings,
                now: Instant::now(),
                out: &mut out,
            };
            self.router.on_event(&event, &mut cx);
            out
        }

        fn submit(&mut self, text: &str) -> Effects {
            self.act(UiAction::Submit {
                text: text.into(),
                shown: text.into(),
            })
        }

        fn notice(&self) -> Option<&str> {
            self.model
                .bottom
                .mode
                .notice
                .as_ref()
                .map(|n| n.text.as_str())
        }
    }

    fn slash(command: SlashCommand) -> UiCommand {
        UiCommand::Slash { command }
    }

    #[test]
    fn the_main_agents_commands_join_the_menu_and_parse() {
        let mut screen = Screen::idle();
        let set = crate::commands::tests::claude_commands();
        screen.event(AppEvent::VendorCommands { set: set.clone() });
        let menu = &screen.model.bottom.menus.commands;
        assert!(
            menu.iter()
                .any(|item| item.name == "ship" && item.group.as_deref() == Some("Claude Code"))
        );
        screen.state.vendor_commands = Some(set);

        assert_eq!(
            screen.submit("/ship  main").commands,
            [slash(SlashCommand::Vendor {
                name: "ship".into(),
                args: "main".into()
            })]
        );
        assert_eq!(
            screen.submit("/reset").commands,
            [slash(SlashCommand::Clear)]
        );
        let quit = screen.submit("/quit");
        assert!(
            quit.exit,
            "the CLI's name for /exit closes the screen as /exit does"
        );
        assert!(quit.commands.is_empty());

        let voice = screen.submit("/voice tap");
        assert!(voice.commands.is_empty());
        assert_eq!(
            screen.notice(),
            Some("/voice works only in Claude Code itself")
        );
        assert_eq!(screen.model.bottom.composer.text(), "/voice tap");
    }

    #[test]
    fn submit_routes_ui_both_app_commands() {
        let mut screen = Screen::new(fixture::working_state());
        assert_eq!(
            screen.submit("/workflows").commands,
            [UiCommand::Request {
                view: ViewRequest::Workflow {
                    run: fixture::run_id(),
                    workflow: None,
                },
            }]
        );
        let help = screen.submit("/help");
        assert!(help.commands.is_empty());
        assert_eq!(help.views.len(), 1);
        let exit = screen.submit("/exit");
        assert!(exit.exit && exit.commands.is_empty());
        for typed in ["exit", "  quit ", "quit"] {
            let out = screen.submit(typed);
            assert!(out.exit && out.commands.is_empty(), "{typed:?}");
        }
        for typed in ["exit now", "Exit", "please quit"] {
            assert!(!screen.submit(typed).exit, "{typed:?} is a message");
        }

        let full = screen.submit("/verbosity full");
        assert_eq!(full.verbosity, Some(Verbosity::Full));
        assert_eq!(
            full.commands,
            [UiCommand::Verbosity {
                level: Verbosity::Full
            }]
        );
        assert_eq!(
            screen.submit("/verbosity quiet").verbosity,
            Some(Verbosity::Quiet)
        );
        assert_eq!(
            screen.submit("/verbosity normal").verbosity,
            Some(Verbosity::Normal)
        );
        // No level asks the controller for the picker.
        let picker = screen.submit("/verbosity");
        assert_eq!(picker.verbosity, None);
        assert_eq!(
            picker.commands,
            [slash(SlashCommand::Verbosity { level: None })]
        );

        let vim = screen.submit("/vim");
        assert_eq!(vim.vim, Some(true));
        assert_eq!(vim.commands, [UiCommand::Vim { on: true }]);
        assert_eq!(
            screen.model.bottom.composer.vim_mode(),
            Some(VimMode::Insert)
        );
        screen.settings.keys = KeyScheme::Vim;
        assert_eq!(
            screen.submit("/vim").commands,
            [UiCommand::Vim { on: false }]
        );
        assert_eq!(screen.model.bottom.composer.vim_mode(), None);

        assert_eq!(
            screen.submit("/theme").commands,
            [UiCommand::Request {
                view: ViewRequest::Settings {
                    section: Some("ui".into()),
                },
            }]
        );
        assert_eq!(
            screen.submit("/theme dark").commands,
            [slash(SlashCommand::Theme {
                name: Some("dark".into())
            })]
        );
        assert_eq!(
            screen.submit("/status").commands,
            [slash(SlashCommand::Status)]
        );
        assert_eq!(
            screen.submit("/solo fix the export").commands,
            [slash(SlashCommand::Solo {
                task: "fix the export".into()
            })]
        );
        assert!(screen.notice().is_none());
    }

    #[test]
    fn input_error_sends_nothing() {
        let mut screen = Screen::idle();
        let out = screen.submit("/nope now");
        assert!(out.commands.is_empty() && out.views.is_empty() && !out.exit);
        assert_eq!(screen.notice(), Some("unknown command /nope"));
        assert_eq!(screen.model.bottom.composer.text(), "/nope now");

        screen.model.bottom.composer.set_text("something else");
        assert!(screen.submit("@ codex").commands.is_empty());
        assert_eq!(
            screen.notice(),
            Some("@ needs a target, such as @codex or @card-002")
        );
        assert_eq!(screen.model.bottom.composer.text(), "something else");
        assert!(screen.model.history.cells().is_empty());
    }

    #[test]
    fn plain_text_echoes_only_when_idle() {
        let mut screen = Screen::idle();
        let out = screen.act(UiAction::Submit {
            text: "look at line 1\nline 2\nline 3\nline 4".into(),
            shown: "look at [Pasted text #1 +3 lines]".into(),
        });
        assert_eq!(
            out.commands,
            [UiCommand::Submit {
                text: "look at line 1\nline 2\nline 3\nline 4".into()
            }]
        );
        // Grey in the live region until its turn starts, which prints it once.
        assert!(screen.model.history.cells().is_empty());
        let echoes: Vec<_> = screen.model.history.active_cells().collect();
        let [(_, Cell::User(echo))] = echoes.as_slice() else {
            panic!("one echo");
        };
        assert_eq!(echo.text, "look at [Pasted text #1 +3 lines]");
        assert!(echo.pending);

        let mut working = Screen::new(fixture::working_state());
        working.submit("keep the old API");
        assert!(working.model.history.cells().is_empty());
    }

    #[test]
    fn keywords_reach_the_input_and_echo_and_the_text_goes_out_as_typed() {
        use argonauts_core::keywords::{Keyword, KeywordLook};

        let ultrathink = Keyword::new("ultrathink", KeywordLook::Rainbow, true);
        let mut screen = Screen::idle();
        let set = crate::commands::tests::claude_commands()
            .with_keywords(std::slice::from_ref(&ultrathink));
        screen.event(AppEvent::VendorCommands { set: set.clone() });
        assert_eq!(
            screen.model.bottom.composer.keywords,
            std::slice::from_ref(&ultrathink)
        );
        screen.state.vendor_commands = Some(set);

        let out = screen.submit("please ultrathink about this");
        assert_eq!(
            out.commands,
            [UiCommand::Submit {
                text: "please ultrathink about this".into()
            }]
        );
        let echoes: Vec<_> = screen.model.history.active_cells().collect();
        let [(_, Cell::User(echo))] = echoes.as_slice() else {
            panic!("one echo");
        };
        assert_eq!(echo.text, "please ultrathink about this");
        assert_eq!(echo.keywords, [ultrathink]);
    }

    #[test]
    fn mentions_need_a_run_and_a_known_target() {
        let mut screen = Screen::new(fixture::working_state());
        assert_eq!(
            screen.submit("@claude-code check the schema").commands,
            [UiCommand::Mention {
                target: MentionTarget::Worker(WorkerRef::new("claude-code", None)),
                text: "check the schema".into(),
            }]
        );
        assert!(screen.submit("@9 hello").commands.is_empty());
        assert_eq!(
            screen.notice(),
            Some("No one in this run answers to @9, so nothing was sent.")
        );
        assert_eq!(screen.model.bottom.composer.text(), "@9 hello");

        let mut idle = Screen::idle();
        assert!(idle.submit("@codex hi").commands.is_empty());
        assert_eq!(idle.notice(), Some("No run is going, so nothing was sent."));
    }

    #[test]
    fn a_message_led_by_a_repo_file_goes_as_plain_text() {
        let files = AppEvent::RepoFiles {
            paths: vec!["README.md".into(), "src/lib.rs".into()],
        };
        let mut idle = Screen::idle();
        idle.event(files.clone());
        let out = idle.submit("@README.md what is this?");
        assert_eq!(
            out.commands,
            [UiCommand::Submit {
                text: "@README.md what is this?".into()
            }]
        );
        assert_eq!(idle.notice(), None);
        assert!(idle.model.bottom.menus.is_open("@sr"));

        // In a run, it's guidance for everyone, and an agent's name is still a mention.
        let mut screen = Screen::new(fixture::working_state());
        screen.event(files);
        assert_eq!(
            screen.submit("@src/ keep the API").commands,
            [UiCommand::Submit {
                text: "@src/ keep the API".into()
            }]
        );
        assert!(matches!(
            screen.submit("@claude-code see @README.md").commands[..],
            [UiCommand::Mention { .. }]
        ));
    }

    #[test]
    fn ambiguous_mention_opens_a_picker() {
        let mut screen = Screen::new(fixture::working_state());
        let out = screen.submit("@codex rebase first");
        assert!(out.commands.is_empty());
        assert_eq!(out.views.len(), 1);
        assert!(format!("{:?}", out.views[0]).contains("MentionPicker"));
    }

    #[test]
    fn take_back_sends_message_ids() {
        let mut state = fixture::working_state();
        let queued = |n: u32, from: Party, text: &str| QueuedMessage {
            id: MessageId::numbered(n),
            from,
            to: MentionTarget::All,
            text: text.into(),
            queued_at: OffsetDateTime::UNIX_EPOCH,
            delivery: Delivery::NextTurn,
        };
        let run = state.runs.get_mut(&fixture::run_id()).unwrap();
        run.queued = vec![
            queued(1, Party::User, "first"),
            queued(2, Party::Rules, "from your rules"),
            queued(3, Party::User, "second"),
        ];
        let mut screen = Screen::new(state);
        let out = screen.act(UiAction::TakeBack(vec!["first".into(), "second".into()]));
        assert_eq!(
            out.commands,
            [UiCommand::TakeBack {
                run: fixture::run_id(),
                messages: vec![MessageId::numbered(1), MessageId::numbered(3)],
            }]
        );

        screen.model.bottom.composer.set_text("draft");
        screen.event(AppEvent::TakenBack {
            texts: vec!["first".into(), "second".into()],
        });
        let composer = &screen.model.bottom.composer;
        assert_eq!(composer.text(), "first\nsecond\ndraft");
        assert_eq!(composer.cursor(), "first\nsecond".len());
    }

    #[test]
    fn typed_text_never_answers_card_routing() {
        assert!(!typed_text_answers_card());
        let mut screen = Screen::new(fixture::working_state());
        let mut card = CardPrompt::new(
            7,
            "Merge card-002 into main?",
            "Merge card-002?",
            vec![CardOption::new("Merge")],
        );
        card.focus();
        screen.model.bottom.card = Some(card);
        let out = screen.submit("yes, merge it");
        assert_eq!(
            out.commands,
            [UiCommand::Submit {
                text: "yes, merge it".into()
            }]
        );
        assert!(screen.model.bottom.card.is_some());
    }

    #[test]
    fn empty_enter_goes_on_only_when_paused() {
        let mut screen = Screen::idle();
        assert!(screen.submit("  ").commands.is_empty());
        screen.state.mode = Some(AppMode::Pausing {
            run: fixture::run_id(),
        });
        assert_eq!(
            screen.submit("").commands,
            [UiCommand::Submit {
                text: String::new()
            }]
        );
    }

    #[test]
    fn prepare_seeds_history_and_menus_and_submits_append() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("history.jsonl");
        std::fs::write(&file, "\"older prompt\"\n").unwrap();
        let options = UiOptions {
            history_file: Some(file.clone()),
            ..UiOptions::default()
        };
        let mut screen = Screen::new(fixture::working_state());
        screen.router = InputRouter::new(&options);
        screen.model.bottom.composer.remember("typed early");
        screen.event(AppEvent::Detached {
            run: fixture::run_id(),
        });
        let bottom = &screen.model.bottom;
        assert_eq!(bottom.composer.history(), ["older prompt", "typed early"]);
        assert_eq!(
            bottom.menus.commands.len(),
            argonauts_core::input::SLASH_COMMANDS.len()
                + argonauts_core::input::SHARED_COMMANDS.len()
        );
        assert!(
            bottom
                .menus
                .commands
                .iter()
                .any(|c| c.name == "solo" && c.needs_argument)
        );
        assert_eq!(bottom.menus.mentions, mentions::candidates(&screen.state));

        screen.submit("/status");
        let saved = std::fs::read_to_string(&file).unwrap();
        let (older, added) = saved.split_once('\n').unwrap();
        assert_eq!(older, "\"older prompt\"");
        assert!(
            added.starts_with("{\"prompt\":\"/status\",\"at\":") && added.ends_with("}\n"),
            "{added}"
        );
    }

    #[test]
    fn search_history_opens_with_this_repos_prompts_and_every_repos() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(state.join("web-111")).unwrap();
        std::fs::write(
            state.join("web-111").join("history.jsonl"),
            "{\"prompt\":\"deploy the web app\",\"at\":5}\n",
        )
        .unwrap();
        let options = UiOptions {
            history_file: Some(state.join("here-222").join("history.jsonl")),
            ..UiOptions::default()
        };
        let mut screen = Screen::idle();
        screen.router = InputRouter::new(&options);
        screen.model.bottom.mode.vim = Some(crate::bottom::VimMode::Normal);
        screen.router.history.append("fix the export test").unwrap();

        let out = screen.act(UiAction::SearchHistory);
        assert!(out.commands.is_empty(), "opening the search sends nothing");
        let search = screen.model.bottom.history_search.as_mut().unwrap();
        assert_eq!(search.scope(), crate::bottom::SearchScope::Everywhere);
        assert_eq!(
            search.shown(),
            ["deploy the web app", "fix the export test"]
        );
        assert!(search.shows_slash_hint(), "Esc i / works from NORMAL");
        search.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('s'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        assert_eq!(search.shown(), ["fix the export test"]);
    }

    #[test]
    fn the_slash_hint_shows_only_when_esc_goes_back_to_normal() {
        for (mode, hint) in [
            (Some(VimMode::Normal), true),
            (Some(VimMode::Insert), false),
            (None, false),
        ] {
            let mut screen = Screen::idle();
            screen.model.bottom.mode.vim = mode;
            screen.act(UiAction::SearchHistory);
            let search = screen.model.bottom.history_search.as_ref().unwrap();
            assert_eq!(search.shows_slash_hint(), hint, "{mode:?}");
        }
    }

    #[test]
    fn search_history_has_prompts_another_window_in_this_repo_sent_since_start() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir
            .path()
            .join("state")
            .join("here-222")
            .join("history.jsonl");
        let options = UiOptions {
            history_file: Some(file.clone()),
            ..UiOptions::default()
        };
        let mut screen = Screen::idle();
        screen.router = InputRouter::new(&options);
        screen.router.history.append("fix the export test").unwrap();
        let line = "{\"prompt\":\"sent from the other window\",\"at\":4000000000}\n";
        let mut out = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap();
        std::io::Write::write_all(&mut out, line.as_bytes()).unwrap();
        drop(out);

        screen.act(UiAction::SearchHistory);
        let search = screen.model.bottom.history_search.as_mut().unwrap();
        let both = ["fix the export test", "sent from the other window"];
        assert_eq!(search.scope(), crate::bottom::SearchScope::Everywhere);
        assert_eq!(search.shown(), both);
        search.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('s'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        assert_eq!(search.scope(), crate::bottom::SearchScope::Project);
        assert_eq!(search.shown(), both);
    }

    #[test]
    fn a_shell_command_goes_to_the_app_and_history_keeps_its_bang() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("history.jsonl");
        let options = UiOptions {
            history_file: Some(file.clone()),
            ..UiOptions::default()
        };
        // Whatever the mode, a run going or not, the command is yours to run.
        let mut screen = Screen::new(fixture::working_state());
        screen.router = InputRouter::new(&options);
        let out = screen.act(UiAction::Shell {
            command: "git status".into(),
        });
        assert_eq!(
            out.commands,
            [UiCommand::Shell {
                command: "git status".into()
            }]
        );
        assert_eq!(
            screen.model.history.active_cells().count(),
            0,
            "the app echoes it once it starts"
        );
        let saved = std::fs::read_to_string(&file).unwrap();
        assert!(
            saved.starts_with("{\"prompt\":\"!git status\",\"at\":") && saved.ends_with("}\n"),
            "{saved}"
        );
        let mut later = Screen::idle();
        later.router = InputRouter::new(&options);
        later.router.prepare(&mut later.model.bottom);
        assert_eq!(later.model.bottom.composer.history(), ["!git status"]);
    }

    #[test]
    fn an_unreadable_history_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let options = UiOptions {
            history_file: Some(dir.path().to_owned()),
            ..UiOptions::default()
        };
        let mut screen = Screen::idle();
        screen.router = InputRouter::new(&options);
        screen.event(AppEvent::Detached {
            run: fixture::run_id(),
        });
        assert!(
            screen
                .notice()
                .is_some_and(|n| n.starts_with("prompt history")),
            "{:?}",
            screen.notice()
        );
    }

    #[test]
    fn the_welcome_brings_the_inputs_example() {
        let mut screen = Screen::idle();
        let welcome = |example: Option<&str>| AppEvent::Welcome {
            version: "0.1.0".into(),
            repo: "~/argonauts".into(),
            preset: "peer-crew".into(),
            agents: Vec::new(),
            runs_going: 0,
            runs_need_you: 0,
            example_prompt: example.map(str::to_owned),
            default_agent: None,
            effort: None,
        };
        screen.event(welcome(Some("fix the flaky export test")));
        let composer = &mut screen.model.bottom.composer;
        assert_eq!(
            composer.placeholder_text(true).as_deref(),
            Some("Try \"fix the flaky export test\"")
        );
        composer.end_example();
        assert_eq!(composer.placeholder_text(true), None);

        let mut screen = Screen::idle();
        screen.event(welcome(None));
        let built_in = format!("Try \"{}\"", crate::bottom::BUILT_IN_EXAMPLE);
        assert_eq!(
            screen.model.bottom.composer.placeholder_text(true),
            Some(built_in)
        );
    }

    #[test]
    fn copy_puts_an_answer_on_screen_on_the_clipboard() {
        let mut screen = Screen::idle();
        let none = screen.submit("/copy");
        assert_eq!(none.clipboard, None);
        assert_eq!(screen.notice(), Some("No answer to copy yet."));

        let history = &mut screen.model.history;
        history.push(crate::cell::AssistantMessage::new("First answer.\n"));
        history.push(UserMessage::new("and then?"));
        history.push(crate::cell::AssistantMessage::new(
            "Second answer,\nin two lines.\n",
        ));
        let last = screen.submit("/copy");
        assert_eq!(
            last.clipboard.as_deref(),
            Some("Second answer,\nin two lines.")
        );
        assert!(last.commands.is_empty(), "the screen copies it itself");
        assert_eq!(screen.notice(), Some("Copied the last answer (2 lines)."));

        let first = screen.submit("/copy 2");
        assert_eq!(first.clipboard.as_deref(), Some("First answer."));
        assert_eq!(
            screen.notice(),
            Some("Copied answer 2 from the end (1 line).")
        );
        assert_eq!(screen.submit("/copy 3").clipboard, None);
        assert_eq!(screen.notice(), Some("There are only 2 answers on screen."));
    }
}
