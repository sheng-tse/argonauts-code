use argonauts_core::app::{AppEvent, ConversationEvent, ShellEvent, TurnReply};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::ApprovalEvent;
use argonauts_core::events::run::Party;
use argonauts_core::settings::protocol::CaptainKind;
use argonauts_core::{AgentEvent, NoteLevel};

use crate::bridge::tools::is_own;
use crate::state::AppState;

use super::agent::agent_lines;
use super::questions::QuestionList;
use super::run::run_lines;
use super::views::view_lines;
use super::words::{labeled, spoken, word};

/// The flat lines one event adds, already sanitized. `state` has the event folded in. Streamed
/// text says nothing here: a whole message or answer says it all, and [`super::Reader`] speaks
/// streamed text a line at a time as it comes.
pub fn lines_for(event: &AppEvent, state: &AppState) -> Vec<String> {
    let lines = match event {
        AppEvent::Welcome { .. } => welcome_lines(state),
        AppEvent::Attached { summary, .. } => vec![format!(
            "run: {}, {}, attached",
            summary.title,
            word(&summary.state)
        )],
        AppEvent::Detached { run } => vec![format!("run: {run} detached")],
        // Replayed events rebuild the state quietly; the condensed replay speaks for them.
        AppEvent::Run { .. } | AppEvent::Conversation { .. } if state.replaying() => Vec::new(),
        AppEvent::Run { event } => run_lines(event, state),
        AppEvent::Conversation { event } => conversation_lines(event, state),
        AppEvent::Choice { choice } => QuestionList::choice(choice).lines(),
        AppEvent::ChoiceClosed { outcome, .. } => labeled("question", outcome),
        AppEvent::Narrator { line } => labeled("narrator", &line.text),
        AppEvent::Notice { .. } if state.notice_repeated => Vec::new(),
        AppEvent::Notice { level, text } => {
            let label = match level {
                NoteLevel::Info | NoteLevel::Aside | NoteLevel::Quiet => "notice",
                NoteLevel::Warn | NoteLevel::Caution => "warning",
                NoteLevel::Error => "error",
            };
            labeled(label, text)
        }
        AppEvent::SettingRefused { why, .. } => labeled("error", why),
        AppEvent::Hint { text } => labeled("notice", text),
        AppEvent::View { data } => view_lines(data, &state.names),
        AppEvent::Recap { text, .. } => labeled("recap", text),
        AppEvent::TakenBack { texts } => texts
            .iter()
            .flat_map(|text| labeled("taken back", text))
            .collect(),
        AppEvent::CliCommands { .. }
        | AppEvent::Names { .. }
        | AppEvent::RepoFiles { .. }
        | AppEvent::BackgroundRuns { .. }
        | AppEvent::PastBegins
        | AppEvent::PastDrawn => Vec::new(),
        // The screen starts over, which these modes never do, so a line says so.
        AppEvent::ConversationCleared { way_back } => labeled(
            "notice",
            &match way_back {
                Some(old) => {
                    format!(
                        "Started a new conversation. /resume {old} picks the last one up again."
                    )
                }
                None => "Started a new conversation.".to_owned(),
            },
        ),
        AppEvent::SettingChanged {
            key,
            value,
            scope,
            reset,
            ..
        } => {
            let value = if value.trim().is_empty() {
                "unset"
            } else {
                value.trim()
            };
            if *reset {
                vec![format!(
                    "setting: {key} reset in {}, now {value}",
                    word(scope)
                )]
            } else {
                vec![format!("setting: {key} is {value} in {}", word(scope))]
            }
        }
        AppEvent::ConversationResumed {
            conversation,
            title,
            turns,
            ..
        } => {
            let mut lines = vec![format!(
                "conversation: resumed {conversation}, {}",
                title.trim()
            )];
            lines.extend(
                turns
                    .iter()
                    .flat_map(|turn| labeled(&party(&turn.from, &state.names), &turn.text)),
            );
            lines
        }
        AppEvent::Report { title, lines } => {
            let mut said = labeled("report", title);
            said.extend(lines.iter().cloned());
            said
        }
        // Said once, whole, when its report comes.
        AppEvent::ReportSoFar { .. } | AppEvent::CommandWorking { .. } => Vec::new(),
        AppEvent::Templates { list } => {
            let said = |label: &str, entries: &[argonauts_core::template_list::TemplateEntry]| {
                let names: Vec<String> = entries
                    .iter()
                    .map(|entry| {
                        let mut name = entry.name.clone();
                        if entry.in_use {
                            name.push_str(" (in use)");
                        }
                        if let Some(changed) = entry.changed {
                            name.push(' ');
                            name.push_str(changed.mark());
                        }
                        name
                    })
                    .collect();
                format!("{label}: {}", names.join(", "))
            };
            vec![
                said("workflows", &list.workflows),
                said("prompts", &list.prompts),
            ]
        }
        AppEvent::Switched { text, undo, caveat } => {
            let mut said = vec![text.clone()];
            said.extend(undo.iter().chain(caveat).cloned());
            said
        }
        // A reader can't unfold it, so the whole text is read.
        AppEvent::Folded { title, lines, more } => {
            let mut said = labeled("report", title);
            said.extend(lines.iter().chain(more).cloned());
            said
        }
        AppEvent::Record {
            title,
            tail,
            lines,
            notes,
        } => {
            let head = match tail {
                Some(tail) => format!("{title} · {tail}"),
                None => title.clone(),
            };
            let mut said = labeled("report", &head);
            said.extend(lines.iter().chain(notes).cloned());
            said
        }
        AppEvent::Continued { text } => vec![text.clone()],
        AppEvent::Shell { event } => shell_lines(event),
        // A line-mode screen never gives the terminal away, so you run it yourself.
        AppEvent::RunInTerminal { argv, .. } => labeled(
            "run",
            &format!("run `{}` in another terminal.", argv.join(" ")),
        ),
        // Drawn by the screen's own lines, redrawn in place, or handled by the reader.
        AppEvent::NarratorStatus { .. }
        | AppEvent::Mode { .. }
        | AppEvent::Status { .. }
        | AppEvent::TakeOver { .. }
        | AppEvent::UiSettings { .. }
        | AppEvent::ReplayDone { .. }
        | AppEvent::Exit => Vec::new(),
    };
    lines
        .iter()
        .map(|line| {
            // Keep the indent of a question's list; it reads as nothing.
            let indent = line.len() - line.trim_start_matches(' ').len();
            format!("{}{}", &line[..indent], spoken(line))
        })
        .filter(|line| !line.trim().is_empty())
        .collect()
}

/// Whether `event` needs you now, so the bell rings. Never while replaying.
pub fn rings_for(event: &AppEvent, state: &AppState) -> bool {
    if state.replaying() {
        return false;
    }
    match event {
        AppEvent::Choice { choice } => choice.bell,
        AppEvent::Run { event } => matches!(
            &event.kind,
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.needs_you
        ),
        _ => false,
    }
}

fn welcome_lines(state: &AppState) -> Vec<String> {
    let Some(welcome) = &state.welcome else {
        return Vec::new();
    };
    let mut lines = vec![format!(
        "Argonauts Code {}, {}, workflow {}",
        welcome.version, welcome.repo, welcome.workflow
    )];
    for agent in &welcome.agents {
        let mut line = format!("agent: {}", state.names.cli(&agent.id));
        if let Some(version) = &agent.version {
            line.push_str(&format!(" {version}"));
        }
        line.push_str(if agent.ready {
            ", ready"
        } else {
            ", not ready"
        });
        if let Some(note) = &agent.note {
            line.push_str(&format!(", {note}"));
        }
        lines.push(line);
    }
    let mut runs = Vec::new();
    if welcome.runs_going > 0 {
        runs.push(format!("{} going", welcome.runs_going));
    }
    if welcome.runs_need_you > 0 {
        runs.push(format!("{} need you", welcome.runs_need_you));
    }
    if !runs.is_empty() {
        lines.push(format!("runs: {}. /resume lists them.", runs.join(", ")));
    }
    lines.push("Type a message and press Enter. /help lists the commands. Ctrl+D leaves.".into());
    lines
}

/// A `!` command reads as you typed it, its output line by line, and how it ended.
fn shell_lines(event: &ShellEvent) -> Vec<String> {
    match event {
        ShellEvent::Started { command, .. } => labeled("shell", &format!("! {command}")),
        ShellEvent::Output { text, .. } => labeled("output", text),
        ShellEvent::Finished { stopped: true, .. } => vec!["shell: stopped".to_owned()],
        ShellEvent::Finished {
            exit_code: Some(0), ..
        } => vec!["shell: done".to_owned()],
        ShellEvent::Finished {
            exit_code: Some(code),
            ..
        } => vec![format!("shell: exit code {code}")],
        ShellEvent::Finished { .. } => vec!["shell: ended by a signal".to_owned()],
    }
}

fn conversation_lines(event: &ConversationEvent, state: &AppState) -> Vec<String> {
    let agent_id = state
        .conversation
        .worker
        .as_ref()
        .map_or("agent", |worker| worker.agent.as_str());
    match event {
        ConversationEvent::TurnStarted { text, .. } | ConversationEvent::Joined { text, .. } => {
            labeled("you", text)
        }
        // The answer comes whole with the turn's end.
        ConversationEvent::Agent {
            event: AgentEvent::Result { is_error, .. },
            ..
        } => {
            if *is_error {
                vec![format!("{agent_id} error: finished with an error")]
            } else {
                Vec::new()
            }
        }
        // Argonauts Code's own calls, such as the report, say nothing.
        ConversationEvent::Agent {
            event: AgentEvent::ToolFinished { id, .. },
            ..
        } if state
            .conversation
            .tools
            .iter()
            .any(|tool| tool.id == *id && is_own(&tool.name)) =>
        {
            Vec::new()
        }
        ConversationEvent::Agent { event, .. } => {
            agent_lines(agent_id, agent_id, event, state, None, None)
        }
        ConversationEvent::TurnFinished { reply, .. } => match reply {
            TurnReply::Answer { markdown } | TurnReply::Unclear { markdown, .. } => {
                answer_lines(agent_id, markdown)
            }
            TurnReply::Change {
                markdown,
                captain_or_crew,
                ..
            } => {
                let mut lines = answer_lines(agent_id, markdown);
                let how = if captain_or_crew.captain == Some(CaptainKind::Direct) {
                    "this works as Captain in your checkout".to_owned()
                } else {
                    format!("this takes a {} run", word(&captain_or_crew.mode))
                };
                lines.push(format!(
                    "{agent_id}: {how}, {}",
                    captain_or_crew.reason.trim()
                ));
                lines
            }
        },
        ConversationEvent::TurnFailed { error, .. } => labeled(&format!("{agent_id} error"), error),
        ConversationEvent::TurnCancelled { .. } => vec![format!("{agent_id}: stopped")],
        ConversationEvent::TurnStopped { why, .. } => vec![format!("{agent_id}: {why}")],
        ConversationEvent::FellBack {
            from,
            to,
            why,
            signed_out,
            ..
        } => {
            let mut line = format!("{} {why}, so {} continues", from.agent, to.agent);
            if let Some(login) = signed_out.as_ref().and_then(|out| out.login.as_deref()) {
                line.push_str(&format!("; {login} signs it in again"));
            }
            vec![line]
        }
        ConversationEvent::AnswerRetried { .. } => {
            vec![format!(
                "{agent_id}: that answer missed its format, so it is asked again"
            )]
        }
        ConversationEvent::ContextLeft { .. } => Vec::new(),
        ConversationEvent::CommandTyped { text, .. } => vec![format!("you typed {text}")],
        ConversationEvent::CommandReplied { reply } => {
            let words = [
                Some(reply.text.as_str()),
                reply.value.as_deref(),
                reply.after.as_deref(),
            ];
            vec![words.into_iter().flatten().collect::<Vec<_>>().join(" ")]
        }
    }
}

/// An answer's Markdown, one labeled line per line. Code fences are markup, not words.
pub fn answer_lines(who: &str, markdown: &str) -> Vec<String> {
    let text: Vec<&str> = markdown
        .lines()
        .filter(|line| !line.trim_start().starts_with("```"))
        .collect();
    labeled(who, &text.join("\n"))
}

/// Who said something, as its line's label.
pub fn party(from: &Party, names: &DisplayNames) -> String {
    match from {
        Party::User => "you".to_owned(),
        Party::Agent { worker, .. } => names.cli(&worker.agent),
        Party::Judge { model } => format!("judge {}", names.cli(&model.agent)),
        Party::OutsideAgent { client } => client.clone(),
        other => word(other),
    }
}
