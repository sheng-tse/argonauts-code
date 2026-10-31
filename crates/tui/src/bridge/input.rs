//! What you type: plain text, `/commands`, and `@mentions`, routed by the app mode. Text typed
//! at the prompt never answers a question; see [`typed_text_answers_question`].

use std::time::{Duration, Instant};

use argonauts_core::app::{AppEvent, AppMode, ConversationEvent, UiCommand, ViewRequest};
use argonauts_core::input::{
    Handler, LineChange, MentionTarget, ParsedInput, SlashCommand, ViewAction, WorkflowVerb,
    parse_input_with,
};
use argonauts_core::runs::api::UsagePeriod;
use argonauts_core::runs::events::run::Party;
use argonauts_core::settings::Settings;
use argonauts_core::settings::stats::StatsPeriod;
use argonauts_core::settings::ui::{UiSettings, ViewOpen};
use argonauts_core::{KeyScheme, Verbosity};

use crate::UiOptions;
use crate::app::UiAction;
use crate::base::input::keymap::Keymap;
use crate::base::input::prompt_history::{PromptHistory, unix_now};
use crate::bottom::menu::commands::{self, command_items};
use crate::bottom::{BottomState, HistorySearch, ModeNotice, VimMode};
use crate::bridge::mentions::{self, MentionPicker, Resolved};
use crate::cells::cell::{Cell, Tone, UserMessage};
use crate::config::ConfigTab;
use crate::panels::{OverviewView, Tab};
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
    /// `/status` ran, so its answer opens the status view rather than printing in the conversation
    /// as "where are we?" does. The next thing you send clears it.
    pub status_view: bool,
    /// `/context all` ran, so the Context tab its answer opens lists every item of every
    /// category. The next thing you send clears it.
    pub context_all: bool,
    /// A command typed while an answer ran, and when, until its answer comes. What it prints
    /// starts with this as its own row, so it reads as yours and not as part of the answer,
    /// and a view it opens is a panel under the working line.
    typed_mid_answer: Option<(String, Instant)>,
    /// The first welcome came, which opens the agent panel with `ui.agents_view`. Later ones,
    /// after the setup saves, leave the screen as it is.
    welcomed: bool,
    /// What you typed in a conversation being drawn again, oldest first, which ↑ brings back
    /// once it is drawn.
    past_prompts: Option<Vec<String>>,
}

/// How long a command typed mid-answer waits for its answer. What comes later isn't its.
const ANSWER_WAITS: Duration = Duration::from_secs(3);

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
                ..Self::default()
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
                if !std::mem::replace(&mut self.welcomed, true) && cx.settings.agents_view {
                    super::keys::open_agent_panel(cx);
                }
            }
            AppEvent::Updated { words } => cx.model.bottom.updated = Some(words.clone()),
            AppEvent::SettingChanged { key, value, .. } if key == Settings::ULTRACODE_TRIGGER => {
                cx.model.bottom.composer.workflow_word_off = value.trim() == "false";
            }
            AppEvent::TakenBack { texts } => {
                cx.model.bottom.composer.restore(texts);
                cx.model.bottom.menus.text_changed();
            }
            AppEvent::ConversationCleared { .. } => cx.model.bottom.composer.start_conversation(),
            AppEvent::ConversationResumed { turns, .. } => {
                let yours = turns
                    .iter()
                    .filter(|turn| matches!(turn.from, Party::User))
                    .map(|turn| turn.text.as_str());
                cx.model.bottom.composer.resume_conversation(yours);
            }
            // A conversation drawn again brings back what you typed in it, for ↑.
            AppEvent::PastBegins => self.past_prompts = Some(Vec::new()),
            AppEvent::Conversation {
                event:
                    ConversationEvent::TurnStarted {
                        text,
                        continues: false,
                        ..
                    },
            } if self.past_prompts.is_some() => {
                if let Some(prompts) = self.past_prompts.as_mut() {
                    prompts.push(text.clone());
                }
            }
            AppEvent::PastDrawn => {
                if let Some(prompts) = self.past_prompts.take()
                    && !prompts.is_empty()
                {
                    let yours = prompts.iter().map(String::as_str);
                    cx.model.bottom.composer.resume_conversation(yours);
                }
            }
            AppEvent::CliCommands { set } => {
                cx.model.bottom.menus.commands = command_items(Some(set));
                cx.model.bottom.composer.keywords.clone_from(&set.keywords);
            }
            AppEvent::RepoFiles { paths } => cx.model.bottom.menus.set_files(paths),
            AppEvent::WorkflowStatuses { statuses } => {
                cx.model.bottom.menus.workflow_statuses = statuses.iter().cloned().collect();
            }
            AppEvent::Hint { text } => notice(cx, text.clone(), Tone::Dim),
            AppEvent::Attached { .. } | AppEvent::Detached { .. } | AppEvent::Run { .. } => {
                // A council's seats come first while it talks here.
                let seats = mentions::seats(&cx.model.bottom.council.faces);
                let mentions = mentions::seats_first(seats, mentions::candidates(cx.state));
                if cx.model.bottom.menus.mentions != mentions {
                    cx.model.bottom.menus.mentions = mentions;
                }
            }
            _ => {}
        }
    }

    /// Takes `Submit` (parsed with `parse_input_with` and the main agent's commands into `Submit`,
    /// `Slash`, or `Mention`, or run here for `Ui` and `Both` commands), `TakeBack`, `OpenHelp`,
    /// and `SearchHistory`.
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
        // Whatever you send takes the reply's row away; a message to the council answers it.
        let replying = cx.model.bottom.replying.take();
        // Typed alone, these leave as `/exit` does, as in Claude Code, instead of going to the
        // agent as a message.
        if matches!(text.trim(), "exit" | "quit") {
            cx.out.exit = true;
            return;
        }
        if let Err(error) = self.history.append(text) {
            notice(cx, error.to_string(), Tone::Error);
        }
        // The draft held the word `ultracode` and Opt+W didn't ignore it. Read for every
        // prompt, so one that goes elsewhere never leaves it for the next.
        let workflow = cx.model.bottom.composer.take_workflow_request();
        // Taken for every prompt too, so a command never leaves its pictures for the next.
        let pictures = cx.model.bottom.composer.take_sent_pictures();
        let parsed = parse_input_with(text, cx.state.cli_commands.as_ref());
        // A mod of yours is a command by its name, when nothing else has that name.
        let parsed = match (
            parsed,
            crate::mods::own_command(text, cx.state.mods.as_ref()),
        ) {
            (Err(argonauts_core::input::InputError::UnknownCommand(_)), Some(command)) => {
                Ok(ParsedInput::Slash { command })
            }
            // A prompt may start with a path of one word, such as `/tmp`.
            (Err(argonauts_core::input::InputError::UnknownCommand(_)), None)
                if starts_with_a_path(text) =>
            {
                Ok(ParsedInput::Plain {
                    text: text.trim().to_owned(),
                })
            }
            (parsed, _) => parsed,
        };
        let parsed = match parsed {
            Ok(ParsedInput::Slash { command }) => Ok(ParsedInput::Slash {
                command: as_its_tab_opens(command, cx.settings),
            }),
            other => other,
        };
        self.typed_mid_answer = (matches!(parsed, Ok(ParsedInput::Slash { .. }))
            && answer_running(cx))
        .then(|| (text.trim().to_owned(), cx.now));
        // A workflow's command goes over the line it leaves, now or once its sheet saves, with
        // a place flag too. Mid-answer every command's line gets its command over it already.
        let switch = |command: &SlashCommand| {
            matches!(
                command,
                SlashCommand::Workflow {
                    name: Some(_),
                    action: WorkflowVerb::Use,
                }
            )
        };
        let switches = match &parsed {
            Ok(ParsedInput::Slash {
                command: SlashCommand::SavedIn { change, .. },
            }) => switch(change),
            Ok(ParsedInput::Slash { command }) => switch(command),
            _ => false,
        };
        cx.model.typed_workflow = (switches && !answer_running(cx)).then(|| text.trim().to_owned());
        // Printing or pinning the tab opens no view.
        self.status_view = matches!(
            parsed,
            Ok(ParsedInput::Slash {
                command: SlashCommand::Status {
                    action: ViewAction::Open
                }
            })
        );
        self.context_all = matches!(
            parsed,
            Ok(ParsedInput::Slash {
                command: SlashCommand::Context {
                    all: true,
                    action: ViewAction::Open
                }
            })
        );
        match parsed {
            Ok(ParsedInput::Empty) => empty_enter(cx),
            Ok(ParsedInput::Plain { text }) => {
                if !to_council(&text, &pictures, replying.as_ref(), cx) {
                    plain(text, shown, workflow, pictures, cx);
                }
            }
            // A seat's name up front is a mention its council reads, not a worker to steer.
            Ok(ParsedInput::Mention { .. })
                if cx.model.bottom.menus.starts_with_file(text)
                    || cx.model.bottom.council.faces.starts_with_seat(text) =>
            {
                if !to_council(text, &pictures, replying.as_ref(), cx) {
                    plain(text.trim().to_owned(), shown, workflow, pictures, cx);
                }
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

impl InputRouter {
    /// Called before `event` prints or opens anything. When it answers a command typed while
    /// an answer ran, what it prints gets the command as a row of its own first, and a view
    /// it opens is marked to open as a panel.
    pub fn answers_mid_answer_command(&mut self, event: &AppEvent, cx: &mut Cx<'_>) {
        let prints = matches!(
            event,
            AppEvent::Report { .. }
                | AppEvent::Notice { .. }
                | AppEvent::SettingChanged { .. }
                | AppEvent::SettingRefused { .. }
                | AppEvent::CommandReplied { .. }
        );
        let opens = matches!(event, AppEvent::View { .. });
        if !prints && !opens {
            return;
        }
        let Some((typed, at)) = self.typed_mid_answer.take() else {
            return;
        };
        if cx.now.saturating_duration_since(at) > ANSWER_WAITS || cx.replaying() {
            return;
        }
        if opens {
            cx.out.views_as_panel = true;
        } else {
            cx.model.history.push(UserMessage::command(typed));
        }
    }
}

/// The panel on its Config tab, as `/config` or `/settings` (`name`) opens it.
fn open_config(name: &str, cx: &mut Cx<'_>) {
    let watch = cx.model.bottom.band.watch.clone();
    let mut tab = ConfigTab::opened_by(name);
    tab.set_mode_now(Some(cx.model.bottom.mode.approval.label().to_owned()));
    cx.out.open(
        OverviewView::new(Tab::Config, watch, crate::config::panel()).with_config(Box::new(tab)),
    );
    cx.out.send(UiCommand::Request {
        view: ViewRequest::Settings { section: None },
    });
}

/// The period `/usage` names, by the words the CLIs' own commands take, `opens_on` when it
/// names none. `None` for a word the controller will refuse, which then says so.
fn usage_period(word: Option<&str>, opens_on: UsagePeriod) -> Option<UsagePeriod> {
    match word.map(str::to_ascii_lowercase).as_deref() {
        None => Some(opens_on),
        Some("day" | "daily" | "today") => Some(UsagePeriod::Today),
        Some("week" | "weekly") => Some(UsagePeriod::Week),
        Some("month" | "monthly") => Some(UsagePeriod::Month),
        Some("all" | "cumulative" | "total") => Some(UsagePeriod::All),
        Some(_) => None,
    }
}

/// An answer or a run is working, so what's typed now lands beside it.
fn answer_running(cx: &Cx<'_>) -> bool {
    cx.model.is_working()
        || matches!(
            cx.state.mode,
            Some(AppMode::Answering { .. } | AppMode::Working { .. } | AppMode::Pausing { .. })
        )
}

/// The one place that decides whether text typed at the prompt can answer a focused question.
/// It never can; notes go through Tab. Flip this to change the rule.
pub fn typed_text_answers_question() -> bool {
    false
}

/// Whether the first word of `text` is something on this computer.
fn starts_with_a_path(text: &str) -> bool {
    text.split_whitespace()
        .next()
        .is_some_and(crate::bottom::is_on_disk)
}

/// Enter on an empty prompt cancels a pause or resumes a waiting run; the controller decides.
fn empty_enter(cx: &mut Cx<'_>) {
    if matches!(
        cx.state.mode,
        Some(AppMode::Pausing { .. } | AppMode::Waiting { .. })
    ) {
        cx.out.send(UiCommand::Submit {
            text: String::new(),
            workflow_requested: false,
            pictures: Vec::new(),
        });
    }
}

/// Plain text goes by the app mode, whatever question waits (see [`typed_text_answers_question`]).
/// With nothing going, it shows as your message at once, grey until its turn starts and prints
/// it; otherwise it shows grey from state until it's delivered. `workflow_requested` says this
/// one request goes through a workflow, which the engine reads.
fn plain(
    text: String,
    shown: &str,
    workflow_requested: bool,
    pictures: Vec<argonauts_core::look::pictures::Picture>,
    cx: &mut Cx<'_>,
) {
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
            .with_keywords(keywords)
            .with_pictures(pictures.iter().map(|picture| picture.number));
        history.push_active(echo);
    }
    cx.out.send(UiCommand::Submit {
        text,
        workflow_requested,
        pictures,
    });
}

/// While a council talks, a message goes to its seats, as a reply when you picked one. One
/// with pictures goes as any other, since a council reads words only. False when it didn't go.
fn to_council(
    text: &str,
    pictures: &[argonauts_core::look::pictures::Picture],
    replying: Option<&crate::council::screens::reply::ReplyBar>,
    cx: &mut Cx<'_>,
) -> bool {
    let say = match pictures {
        [] => cx.model.bottom.council.say(text, replying),
        _ => None,
    };
    let Some(command) = say.and_then(|say| say.command()) else {
        return false;
    };
    cx.out.send(command);
    true
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
        Some(AppMode::Working { .. } | AppMode::Pausing { .. } | AppMode::Waiting { .. })
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

/// A tab's command typed alone prints the tab, as its `print` does, where
/// `ui.views.<tab>.open` says `print`. Any other setting opens the panel.
fn as_its_tab_opens(command: SlashCommand, ui: &UiSettings) -> SlashCommand {
    let prints = |tab: ViewOpen| tab == ViewOpen::Print;
    let (open, print) = (ViewAction::Open, ViewAction::Print);
    let views = &ui.views;
    match command {
        SlashCommand::Status { action } if action == open && prints(views.status.open) => {
            SlashCommand::Status { action: print }
        }
        SlashCommand::Usage { period, action } if action == open && prints(views.usage.open) => {
            SlashCommand::Usage {
                period,
                action: print,
            }
        }
        SlashCommand::Context { all, action } if action == open && prints(views.context.open) => {
            SlashCommand::Context { all, action: print }
        }
        SlashCommand::Limits {
            key: None,
            value: None,
            action,
        } if action == open && prints(views.limits.open) => SlashCommand::Limits {
            key: None,
            value: None,
            action: print,
        },
        other => other,
    }
}

fn slash(command: SlashCommand, typed: &str, cx: &mut Cx<'_>) {
    // The conversation's record keeps it, except one that leaves the conversation.
    let leaves = matches!(
        command,
        SlashCommand::Clear | SlashCommand::Resume { .. } | SlashCommand::Exit
    );
    if !leaves {
        cx.out.typed = Some(typed.trim().to_owned());
    }
    let name = typed
        .trim_start()
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or_default();
    let handler = commands::handler(name, cx.state.cli_commands.as_ref());
    // `/config key=value --session` prints under the command, as it does without the flag.
    if let SlashCommand::SavedIn { change, .. } = &command
        && let SlashCommand::Config {
            key: Some(key),
            value: Some(_),
            reset: false,
        } = change.as_ref()
    {
        cx.model.typed_config = Some((key.clone(), typed.trim().to_owned()));
    }
    match (handler, command) {
        (Handler::Ui, SlashCommand::Workflows) => super::keys::open_tree(cx),
        (Handler::Ui, SlashCommand::Overview) => super::keys::open_overview(cx),
        (Handler::Ui, SlashCommand::Copy { nth }) => copy_answer(nth, cx),
        (Handler::Ui, SlashCommand::Help) => {
            cx.out.views_as_panel = answer_running(cx);
            open_help(cx);
        }
        (Handler::Ui, SlashCommand::Artifacts) => {
            cx.out.open(crate::views::artifacts::ArtifactsView::new());
        }
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
        // `/vim --session`: the keys change here at once, and the controller saves it there.
        (Handler::Both, SlashCommand::SavedIn { scope, change })
            if *change == SlashCommand::Vim =>
        {
            let on = cx.settings.keys != KeyScheme::Vim;
            cx.out.vim = Some(on);
            cx.out.send(UiCommand::Slash {
                command: SlashCommand::SavedIn { scope, change },
            });
            cx.model.bottom.composer.set_vim(on);
            cx.model.bottom.mode.vim = cx.model.bottom.composer.vim_mode();
        }
        // The tab opens at once, here, and the numbers it draws follow from the controller.
        (
            _,
            SlashCommand::Usage {
                period,
                action: ViewAction::Open,
            },
        ) => {
            let opens_on = cx.settings.views.usage.period.usage();
            if let Some(shown) = usage_period(period.as_deref(), opens_on) {
                let watch = cx.model.bottom.band.watch.clone();
                cx.out.open(
                    OverviewView::new(Tab::Usage, watch, crate::config::panel()).with_period(shown),
                );
            }
            cx.out.send(UiCommand::Slash {
                command: SlashCommand::Usage {
                    period,
                    action: ViewAction::Open,
                },
            });
        }
        // `/stats` opens its tab at once. With no period it waits for the one the stats
        // mod's file sets, which comes with the numbers.
        (
            _,
            SlashCommand::Stats {
                period,
                action: ViewAction::Open,
            },
        ) => {
            let watch = cx.model.bottom.band.watch.clone();
            let view = OverviewView::new(Tab::Stats, watch, crate::config::panel());
            let view = match period.as_deref().and_then(StatsPeriod::parse) {
                Some(given) => view.with_period(given.usage()),
                None => view.with_stats_period_from_file(),
            };
            cx.out.open(view);
            cx.out.send(UiCommand::Slash {
                command: SlashCommand::Stats {
                    period,
                    action: ViewAction::Open,
                },
            });
        }
        // `/mods stats`: the stats mod's page is the Stats tab's own.
        (
            _,
            SlashCommand::Mods {
                action:
                    argonauts_core::input::ModsAction::Page {
                        the_mod: argonauts_core::settings::ui::ModName::Stats,
                    },
                scope: None,
            },
        ) => super::keys::open_stats(cx, true),
        // `/limits` alone opens its tab, which shows what counts toward the limits above the
        // limits themselves. A limit to set, or one the tab opens, goes to the controller.
        (
            _,
            SlashCommand::Limits {
                key: None,
                value: None,
                action: ViewAction::Open,
            },
        ) => {
            let watch = cx.model.bottom.band.watch.clone();
            cx.out.open(OverviewView::new(
                Tab::Limits,
                watch,
                crate::config::panel(),
            ));
            // The Limits tab opens on all, whatever period the Usage tab opens on.
            cx.out.send(UiCommand::Slash {
                command: SlashCommand::Usage {
                    period: Some("all".to_owned()),
                    action: ViewAction::Open,
                },
            });
        }
        // `/config` and `/settings` open the Config tab at once, the search box taking the keys;
        // every key follows from the controller.
        (
            _,
            SlashCommand::Config {
                key: None,
                value: None,
                reset: false,
            },
        ) => open_config(name, cx),
        // `/config key=value` prints what it saved under the command, once it is saved.
        (
            _,
            SlashCommand::Config {
                key: Some(key),
                value: Some(value),
                reset: false,
            },
        ) => {
            cx.model.typed_config = Some((key.clone(), typed.trim().to_owned()));
            cx.out.send(UiCommand::Slash {
                command: SlashCommand::Config {
                    key: Some(key),
                    value: Some(value),
                    reset: false,
                },
            });
        }
        // A bare `/bottom-line` is its picker, which saves through the command once you pick.
        (
            Handler::Both,
            SlashCommand::BottomLine {
                change: LineChange::Open,
                scope,
            },
        ) => {
            let bottom = &cx.model.bottom;
            let picker = super::bottom_line::LinePicker::new(
                cx.state,
                bottom.mode.clone(),
                cx.settings,
                &bottom.line_slots,
                scope,
            );
            cx.out.open(picker);
        }
        (Handler::Both, SlashCommand::Theme { name: None }) => cx.out.send(UiCommand::Request {
            view: ViewRequest::Settings {
                section: Some("ui".into()),
            },
        }),
        // Its rows come a while later, so the command shows at once, above them.
        (_, SlashCommand::Update) => {
            cx.model.history.push(UserMessage::command(typed.trim()));
            cx.out.send(UiCommand::Slash {
                command: SlashCommand::Update,
            });
        }
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
        .open(HelpView::new(keymap).with_commands(cx.state.cli_commands.clone()));
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
    // Conversation text waiting for its turn, or in a session that hasn't read it yet.
    if messages.is_empty() {
        if !cx.state.conversation.queued.is_empty() {
            cx.out.send(UiCommand::TakeBackConversation);
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

    use argonauts_core::base::names::MessageId;
    use argonauts_core::base::names::ModelRef;
    use argonauts_core::runs::events::session::{Delivery, QueuedMessage};
    use argonauts_core::settings::ui::UiSettings;
    use time::OffsetDateTime;

    use super::*;
    use crate::app::model::ViewModel;
    use crate::bottom::{QuestionOption, QuestionPrompt, VimMode};
    use crate::bridge::Effects;
    use crate::bridge::mentions::fixture;
    use crate::cells::cell::Cell;
    use crate::frame::state::AppState;

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

    /// A slash command goes in the conversation's record as you typed it, one handled on this
    /// screen alone too, except one that leaves the conversation.
    #[test]
    fn a_typed_command_is_kept_as_typed() {
        let mut screen = Screen::idle();
        assert_eq!(
            screen.submit("/usage  week").typed.as_deref(),
            Some("/usage  week")
        );
        assert_eq!(
            screen.submit("/artifacts").typed.as_deref(),
            Some("/artifacts")
        );
        assert_eq!(screen.submit("/clear").typed, None);
        assert_eq!(screen.submit("why?").typed, None);
    }

    /// `/artifacts` opens the list on this screen, and nothing goes to the controller.
    #[test]
    fn artifacts_opens_the_list_of_published_pages() {
        let mut screen = Screen::idle();
        let out = screen.submit("/artifacts");
        assert!(out.commands.is_empty(), "{:?}", out.commands);
        assert_eq!(out.views.len(), 1);
        assert!(format!("{:?}", out.views[0]).starts_with("ArtifactsView"));
    }

    /// A bare `/bottom-line` opens its picker on this screen; its typed forms go to the
    /// controller, which saves them.
    #[test]
    fn bottom_line_opens_its_picker_and_its_words_go_on() {
        let mut screen = Screen::idle();
        let out = screen.submit("/bottom-line");
        assert!(out.commands.is_empty(), "{:?}", out.commands);
        assert_eq!(out.views.len(), 1);
        assert!(format!("{:?}", out.views[0]).starts_with("LinePicker"));

        let out = screen.submit("/bottom-line add agy");
        assert!(out.views.is_empty());
        assert!(
            matches!(
                out.commands.as_slice(),
                [UiCommand::Slash {
                    command: SlashCommand::BottomLine {
                        change: LineChange::Add { .. },
                        scope: None
                    }
                }]
            ),
            "{:?}",
            out.commands
        );
    }

    /// `/overview` opens the usage panel on its Overview tab, which asks for its numbers.
    #[test]
    fn overview_opens_the_panel_on_its_overview_tab() {
        let mut screen = Screen::idle();
        let mut out = screen.submit("/overview");
        assert_eq!(
            out.commands,
            [UiCommand::Request {
                view: argonauts_core::app::ViewRequest::Overview {
                    period: argonauts_core::app::overview::OverviewPeriod::All,
                    import: None,
                },
            }]
        );
        assert_eq!(out.views.len(), 1);
        let view = out.views.remove(0);
        assert!(format!("{view:?}").contains("tab: Overview"), "{view:?}");
    }

    #[test]
    fn the_main_agents_commands_join_the_menu_and_parse() {
        let mut screen = Screen::idle();
        let set = crate::bottom::menu::commands::tests::claude_commands();
        screen.event(AppEvent::CliCommands { set: set.clone() });
        let menu = &screen.model.bottom.menus.commands;
        assert!(
            menu.iter()
                .any(|item| item.name == "ship" && item.group.as_deref() == Some("Claude Code"))
        );
        screen.state.cli_commands = Some(set);

        assert_eq!(
            screen.submit("/ship  main").commands,
            [slash(SlashCommand::Cli {
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
        let mut state = fixture::working_state();
        // A run with agents and no workflow opens its newest agent instead.
        if let Some(run) = state.runs.get_mut(&fixture::run_id()) {
            run.workflows.push(crate::frame::state::WorkflowState {
                id: argonauts_core::base::names::WorkflowId::parse("wf-001").unwrap(),
                name: "export-json".into(),
                from: None,
                description: "Add a --json flag to export".into(),
                phases: Vec::new(),
                parent: None,
                steps: Vec::new(),
            });
        }
        let mut screen = Screen::new(state);
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
            [slash(SlashCommand::Status {
                action: ViewAction::Open
            })]
        );
        assert_eq!(
            screen.submit("/captain fix the export").commands,
            [slash(SlashCommand::Captain {
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
            Some("@ needs a target, such as @codex or @task-002")
        );
        assert_eq!(screen.model.bottom.composer.text(), "something else");
        assert!(screen.model.history.cells().is_empty());
    }

    /// A prompt that starts with a one-word path goes to the agent whole, where a name that
    /// is neither a command nor on the disk is refused.
    #[test]
    fn a_prompt_that_starts_with_a_one_word_path_is_submitted() {
        let mut screen = Screen::idle();
        let out = screen.submit("/tmp is full, clean it");
        assert_eq!(
            out.commands,
            [UiCommand::Submit {
                text: "/tmp is full, clean it".into(),
                workflow_requested: false,
                pictures: Vec::new(),
            }]
        );
        let out = screen.submit("/no-such-folder-here is full");
        assert!(out.commands.is_empty(), "{:?}", out.commands);
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
                text: "look at line 1\nline 2\nline 3\nline 4".into(),
                workflow_requested: false,
                pictures: Vec::new(),
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

    /// A council reads words only, so while one talks a prompt with a picture still goes out
    /// with its picture.
    #[test]
    fn a_picture_goes_with_its_prompt_while_a_council_talks() {
        use crossterm::event::{KeyCode, KeyEvent};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        std::fs::write(&path, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();
        let mut screen = Screen::idle();
        screen.model.bottom.council.talking = true;
        let composer = &mut screen.model.bottom.composer;
        composer.handle_paste(&path.display().to_string());
        let crate::bottom::ComposerEvent::Submitted { text, shown } =
            composer.handle_key(KeyEvent::from(KeyCode::Enter))
        else {
            panic!("not sent");
        };
        let out = screen.act(UiAction::Submit { text, shown });
        let [UiCommand::Submit { pictures, .. }] = out.commands.as_slice() else {
            panic!("{:?}", out.commands);
        };
        assert_eq!(pictures.len(), 1);
    }

    /// A picture dropped into the input goes out with the prompt, and its echo has a row for
    /// it beneath, as in Claude Code.
    #[test]
    fn a_picture_goes_out_with_the_prompt_and_its_echo_has_a_row_for_it() {
        use crossterm::event::{KeyCode, KeyEvent};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        std::fs::write(&path, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();
        let mut screen = Screen::idle();
        let composer = &mut screen.model.bottom.composer;
        composer.handle_paste("what's off here?");
        composer.handle_paste(&path.display().to_string());
        let crate::bottom::ComposerEvent::Submitted { text, shown } =
            composer.handle_key(KeyEvent::from(KeyCode::Enter))
        else {
            panic!("not sent");
        };
        let out = screen.act(UiAction::Submit { text, shown });
        let [UiCommand::Submit { text, pictures, .. }] = out.commands.as_slice() else {
            panic!("{:?}", out.commands);
        };
        assert_eq!(text, "what's off here? [Image #1]", "parsed, so trimmed");
        assert_eq!(pictures.len(), 1);
        assert_ne!(
            pictures[0].path, path,
            "the copy kept when it was dropped goes"
        );
        assert_eq!(
            std::fs::read(&pictures[0].path).unwrap(),
            std::fs::read(&path).unwrap()
        );
        let echoes: Vec<_> = screen.model.history.active_cells().collect();
        let [(_, Cell::User(echo))] = echoes.as_slice() else {
            panic!("one echo");
        };
        assert_eq!(echo.pictures, [1]);
    }

    /// The typed word asks for a workflow for that one request, unless Opt+W
    /// ignored it. The text goes out as typed either way.
    #[test]
    fn the_word_ultracode_marks_the_request_unless_it_was_ignored() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut screen = Screen::idle();
        let send = |screen: &mut Screen, text: &str, ignore: bool| {
            let composer = &mut screen.model.bottom.composer;
            composer.set_text(text);
            if ignore {
                composer.toggle_workflow_word();
            }
            let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
            let crate::bottom::ComposerEvent::Submitted { text, shown } =
                composer.handle_key(enter)
            else {
                panic!("the prompt goes out");
            };
            screen.act(UiAction::Submit { text, shown }).commands
        };
        let submit = |text: &str, workflow_requested: bool| UiCommand::Submit {
            text: text.into(),
            workflow_requested,
            pictures: Vec::new(),
        };
        let asked = "ultracode fix the flaky export test";
        assert_eq!(send(&mut screen, asked, false), [submit(asked, true)]);
        assert_eq!(send(&mut screen, asked, true), [submit(asked, false)]);
        let plain = "fix the flaky export test";
        assert_eq!(send(&mut screen, plain, false), [submit(plain, false)]);
    }

    #[test]
    fn keywords_reach_the_input_and_echo_and_the_text_goes_out_as_typed() {
        use argonauts_core::look::keywords::{Keyword, KeywordLook};

        let ultrathink = Keyword::new("ultrathink", KeywordLook::Rainbow, true);
        let mut screen = Screen::idle();
        let set = crate::bottom::menu::commands::tests::claude_commands()
            .with_keywords(std::slice::from_ref(&ultrathink));
        screen.event(AppEvent::CliCommands { set: set.clone() });
        assert_eq!(
            screen.model.bottom.composer.keywords,
            std::slice::from_ref(&ultrathink)
        );
        screen.state.cli_commands = Some(set);

        let out = screen.submit("please ultrathink about this");
        assert_eq!(
            out.commands,
            [UiCommand::Submit {
                text: "please ultrathink about this".into(),
                workflow_requested: false,
                pictures: Vec::new(),
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
                target: MentionTarget::Worker(ModelRef::new("claude-code", None)),
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
                text: "@README.md what is this?".into(),
                workflow_requested: false,
                pictures: Vec::new(),
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
                text: "@src/ keep the API".into(),
                workflow_requested: false,
                pictures: Vec::new(),
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
    fn typed_text_never_answers_question_routing() {
        assert!(!typed_text_answers_question());
        let mut screen = Screen::new(fixture::working_state());
        let mut question = QuestionPrompt::new(
            7,
            "Merge task-002 into main?",
            "Merge task-002?",
            vec![QuestionOption::new("Merge")],
        );
        question.focus();
        screen.model.bottom.question = Some(question);
        let out = screen.submit("yes, merge it");
        assert_eq!(
            out.commands,
            [UiCommand::Submit {
                text: "yes, merge it".into(),
                workflow_requested: false,
                pictures: Vec::new(),
            }]
        );
        assert!(screen.model.bottom.question.is_some());
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
                text: String::new(),
                workflow_requested: false,
                pictures: Vec::new(),
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
                .any(|c| c.name == "crew" && c.needs_argument)
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
    fn the_first_start_after_an_update_keeps_its_row_for_the_input() {
        let mut screen = Screen::idle();
        screen.event(AppEvent::Updated {
            words: "Updated to 0.1.1 · /release-notes".into(),
        });
        assert_eq!(
            screen.model.bottom.updated.as_deref(),
            Some("Updated to 0.1.1 · /release-notes")
        );
    }

    #[test]
    fn the_welcome_brings_the_inputs_example() {
        let mut screen = Screen::idle();
        let welcome = |example: Option<&str>| AppEvent::Welcome {
            version: "0.1.0".into(),
            repo: "~/argonauts".into(),
            workflow: "council".into(),
            agents: Vec::new(),
            runs_going: 0,
            runs_need_you: 0,
            example_prompt: example.map(str::to_owned),
            main_agent: None,
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

    /// `ui.agents_view` opens the agent panel with the first welcome only, as ← would: a
    /// welcome after the setup saves leaves the screen as it is.
    #[test]
    fn the_agents_view_opens_at_start_when_asked() {
        let welcome = || AppEvent::Welcome {
            version: "0.1.0".into(),
            repo: "~/argonauts".into(),
            workflow: "council".into(),
            agents: Vec::new(),
            runs_going: 0,
            runs_need_you: 0,
            example_prompt: None,
            main_agent: None,
            effort: None,
        };
        let mut screen = Screen::idle();
        assert!(screen.event(welcome()).views.is_empty(), "off by default");

        let mut screen = Screen::idle();
        screen.settings.agents_view = true;
        let first = screen.event(welcome());
        assert_eq!(first.views.len(), 1);
        // It asks for its rows as it opens, as the panel does however it opens.
        assert_eq!(
            crate::frame::view::View::asks_on_open(first.views[0].as_ref()),
            Some(ViewRequest::Crew)
        );
        assert!(screen.event(welcome()).views.is_empty());
    }

    /// Saving `workflows.ultracode_trigger` changes what the word does in the input at once.
    #[test]
    fn the_workflow_word_follows_its_setting() {
        let mut screen = Screen::idle();
        let changed = |value: &str| AppEvent::SettingChanged {
            key: argonauts_core::settings::Settings::ULTRACODE_TRIGGER.to_owned(),
            value: value.to_owned(),
            scope: argonauts_core::settings::SettingScope::Global,
            previous: None,
            reset: false,
            why: None,
            kept: false,
        };
        screen.event(changed("false"));
        assert!(screen.model.bottom.composer.workflow_word_off);
        screen.event(changed("true"));
        assert!(!screen.model.bottom.composer.workflow_word_off);
    }

    #[test]
    fn copy_puts_an_answer_on_screen_on_the_clipboard() {
        let mut screen = Screen::idle();
        let none = screen.submit("/copy");
        assert_eq!(none.clipboard, None);
        assert_eq!(screen.notice(), Some("No answer to copy yet."));

        let history = &mut screen.model.history;
        history.push(crate::cells::cell::AssistantMessage::new("First answer.\n"));
        history.push(UserMessage::new("and then?"));
        history.push(crate::cells::cell::AssistantMessage::new(
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

    /// `/usage` with no word opens on the period the Usage tab opens on, All unless a setting
    /// says otherwise; a word still picks its period.
    #[test]
    fn bare_usage_opens_on_all() {
        let all = UsagePeriod::All;
        assert_eq!(usage_period(None, all), Some(UsagePeriod::All));
        assert_eq!(
            usage_period(None, UsagePeriod::Week),
            Some(UsagePeriod::Week)
        );
        assert_eq!(usage_period(Some("today"), all), Some(UsagePeriod::Today));
        assert_eq!(usage_period(Some("WEEKLY"), all), Some(UsagePeriod::Week));
        assert_eq!(usage_period(Some("yearly"), all), None);
    }

    /// `ui.views.usage.period` is the period a bare `/usage` opens the tab on, and the Limits
    /// tab still opens on All.
    #[test]
    fn the_usage_tab_opens_on_the_period_its_setting_names() {
        use argonauts_core::settings::ui::UsagePeriod as Setting;
        let mut screen = Screen::idle();
        screen.settings.views.usage.period = Setting::Week;
        let out = screen.submit("/usage");
        assert_eq!(out.views.len(), 1);
        let opened = format!("{:?}", out.views[0]);
        assert!(opened.contains("period: Week"), "{opened}");
        let out = screen.submit("/limits");
        let opened = format!("{:?}", out.views[0]);
        assert!(opened.contains("period: All"), "{opened}");
        assert!(
            out.commands.contains(&slash(SlashCommand::Usage {
                period: Some("all".to_owned()),
                action: ViewAction::Open,
            })),
            "{:?}",
            out.commands
        );
    }
}
