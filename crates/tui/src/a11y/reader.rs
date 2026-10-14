//! The screen reader's memory between events: text streamed a line at a time, the question that
//! typed numbers answer, and a take-over waiting for you to come back.

use std::collections::BTreeMap;
use std::path::Path;

use argonauts_core::api::TakeOverTicket;
use argonauts_core::app::{AnswerTarget, AppEvent, ConversationEvent, UiCommand, ViewRequest};
use argonauts_core::cli_commands::CliCommandSet;
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::input::{ParsedInput, SlashCommand, parse_input_with};
use argonauts_core::settings::ui::TakeOverWhere;
use argonauts_core::{AgentEvent, KeyScheme, RunId, TaskId, Verbosity};

use crate::UiOptions;
use crate::bridge::keys::shown_run;
use crate::commands::help_sections;
use crate::mentions::{Resolved, resolve_target};
use crate::runtime::{TakeOverPlan, plan_take_over, write_spec_files};
use crate::state::AppState;
use crate::views::waiting::{self, RunEntry};

use super::answer::Armed;
use super::decisions::Decisions;
use super::lines::{lines_for, rings_for};
use super::offer::Offer;
use super::questions::{ListTarget, QuestionList};
use super::words::{labeled, shows, speaker};

/// What one event or one typed line comes to.
#[derive(Debug, Default, PartialEq)]
pub struct Said {
    pub lines: Vec<String>,
    pub bell: bool,
    pub commands: Vec<UiCommand>,
    /// `/exit`, or the app closed the screen.
    pub exit: bool,
    pub tmux: Option<TmuxTakeOver>,
}

/// A take-over to open in a tmux pane or window. If tmux can't, it runs in another terminal.
#[derive(Debug, Clone, PartialEq)]
pub struct TmuxTakeOver {
    pub argv: Vec<String>,
    pub sentinel: String,
    pub ticket: TakeOverTicket,
    pub run: RunId,
}

/// Text a speaker is streaming: what's not yet a whole line, and the lines already spoken, so
/// a whole message that repeats them stays quiet.
#[derive(Debug, Default)]
struct Stream {
    partial: String,
    spoken: Vec<String>,
}

const CONVERSATION: &str = "conversation";

#[derive(Debug)]
pub struct Reader {
    pub state: AppState,
    streams: BTreeMap<String, Stream>,
    /// The question typed numbers answer.
    pub(super) question: Option<ListTarget>,
    /// Questions answered here that the app hasn't closed yet.
    pub(super) answered: Vec<AnswerTarget>,
    /// Type my own answer was picked without the answer; the next line is it.
    pub(super) own_answer: Option<QuestionList>,
    pub(super) armed: Option<Armed>,
    /// Fallback questions whose Remember this pick is on.
    pub(super) remember: Vec<AnswerTarget>,
    pub(super) decisions: Decisions,
    /// A take-over running in another terminal; the next line says you're back.
    take_over: Option<(RunId, TaskId)>,
    /// What the list just read out offers the next line, such as an approval by number.
    offer: Option<Offer>,
}

impl Reader {
    pub fn new(options: &UiOptions) -> Self {
        let mut state = AppState::default();
        let mut settings = options.settings.clone();
        settings.verbosity = options.verbosity;
        settings.bell = options.bell;
        state.ui = Some(settings);
        Self {
            state,
            streams: BTreeMap::new(),
            question: None,
            answered: Vec::new(),
            own_answer: None,
            armed: None,
            remember: Vec::new(),
            decisions: Decisions::default(),
            take_over: None,
            offer: None,
        }
    }

    /// Folds `event` in and says what it adds.
    pub fn event(&mut self, event: &AppEvent) -> Said {
        self.decisions.saw(event, &self.state);
        self.state.apply(event);
        let mut said = Said::default();
        match event {
            AppEvent::Exit => {
                said.exit = true;
                return said;
            }
            AppEvent::TakeOver { ticket, place, run } => {
                return self.take_over(ticket, *place, run);
            }
            _ => {}
        }
        if let Some((key, who, text)) = self.delta(event) {
            said.lines = self.stream(key, &who, text);
            return said;
        }
        let mut lines = lines_for(event, &self.state);
        if let AppEvent::View { data } = event {
            self.offer = Offer::for_view(data);
            lines.extend(self.offer.as_ref().map(|offer| offer.hint().to_owned()));
        }
        said.lines = match self.closes(event) {
            Some(Close::Whole { key, ends }) => self.whole(&key, lines, ends),
            Some(Close::Tail { key, who, ends }) => {
                let mut spoken = self.flush(&key, &who, ends);
                spoken.extend(lines);
                spoken
            }
            None => lines,
        };
        said.bell = rings_for(event, &self.state);
        self.follow_questions(event, &mut said);
        said
    }

    /// Reads one typed line: the end of a take-over, your own answer, a question's answer, a
    /// command, or a message.
    pub fn typed(&mut self, line: &str) -> Said {
        let mut said = Said::default();
        if let Some((run, task)) = self.take_over.take() {
            said.lines.push("take over: back".to_owned());
            said.commands.push(UiCommand::TakeOverEnded {
                run,
                task,
                exit_code: None,
            });
            return said;
        }
        if let Some(question) = self.own_answer.take() {
            self.own_answer(question, line, &mut said);
            return said;
        }
        if let Some(taken) = self.offer.take().and_then(|offer| offer.take(line)) {
            said.lines = taken.lines;
            said.commands.extend(taken.command);
            return said;
        }
        // `!` runs the rest in your shell, as it does in the full screen.
        if let Some(command) = line.trim().strip_prefix('!') {
            if !command.trim().is_empty() {
                said.commands.push(UiCommand::Shell {
                    command: command.trim().to_owned(),
                });
            }
            return said;
        }
        match parse_input_with(line, self.state.cli_commands.as_ref()) {
            Ok(ParsedInput::Empty) => {}
            Ok(ParsedInput::Slash { command }) => self.slash(command, &mut said),
            Ok(ParsedInput::Mention { target, text }) => match resolve_target(&target, &self.state)
            {
                Resolved::One(target) => said.commands.push(UiCommand::Mention { target, text }),
                Resolved::Ambiguous(choices) => {
                    let names: Vec<String> = choices.into_iter().map(|(name, _)| name).collect();
                    said.lines.push(format!(
                        "warning: several agents answer to that name: {}. Nothing was sent.",
                        names.join(", ")
                    ));
                }
                Resolved::Unknown => said.lines.push(
                    "warning: no one in this run answers to that name, so nothing was sent.".into(),
                ),
            },
            Ok(ParsedInput::Plain { text }) => self.plain(text, &mut said),
            Err(error) => said.lines.push(format!("error: {error}")),
        }
        said
    }

    /// Stdin ended: Ctrl+D, or the input was a file that ran out.
    pub fn input_ended(&mut self) -> Said {
        Said {
            commands: vec![UiCommand::Exit],
            exit: true,
            ..Said::default()
        }
    }

    /// A tmux take-over came back, or tmux couldn't open it.
    pub fn tmux_returned(&mut self, job: TmuxTakeOver, opened: std::io::Result<()>) -> Said {
        match opened {
            Ok(()) => Said {
                lines: vec!["take over: back".to_owned()],
                commands: vec![UiCommand::TakeOverEnded {
                    run: job.run,
                    task: job.ticket.task,
                    exit_code: None,
                }],
                ..Said::default()
            },
            Err(error) => {
                let mut said = self.take_over(&job.ticket, TakeOverWhere::Here, &job.run);
                said.lines.insert(
                    0,
                    format!("warning: tmux couldn't open the session ({error})."),
                );
                said
            }
        }
    }

    fn slash(&mut self, command: SlashCommand, said: &mut Said) {
        match command {
            SlashCommand::Exit => {
                said.commands.push(UiCommand::Exit);
                said.exit = true;
            }
            SlashCommand::Help => said
                .lines
                .extend(help_lines(self.state.cli_commands.as_ref())),
            // One line each, newest first, with the link to copy from the terminal.
            SlashCommand::Artifacts => {
                let published = self.state.artifacts.all();
                if published.is_empty() {
                    said.lines
                        .push(crate::views::artifacts::NONE_YET.to_owned());
                }
                said.lines.extend(published.iter().rev().map(|published| {
                    format!(
                        "artifact {}: {}",
                        published.artifact.name, published.artifact.url
                    )
                }));
            }
            // What the run's row opens on screen: the main agent's session while it
            // designs, else the workflow, printed as text.
            SlashCommand::Workflows => match shown_run(&self.state).map(|run| run.id.clone()) {
                Some(run) => match waiting::entry(&self.state, &run) {
                    RunEntry::Session(session) => said.commands.push(UiCommand::Request {
                        view: ViewRequest::Agent { run, session },
                    }),
                    RunEntry::Workflow => said.commands.push(UiCommand::Request {
                        view: ViewRequest::Workflow {
                            run,
                            workflow: None,
                        },
                    }),
                    RunEntry::Waiting => said
                        .lines
                        .push(waiting::waiting_for(self.state.runs.get(&run))),
                },
                None => said.commands.push(UiCommand::Request {
                    view: ViewRequest::WorkflowLibrary,
                }),
            },
            SlashCommand::Verbosity { level: Some(level) } => self.set_verbosity(level, said),
            SlashCommand::Vim => {
                let on = self
                    .state
                    .ui
                    .as_ref()
                    .is_none_or(|ui| ui.keys != KeyScheme::Vim);
                said.commands.push(UiCommand::Vim { on });
            }
            SlashCommand::Theme { name: None } => said.commands.push(UiCommand::Request {
                view: ViewRequest::Settings {
                    section: Some("ui".into()),
                },
            }),
            command => said.commands.push(UiCommand::Slash { command }),
        }
    }

    fn set_verbosity(&mut self, level: Verbosity, said: &mut Said) {
        if let Some(ui) = self.state.ui.as_mut() {
            ui.verbosity = level;
        }
        said.commands.push(UiCommand::Verbosity { level });
    }

    fn take_over(&mut self, ticket: &TakeOverTicket, place: TakeOverWhere, run: &RunId) -> Said {
        let mut said = Said::default();
        if let Some(spec) = &ticket.command
            && let Err(error) = write_spec_files(spec)
        {
            said.lines.push(format!(
                "warning: couldn't write the session's files: {error}"
            ));
        }
        match plan_take_over(ticket, place) {
            TakeOverPlan::Refused(reason) => {
                said.lines.extend(labeled("take over", &reason));
                said.commands.push(UiCommand::TakeOverEnded {
                    run: run.clone(),
                    task: ticket.task.clone(),
                    exit_code: None,
                });
            }
            TakeOverPlan::Here { argv, cwd, notice } => {
                said.lines.extend(here_lines(&argv, &cwd));
                if let Some(notice) = notice {
                    said.lines.extend(labeled("take over", &notice));
                }
                self.take_over = Some((run.clone(), ticket.task.clone()));
            }
            TakeOverPlan::TmuxSplit { argv, sentinel }
            | TakeOverPlan::NewTab { argv, sentinel } => {
                said.lines
                    .push("take over: the session opened in a tmux pane.".to_owned());
                said.tmux = Some(TmuxTakeOver {
                    argv,
                    sentinel,
                    ticket: ticket.clone(),
                    run: run.clone(),
                });
            }
        }
        said
    }

    /// The speaker, stream key, and text of a streamed piece of an answer.
    fn delta<'e>(&self, event: &'e AppEvent) -> Option<(String, String, &'e str)> {
        if self.state.replaying() || !shows(&self.state, Verbosity::Normal) {
            return None;
        }
        match event {
            AppEvent::Conversation {
                event:
                    ConversationEvent::Agent {
                        turn,
                        event: AgentEvent::TextDelta { text, parent: None },
                    },
            } if self.state.conversation.turn.as_ref() == Some(turn) => {
                Some((CONVERSATION.to_owned(), self.conversation_speaker(), text))
            }
            AppEvent::Run { event } => match &event.kind {
                RunEventKind::Agent(AgentEvent::TextDelta { text, parent: None })
                    if event.session.is_some() =>
                {
                    Some((session_key(event), self.run_speaker(event), text))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether `event` ends what a speaker was streaming, and how.
    fn closes(&self, event: &AppEvent) -> Option<Close> {
        match event {
            AppEvent::Conversation { event } => {
                let key = CONVERSATION.to_owned();
                let who = self.conversation_speaker();
                match event {
                    ConversationEvent::TurnStarted { .. } => Some(Close::Tail {
                        key,
                        who,
                        ends: true,
                    }),
                    ConversationEvent::Agent { event, .. } => match event {
                        AgentEvent::Message { parent: None, .. } => {
                            Some(Close::Whole { key, ends: false })
                        }
                        AgentEvent::ToolStarted { parent: None, .. }
                        | AgentEvent::Result { .. }
                        | AgentEvent::Error { .. } => Some(Close::Tail {
                            key,
                            who,
                            ends: false,
                        }),
                        _ => None,
                    },
                    ConversationEvent::TurnFinished { .. } => {
                        Some(Close::Whole { key, ends: true })
                    }
                    ConversationEvent::FellBack { .. }
                    | ConversationEvent::Joined { .. }
                    | ConversationEvent::AnswerRetried { .. } => Some(Close::Tail {
                        key,
                        who,
                        ends: false,
                    }),
                    ConversationEvent::TurnFailed { .. }
                    | ConversationEvent::TurnCancelled { .. } => Some(Close::Tail {
                        key,
                        who,
                        ends: true,
                    }),
                }
            }
            AppEvent::Run { event } if event.session.is_some() => {
                let key = session_key(event);
                match &event.kind {
                    RunEventKind::Agent(AgentEvent::Message { parent: None, .. }) => {
                        Some(Close::Whole { key, ends: true })
                    }
                    RunEventKind::Agent(
                        AgentEvent::ToolStarted { parent: None, .. }
                        | AgentEvent::Result { .. }
                        | AgentEvent::Error { .. },
                    )
                    | RunEventKind::Session(SessionEvent::Ended(_)) => Some(Close::Tail {
                        key,
                        who: self.run_speaker(event),
                        ends: true,
                    }),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Adds `text` to the stream and speaks each line it completes.
    fn stream(&mut self, key: String, who: &str, text: &str) -> Vec<String> {
        let stream = self.streams.entry(key).or_default();
        stream.partial.push_str(text);
        let mut lines = Vec::new();
        while let Some(end) = stream.partial.find('\n') {
            let line: String = stream.partial.drain(..=end).collect();
            lines.extend(labeled(who, &line));
        }
        stream.spoken.extend(lines.iter().cloned());
        lines
    }

    /// Speaks what's left of a line still streaming.
    fn flush(&mut self, key: &str, who: &str, ends: bool) -> Vec<String> {
        let Some(stream) = self.streams.get_mut(key) else {
            return Vec::new();
        };
        let lines = labeled(who, &std::mem::take(&mut stream.partial));
        stream.spoken.extend(lines.iter().cloned());
        if ends {
            self.streams.remove(key);
        }
        lines
    }

    /// A whole message or answer: only the lines streaming hasn't said yet.
    fn whole(&mut self, key: &str, lines: Vec<String>, ends: bool) -> Vec<String> {
        let Some(stream) = self.streams.get_mut(key) else {
            return lines;
        };
        stream.partial.clear();
        // Each spoken line matches one line of the message at most, so a line said twice
        // in the message is said again.
        let mut pool = stream.spoken.clone();
        let mut unsaid = Vec::new();
        for line in lines {
            match pool.iter().position(|spoken| *spoken == line) {
                Some(at) => {
                    pool.remove(at);
                }
                None => unsaid.push(line),
            }
        }
        if ends {
            self.streams.remove(key);
        } else {
            stream.spoken.extend(unsaid.iter().cloned());
        }
        unsaid
    }

    fn conversation_speaker(&self) -> String {
        self.state
            .conversation
            .worker
            .as_ref()
            .map_or_else(|| "agent".to_owned(), |worker| worker.agent.clone())
    }

    fn run_speaker(&self, event: &RunEvent) -> String {
        speaker(
            &self.state,
            &event.run_id,
            event.session.as_ref(),
            &event.source,
        )
    }
}

enum Close {
    /// A whole message: lines streaming already said stay quiet.
    Whole { key: String, ends: bool },
    /// Something else from the speaker: the line still streaming is said first.
    Tail {
        key: String,
        who: String,
        ends: bool,
    },
}

fn session_key(event: &RunEvent) -> String {
    let session = event
        .session
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    format!("{}/{session}", event.run_id)
}

fn here_lines(argv: &[String], cwd: &Path) -> Vec<String> {
    vec![
        format!(
            "take over: run this in another terminal, in {}:",
            cwd.display()
        ),
        shell_words::join(argv),
        "take over: press Enter here when you're done.".to_owned(),
    ]
}

/// The commands under their categories, as the `/` menu and `/help` group them.
fn help_lines(agent_id: Option<&CliCommandSet>) -> Vec<String> {
    let mut lines = Vec::new();
    for section in help_sections(agent_id) {
        lines.push(if section.runs_here {
            format!("help: {} commands:", section.heading)
        } else {
            format!("help: {}:", section.heading)
        });
        for (usage, help) in section.rows {
            lines.push(format!("help: {usage}, {help}"));
        }
    }
    lines.push("help: answer a question with its number or letter, and put a note after a colon. Ctrl+D leaves.".to_owned());
    lines
}
