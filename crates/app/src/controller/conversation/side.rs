//! Turns about the conversation itself, whatever CLI the main agent is: `/btw` asks off the
//! record, `/rename` names it, `/compact` shrinks it to a summary, `/recap` sums it up, and
//! `/review` and `/security-review` have the changes judged by an agent from another provider
//! where the crew has one.

use std::sync::Arc;

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, AppMode, TurnReply};
use argonauts_core::names::{ModelRef, RoleName, TurnId};
use argonauts_core::session_runner::{RunnerError, SessionRunner};

use super::excerpt::{Exchange, reply_text};
use super::lifecycle::{begin, begin_side};
use super::{ConversationState, Queued};
use crate::controller::recap::{recap_from_summary, title_from_request};
use crate::controller::turn::{self, TurnStep};
use crate::controller::workspace::review_changes;
use crate::controller::{Ctx, Effect};

/// Side turns number apart from the conversation's, so the journal never sees them.
const SIDE_PREFIX: &str = "t-side-";
const COMPACT: &str = "/compact";
/// What a compacted conversation's summary answers, for a session primed with it.
const COMPACTED: &str = "Summarize the conversation so far.";
/// The longest name `/rename` keeps.
const TITLE_CHARS: usize = 60;
/// How much of an answer `/recap` quotes.
const RECAP_CHARS: usize = 140;

pub(super) fn side_turn_id(n: u32) -> TurnId {
    TurnId::parse(&format!("{SIDE_PREFIX}{n:03}")).unwrap_or_else(|| TurnId::numbered(n))
}

pub(super) fn is_side_turn(turn: &TurnId) -> bool {
    turn.as_str().starts_with(SIDE_PREFIX)
}

/// Plain text never starts with a slash, so only `/compact` records a turn like this.
pub(super) fn is_compact_request(request: &str) -> bool {
    request == COMPACT || request.starts_with("/compact ")
}

/// `/btw`: answered from a session of its own that starts from the conversation so far. The
/// answer shows, but the conversation, its journal, and its CLI session never hold it.
pub(crate) fn btw(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    question: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    let question = question.unwrap_or_default().trim().to_owned();
    if question.is_empty() {
        ctx.notice(
            NoteLevel::Info,
            "Put the side question after /btw. Its answer stays out of the conversation.",
        );
        return Ok(Vec::new());
    }
    if conversation.turn.is_some() {
        conversation.queue(Queued::Side(question));
        return Ok(Vec::new());
    }
    begin_side(ctx, conversation, question, TurnStep::Side)?;
    Ok(Vec::new())
}

/// `/rename <name>` names the conversation; with no name the main agent suggests one, off the
/// record.
pub(crate) fn rename(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    name: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    if let Some(name) = name
        .map(|name| clean_title(&name))
        .filter(|n| !n.is_empty())
    {
        return Ok(set_title(ctx, conversation, name));
    }
    if conversation.history.is_empty() {
        ctx.notice(
            NoteLevel::Info,
            "Nothing to name yet. /rename <name> names it.",
        );
        return Ok(Vec::new());
    }
    if conversation.turn.is_some() {
        ctx.notice(
            NoteLevel::Info,
            "A turn is still running. /rename <name> names it now, or /rename once it ends.",
        );
        return Ok(Vec::new());
    }
    begin_side(
        ctx,
        conversation,
        "Name this conversation".to_owned(),
        TurnStep::Title,
    )?;
    Ok(Vec::new())
}

fn set_title(ctx: &Ctx, conversation: &mut ConversationState, title: String) -> Vec<Effect> {
    if let Err(err) = conversation.store.save_title(&title) {
        ctx.notice(
            NoteLevel::Warn,
            format!("The name lasts only until you close the app: {err:#}"),
        );
    }
    ctx.notice(
        NoteLevel::Info,
        format!("Renamed this conversation to \"{title}\"."),
    );
    conversation.title = title;
    vec![Effect::Status]
}

/// The first line, without quotes, a heading mark, or a full stop.
fn clean_title(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let line = line
        .trim_start_matches('#')
        .trim()
        .trim_matches(|c| matches!(c, '"' | '\'' | '`' | '*' | '“' | '”'))
        .trim_end_matches('.')
        .trim();
    cut(line, TITLE_CHARS)
}

/// `/compact`: the main agent sums up the conversation, and the next turn starts a fresh
/// session from that summary.
pub(crate) fn compact(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    instructions: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    if conversation.history.is_empty() {
        ctx.notice(NoteLevel::Info, "Nothing to compact yet.");
        return Ok(Vec::new());
    }
    if conversation.turn.is_some() {
        ctx.notice(
            NoteLevel::Info,
            "A turn is still running. Esc stops it, then /compact sums up the conversation.",
        );
        return Ok(Vec::new());
    }
    let text = match &instructions {
        Some(instructions) => format!("{COMPACT} {instructions}"),
        None => COMPACT.to_owned(),
    };
    begin(ctx, conversation, text, |_| TurnStep::Compact {
        instructions,
    })?;
    Ok(Vec::new())
}

/// What a finished turn of these steps changes in the conversation.
pub(super) fn after(
    ctx: &Ctx,
    conversation: &mut ConversationState,
    step: &TurnStep,
    reply: &TurnReply,
) -> Vec<Effect> {
    match step {
        TurnStep::Title => {
            let title = clean_title(reply_text(reply));
            if title.is_empty() {
                ctx.notice(
                    NoteLevel::Info,
                    "No name came back. /rename <name> names the conversation.",
                );
                return Vec::new();
            }
            set_title(ctx, conversation, title)
        }
        TurnStep::Compact { .. } => {
            conversation.history = vec![Exchange::new(COMPACTED.to_owned(), reply)];
            super::carry::forget_summary(conversation);
            conversation.session = None;
            if let Err(err) = conversation.store.forget_session() {
                ctx.notice(NoteLevel::Warn, format!("{err:#}"));
            }
            if let Some(row) = conversation.context.as_mut() {
                row.used = 0;
            }
            ctx.notice(
                NoteLevel::Info,
                "Compacted. The next turn starts a fresh session from this summary.",
            );
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// `/recap`: the run on screen, or else the conversation, in a line, at no token cost.
pub(crate) async fn recap(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
) -> anyhow::Result<Vec<Effect>> {
    if let AppMode::Working { run } | AppMode::Pausing { run } | AppMode::Waiting { run, .. } =
        &ctx.mode
    {
        let run = run.clone();
        match ctx.services.conductor.status(&run).await {
            Ok(summary) => {
                ctx.send(AppEvent::Recap {
                    text: recap_from_summary(&summary, None, &[]),
                    model: None,
                });
                return Ok(Vec::new());
            }
            Err(err) => ctx.notice(
                NoteLevel::Warn,
                format!("Couldn't read {run}'s status, so this recaps the conversation: {err}"),
            ),
        }
    }
    let Some(last) = conversation.history.last() else {
        ctx.notice(NoteLevel::Info, "Nothing to recap yet.");
        return Ok(Vec::new());
    };
    ctx.send(AppEvent::Recap {
        text: conversation_recap(&conversation.title, conversation.history.len(), last),
        model: None,
    });
    Ok(Vec::new())
}

fn conversation_recap(title: &str, exchanges: usize, last: &Exchange) -> String {
    let asked = title_from_request(last.request());
    let title = if title.is_empty() { &asked } else { title };
    let count = if exchanges == 1 {
        "1 exchange".to_owned()
    } else {
        format!("{exchanges} exchanges")
    };
    let answer = first_sentence(last.answer());
    format!("{title}, {count}. Last you asked \"{asked}\": {answer}")
}

/// The answer's first sentence as plain text, without Markdown's marks.
fn first_sentence(markdown: &str) -> String {
    let line = markdown
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.trim_start_matches(['-', '*', '>', ' ']))
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let sentence = match line.find(". ") {
        Some(end) => &line[..=end],
        None => line,
    };
    cut(&sentence.replace(['*', '`'], ""), RECAP_CHARS)
}

fn cut(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(chars - 1).collect();
    short.push('…');
    short
}

/// `/review` and `/security-review`: a read-only turn by an agent from another provider than
/// the main agent's where the crew has one ready, else by the main agent. Its answer joins the
/// conversation, so a follow-up can act on it.
pub(crate) fn review(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    security: bool,
    target: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    let command = if security {
        "/security-review"
    } else {
        "/review"
    };
    if conversation.turn.is_some() {
        ctx.notice(
            NoteLevel::Info,
            format!("A turn is still running. Esc stops it, or {command} again once it ends."),
        );
        return Ok(Vec::new());
    }
    // The changes where the conversation works, which is the run's worktree for an agent's
    // session carried on.
    let folder = conversation.folder(ctx).to_path_buf();
    let Some(changes) = review_changes(&folder, target.as_deref())? else {
        ctx.notice(
            NoteLevel::Info,
            "Nothing to review: the working tree has no changes. Name a branch, a commit range, \
             or a pull request.",
        );
        return Ok(Vec::new());
    };
    let other_provider = other_provider(ctx, &[]).is_some();
    if !other_provider {
        ctx.notice(
            NoteLevel::Info,
            format!(
                "No agent from another provider is in the crew and ready, so {} reviews.",
                ctx.settings().crew.main
            ),
        );
    }
    let text = match &target {
        Some(target) => format!("{command} {target}"),
        None => command.to_owned(),
    };
    begin(ctx, conversation, text, |_| TurnStep::Review {
        security,
        target: changes.target,
        changes: changes.text,
        other_provider,
    })?;
    Ok(Vec::new())
}

/// The reviewer a review turn runs on, given the workers that already failed it.
pub(super) fn reviewer(
    ctx: &Ctx,
    tried: &[ModelRef],
) -> Result<(ModelRef, Arc<dyn SessionRunner>), RunnerError> {
    match other_provider(ctx, tried) {
        Some(found) => Ok(found),
        None => turn::pick(ctx, tried),
    }
}

/// The reviewer role's workers first, then every other one, as a run picks its reviewers: the
/// first of another model provider than the main agent's that is on and in service.
fn other_provider(ctx: &Ctx, tried: &[ModelRef]) -> Option<(ModelRef, Arc<dyn SessionRunner>)> {
    let settings = ctx.settings();
    let registry = &ctx.services.registry;
    let main = registry.provider(&settings.crew.main);
    let mut pool = argonauts_crew::roles::candidates(settings, RoleName::Reviewer);
    pool.extend(ctx.services.workers.available());
    pool.into_iter().find_map(|worker| {
        let fit = !tried.contains(&worker)
            && registry.provider(&worker) != main
            && !turn::agent_out(ctx, &worker);
        let handle = ctx.services.workers.get(&worker).ok().filter(|_| fit)?;
        Some((worker, handle))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_suggested_name_loses_its_quotes_and_full_stop() {
        assert_eq!(
            clean_title("\"Rate limiting after auth.\"\n"),
            "Rate limiting after auth"
        );
        assert_eq!(clean_title("\n# Login loop\nmore"), "Login loop");
        assert_eq!(
            clean_title(&"word ".repeat(40)).chars().count(),
            TITLE_CHARS
        );
    }

    #[test]
    fn side_turns_number_apart_and_compact_requests_are_known() {
        let side = side_turn_id(2);
        assert_eq!(side.as_str(), "t-side-002");
        assert!(is_side_turn(&side));
        assert!(!is_side_turn(&TurnId::numbered(2)));
        assert!(is_compact_request("/compact"));
        assert!(is_compact_request("/compact keep the plan"));
        assert!(!is_compact_request("/compactly"));
    }

    #[test]
    fn a_recap_names_the_conversation_and_quotes_the_last_answer() {
        let last = Exchange::new(
            "why does login loop?".to_owned(),
            &TurnReply::Answer {
                markdown: "## Cause\n**The cookie** is set twice. Then it redirects.".to_owned(),
            },
        );
        assert_eq!(
            conversation_recap("Login loop", 3, &last),
            "Login loop, 3 exchanges. Last you asked \"why does login loop?\": The cookie is set \
             twice."
        );
        assert!(conversation_recap("", 1, &last).starts_with("why does login loop?, 1 exchange."));
    }
}
