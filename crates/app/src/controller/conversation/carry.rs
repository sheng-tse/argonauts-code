//! A new main model mid-conversation. On the same CLI the session carries on with the new
//! model. On another CLI the new session starts from what `switch.carry` says: by default a
//! summary the model being left writes in its own session with `/compact`'s prompt, the recent
//! exchanges in full, and the transcript's path. The summary is one short turn made right after
//! the switch, unseen but for its folded result. When the model being left can't write it, the
//! new model writes it from the conversation's record. Switching back before you send anything
//! cancels it, and the conversation carries on in the session it left.

use argonauts_core::NoteLevel;
use argonauts_core::app::AppEvent;
use argonauts_core::names::{ModelRef, TurnId};
use argonauts_core::settings::switch::Carry;
use argonauts_core::steps::{StepError, StepOutcome};

use super::excerpt::{self, Exchange, Summary, exchanges};
use super::lifecycle::begin_carry;
use super::{ConversationState, Turn, send_queued, settle_mode, stop_turn};
use crate::controller::captain_or_crew::{Reading, read_answer};
use crate::controller::turn::{self, TurnStep};
use crate::controller::{Ctx, Effect};
use crate::conversation_store::ConversationSession;
use argonauts_core::events::session::InputPart;

/// Carry turns number apart from the conversation's, and neither the screen nor the journal
/// sees them: only the summary they write shows.
const CARRY_PREFIX: &str = "t-carry-";
/// How much of the conversation's record the new model reads when it writes the summary.
const RECORD_CHARS: usize = 120_000;
/// The summary's lines shown before Ctrl+O.
const SHOWN_LINES: usize = 2;

/// What a switch of the main model carried, until the conversation moves on.
#[derive(Debug, Default)]
pub(super) struct Carried {
    /// The session of the CLI being left, and the model it ran, kept until you send something,
    /// so switching back carries on in it.
    left: Option<(ConversationSession, Option<String>)>,
    /// A summary is still to be made, as when the switch came while a turn ran.
    due: bool,
    /// The turn making it, with how many exchanges it covers.
    making: Option<(TurnId, usize)>,
    summary: Option<Summary>,
}

impl Carried {
    pub(super) fn summary(&self) -> Option<&Summary> {
        self.summary.as_ref()
    }
}

pub(super) fn is_carry_turn(turn: &TurnId) -> bool {
    turn.as_str().starts_with(CARRY_PREFIX)
}

pub(super) fn carry_turn_id(n: u32) -> TurnId {
    TurnId::parse(&format!("{CARRY_PREFIX}{n:03}")).unwrap_or_else(|| TurnId::numbered(n))
}

/// The main agent changed. Its CLI's session carries on, or the switch to another CLI says what
/// carries over and starts the summary.
pub(super) fn switched(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
) -> anyhow::Result<Vec<Effect>> {
    let main = ctx.settings().crew.main.clone();
    if let Some((session, model)) = conversation
        .carry
        .left
        .take_if(|(left, _)| left.worker.agent == main.agent)
    {
        return Ok(switched_back(ctx, conversation, session, model));
    }
    // With no session to carry on or sum up in, every turn starts from the conversation so far
    // already.
    if conversation.session.is_none() && conversation.carry.left.is_none() {
        conversation.carried_model = None;
        return Ok(Vec::new());
    }
    let same_cli = conversation
        .session
        .as_ref()
        .is_some_and(|session| session.worker.agent == main.agent);
    if same_cli {
        // The model you picked applies from the next turn.
        conversation.carried_model = None;
        if !conversation.history.is_empty() {
            let cli = ctx.names.cli(&main.agent);
            say_carried(
                ctx,
                format!("the whole conversation, in the same {cli} session"),
            );
        }
        return Ok(Vec::new());
    }
    if let Some(session) = conversation.session.take() {
        let model = conversation
            .carried_model
            .take()
            .or(session.worker.model.clone());
        conversation.carry.left = Some((session, model));
    }
    conversation.carried_model = None;
    conversation.store.forget_session()?;
    if conversation.history.is_empty() {
        return Ok(Vec::new());
    }
    let settings = &ctx.settings().switch;
    let line = carried_line(settings.carry, conversation.history.len(), settings.recent);
    say_carried(ctx, line);
    let covered = conversation
        .carry
        .summary
        .as_ref()
        .is_some_and(|summary| summary.covers >= conversation.history.len());
    conversation.carry.due =
        settings.carry == Carry::Summary && !covered && conversation.carry.making.is_none();
    start_if_due(ctx, conversation);
    Ok(Vec::new())
}

/// Back on the CLI the conversation left before you sent anything: it carries on in that
/// session, and a summary still being written is stopped.
fn switched_back(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    session: ConversationSession,
    model: Option<String>,
) -> Vec<Effect> {
    conversation.carry.due = false;
    if let Some((making, _)) = conversation.carry.making.take()
        && conversation
            .turn
            .as_ref()
            .is_some_and(|turn| turn.id == making)
    {
        stop_turn(ctx, conversation);
        settle_mode(ctx, &making);
    }
    if let Err(err) = conversation.store.save_session(&session) {
        ctx.notice(
            NoteLevel::Warn,
            format!("The next turn can't resume this session: {err:#}"),
        );
    }
    let cli = ctx.names.cli(&session.worker.agent);
    conversation.session = Some(session);
    conversation.carried_model = model;
    if !conversation.history.is_empty() {
        say_carried(
            ctx,
            format!("the whole conversation, in the same {cli} session"),
        );
    }
    if conversation.turn.is_none() {
        send_queued(conversation)
    } else {
        Vec::new()
    }
}

/// `Carried over: …`, hung under the `Set model to …` line the switch printed.
fn say_carried(ctx: &Ctx, what: String) {
    ctx.send(AppEvent::Continued {
        text: format!("Carried over: {what}"),
    });
}

/// `a summary of 14 exchanges, the last 6 in full, and the transcript to read`.
fn carried_line(carry: Carry, count: usize, recent: usize) -> String {
    let full = if count <= recent {
        match count {
            1 => "the 1 exchange in full".to_owned(),
            count => format!("all {count} in full"),
        }
    } else {
        format!("the last {recent} in full")
    };
    match carry {
        Carry::Summary => format!(
            "a summary of {}, {full}, and the transcript to read",
            exchanges(count)
        ),
        Carry::Recent if count <= recent => {
            format!("{} in full and the transcript to read", every(count))
        }
        Carry::Recent => format!(
            "the last {} in full and the transcript to read",
            exchanges(recent)
        ),
        Carry::All => format!("{} in full and the transcript to read", every(count)),
    }
}

fn every(count: usize) -> String {
    match count {
        1 => "the 1 exchange".to_owned(),
        count => format!("all {count} exchanges"),
    }
}

/// You sent something on the new CLI, so switching back no longer goes back.
pub(super) fn sent(conversation: &mut ConversationState) {
    conversation.carry.left = None;
}

/// `/compact` and the like replace the history, so an older summary no longer fits it.
pub(super) fn forget_summary(conversation: &mut ConversationState) {
    conversation.carry.summary = None;
}

/// The conversation so far, for a session that can't resume it.
pub(super) fn primer(ctx: &Ctx, conversation: &ConversationState) -> Option<InputPart> {
    let settings = &ctx.settings().switch;
    let recent = (settings.carry != Carry::All).then_some(settings.recent);
    excerpt::excerpt(
        &conversation.history,
        conversation.carry.summary(),
        recent,
        conversation.store.journal_path(),
    )
}

/// Starts the summary once no turn runs. The model being left writes it in its own session,
/// where it still holds what its tools found, unless it is out or its session is still in use;
/// then the new model writes it from the record.
pub(super) fn start_if_due(ctx: &mut Ctx, conversation: &mut ConversationState) {
    if !conversation.carry.due || conversation.turn.is_some() || conversation.history.is_empty() {
        return;
    }
    conversation.carry.due = false;
    // A turn that ran through the switch kept its session on the CLI being left.
    let main = ctx.settings().crew.main.agent.clone();
    if let Some(session) = conversation
        .session
        .take_if(|session| session.worker.agent != main)
    {
        let model = conversation
            .carried_model
            .take()
            .or(session.worker.model.clone());
        conversation.carry.left = Some((session, model));
    }
    let left = conversation.carry.left.clone().filter(|(session, _)| {
        // A session to branch from isn't this conversation's to write a summary in.
        !session.fork
            && conversation.stopping.is_empty()
            && !turn::agent_out(ctx, &session.worker)
            && ctx
                .services
                .workers
                .get(&session.worker)
                .is_ok_and(|handle| handle.capabilities().resume)
    });
    let step = match left {
        Some((session, model)) => TurnStep::Carry {
            writer: ModelRef::new(&session.worker.agent, model),
            resume: Some(session.cli_session),
            record: None,
        },
        None => by_new_model(ctx, &conversation.history),
    };
    begin(ctx, conversation, step);
}

fn by_new_model(ctx: &Ctx, history: &[Exchange]) -> TurnStep {
    TurnStep::Carry {
        writer: ctx.settings().crew.main.clone(),
        resume: None,
        record: Some(record(history)),
    }
}

fn begin(ctx: &mut Ctx, conversation: &mut ConversationState, step: TurnStep) {
    let covers = conversation.history.len();
    let by_left = matches!(&step, TurnStep::Carry { record: None, .. });
    match begin_carry(ctx, conversation, step) {
        Ok(id) => conversation.carry.making = Some((id, covers)),
        Err(err) if by_left => {
            cant_write(ctx, conversation, &format!("{err:#}"));
        }
        Err(err) => no_summary(ctx, conversation, &format!("{err:#}")),
    }
}

/// The model being left couldn't write it, so the new model writes it from the record.
fn cant_write(ctx: &mut Ctx, conversation: &mut ConversationState, why: &str) {
    let left = conversation
        .carry
        .left
        .as_ref()
        .map(|(session, _)| model_name(ctx, &session.worker))
        .unwrap_or_else(|| "The model you left".to_owned());
    let new = model_name(ctx, &ctx.settings().crew.main);
    ctx.notice(
        NoteLevel::Info,
        format!("{left} couldn't write the summary ({why}), so {new} writes it from the record."),
    );
    let step = by_new_model(ctx, &conversation.history);
    begin(ctx, conversation, step);
}

fn no_summary(ctx: &Ctx, conversation: &ConversationState, why: &str) {
    let settings = &ctx.settings().switch;
    let recent = conversation.history.len().min(settings.recent);
    let new = model_name(ctx, &ctx.settings().crew.main);
    ctx.notice(
        NoteLevel::Warn,
        format!(
            "No summary could be written ({why}), so {new} starts from the last {} in full and \
             the transcript.",
            exchanges(recent)
        ),
    );
}

/// The summary turn ended: its summary shows folded, or the next writer tries.
pub(super) fn summary_done(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    turn: Turn,
    outcome: &Result<StepOutcome, StepError>,
) -> Vec<Effect> {
    settle_mode(ctx, &turn.id);
    let covers = match conversation.carry.making.take() {
        Some((making, covers)) if making == turn.id => covers,
        other => {
            conversation.carry.making = other;
            return send_queued(conversation);
        }
    };
    let TurnStep::Carry { writer, record, .. } = &turn.step else {
        return send_queued(conversation);
    };
    let text = match read_answer(outcome) {
        Ok(Reading::Answer { markdown }) => Ok(markdown),
        Ok(_) => Ok(String::new()),
        Err(err) => Err(turn::describe(&err)),
    };
    match text {
        Ok(text) if !text.trim().is_empty() => {
            let summary = Summary {
                text: text.trim().to_owned(),
                writer: model_name(ctx, writer),
                covers,
            };
            show(ctx, &summary);
            conversation.carry.summary = Some(summary);
        }
        failed => {
            let why = failed
                .err()
                .unwrap_or_else(|| "it came back empty".to_owned());
            if record.is_none() {
                cant_write(ctx, conversation, &why);
            } else {
                no_summary(ctx, conversation, &why);
            }
        }
    }
    if conversation.turn.is_some() {
        return Vec::new();
    }
    send_queued(conversation)
}

/// A summary turn you stopped: the new model starts without it.
pub(super) fn stopped(ctx: &Ctx, conversation: &mut ConversationState, turn: &TurnId) {
    if conversation
        .carry
        .making
        .take_if(|(making, _)| making == turn)
        .is_some()
    {
        no_summary(ctx, conversation, "you stopped it");
    }
}

/// `● Summary for GPT-6.1-Sol · written by Opus 5.5 · 212 words`, its first lines under it and
/// the rest behind Ctrl+O.
fn show(ctx: &Ctx, summary: &Summary) {
    let new = model_name(ctx, &ctx.settings().crew.main);
    let words = summary.text.split_whitespace().count();
    let mut lines = summary
        .text
        .lines()
        .map(str::trim_end)
        .skip_while(|line| line.trim().is_empty());
    let shown: Vec<String> = lines
        .by_ref()
        .filter(|line| !line.trim().is_empty())
        .take(SHOWN_LINES)
        .map(plain)
        .collect();
    let more: Vec<String> = lines.map(plain).collect();
    ctx.send(AppEvent::Folded {
        title: format!(
            "Summary for {new} · written by {} · {words} words",
            summary.writer
        ),
        lines: shown,
        more,
    });
}

/// A summary line in plain words, since the folded lines are drawn as text, not Markdown: its
/// bold marks, code quotes and a heading's hashes go.
fn plain(line: &str) -> String {
    let line = line.trim_end();
    let heading = line.trim_start_matches('#');
    let line = if heading.len() < line.len() && heading.starts_with(' ') {
        heading.trim_start()
    } else {
        line
    };
    line.replace("**", "").replace('`', "")
}

/// The model by its name, a CLI's own default included, as the bottom line names it, or the
/// CLI when nothing says which model that is.
fn model_name(ctx: &Ctx, worker: &ModelRef) -> String {
    let (worker, _) = turn::conversation_agent(ctx.settings(), worker);
    let model = worker.model.clone().or_else(|| {
        crate::controller::names::cli_defaults(ctx, [worker.agent.as_str()]).remove(&worker.agent)
    });
    model
        .as_deref()
        .and_then(|model| ctx.names.model(&worker.agent, model))
        .unwrap_or_else(|| ctx.names.cli(&worker.agent))
}

/// The conversation as Argonauts Code recorded it, for the new model to sum up. A long one keeps
/// its tail.
fn record(history: &[Exchange]) -> String {
    let mut text = String::new();
    for exchange in history {
        text.push_str(&format!(
            "Asked:\n{}\n\nAnswered:\n{}\n\n",
            exchange.request().trim(),
            exchange.answer().trim()
        ));
    }
    let count = text.chars().count();
    if count <= RECORD_CHARS {
        return text;
    }
    let tail: String = text.chars().skip(count - RECORD_CHARS).collect();
    format!("(Only the record's tail fits here.)\n\n…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_says_what_carries_over() {
        assert_eq!(
            carried_line(Carry::Summary, 14, 6),
            "a summary of 14 exchanges, the last 6 in full, and the transcript to read"
        );
        assert_eq!(
            carried_line(Carry::Summary, 3, 6),
            "a summary of 3 exchanges, all 3 in full, and the transcript to read"
        );
        assert_eq!(
            carried_line(Carry::Recent, 14, 6),
            "the last 6 exchanges in full and the transcript to read"
        );
        assert_eq!(
            carried_line(Carry::Recent, 1, 6),
            "the 1 exchange in full and the transcript to read"
        );
        assert_eq!(
            carried_line(Carry::All, 14, 6),
            "all 14 exchanges in full and the transcript to read"
        );
    }

    #[test]
    fn a_long_record_keeps_its_tail() {
        let reply = argonauts_core::app::TurnReply::Answer {
            markdown: "x".repeat(RECORD_CHARS / 2 + 100),
        };
        let history = vec![
            Exchange::new("first question".to_owned(), &reply),
            Exchange::new("last question".to_owned(), &reply),
        ];
        let text = record(&history);
        assert!(text.starts_with("(Only the record's tail fits here.)"));
        assert!(text.contains("last question"));
        assert!(!text.contains("first question"));
    }
}
