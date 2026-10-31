//! Copies of the conversation: `/branch` carries on in a copy, `/fork` leaves one behind and,
//! with a prompt, starts a Captain run on it, `/export` writes it out as plain text, and `/copy`
//! hands an answer to an outside agent with no clipboard of its own.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::NoteLevel;
use argonauts_core::app::AppEvent;
use argonauts_core::base::clock;
use argonauts_core::runs::events::run::RunMode;
use time::OffsetDateTime;

use super::excerpt::{Exchange, cut, reply_text};
use super::past::{self, PastConversation};
use super::{ConversationState, handover, planned, run_request, settled};
use crate::controller::folders::workspace::resolve_path;
use crate::controller::{Ctx, Effect};

/// How many of the last exchanges a forked run is told, and how long each part may be.
const FORK_EXCHANGES: usize = 4;
const FORK_CHARS: usize = 600;

/// `/branch`: a copy of the conversation that you carry on, while the original stays as it
/// was for `/resume`. The copy starts a fresh CLI session primed with the conversation, with
/// the same agent in the same folder when the original carries on an agent's session.
pub(crate) fn branch(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    name: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    if conversation.turns == 0 {
        ctx.notice(NoteLevel::Info, "Nothing to branch yet.");
        return Ok(Vec::new());
    }
    if !settled(conversation) {
        ctx.notice(
            NoteLevel::Info,
            "A turn is still running. Esc stops it, then /branch starts the copy.",
        );
        return Ok(Vec::new());
    }
    let original = conversation.id.clone();
    let copy = copy_of(ctx, conversation, name, "branch")?;
    ctx.notice(
        NoteLevel::Info,
        format!(
            "Branched into {}, \"{}\".{} /resume {original} goes back to the original.",
            copy.id,
            copy.title,
            works_in(ctx, &copy)
        ),
    );
    conversation.succeed(ctx, copy);
    Ok(vec![Effect::Status])
}

/// `/fork`: leaves a copy of the conversation for `/resume` and stays on this one. With a
/// prompt, a Captain run takes it on, told the conversation so far.
pub(crate) fn fork(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    prompt: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    let prompt = prompt
        .map(|prompt| prompt.trim().to_owned())
        .filter(|prompt| !prompt.is_empty());
    if conversation.turns == 0 && prompt.is_none() {
        ctx.notice(
            NoteLevel::Info,
            "Nothing to fork yet. /fork <prompt> starts a Captain run on the prompt.",
        );
        return Ok(Vec::new());
    }
    if conversation.turns > 0 {
        let copy = copy_of(ctx, conversation, None, "fork")?;
        ctx.notice(
            NoteLevel::Info,
            format!(
                "Forked this conversation to {}.{} /resume {} picks the copy up.",
                copy.id,
                works_in(ctx, &copy),
                copy.id
            ),
        );
    }
    let Some(prompt) = prompt else {
        return Ok(Vec::new());
    };
    let mut request = run_request(
        ctx,
        forked_request(&prompt, &conversation.history),
        RunMode::Captain,
        None,
        None,
    );
    if conversation.prefs.plan_first {
        request = planned(request);
    }
    Ok(vec![handover::start(request)])
}

/// The prompt, then the end of the conversation it came from.
fn forked_request(prompt: &str, history: &[Exchange]) -> String {
    if history.is_empty() {
        return prompt.to_owned();
    }
    let skipped = history.len().saturating_sub(FORK_EXCHANGES);
    let mut text =
        format!("{prompt}\n\nFrom the conversation this was forked from, oldest first:\n");
    for exchange in &history[skipped..] {
        text.push_str(&format!(
            "\nAsked: {}\nAnswered: {}\n",
            cut(exchange.request(), FORK_CHARS),
            cut(exchange.answer(), FORK_CHARS)
        ));
    }
    text
}

/// Where a copy of a conversation that carries on an agent's session works, said after its
/// name, with the agent it goes to. Nothing for one that works in your checkout.
fn works_in(ctx: &Ctx, copy: &ConversationState) -> String {
    let Some(place) = &copy.place else {
        return String::new();
    };
    let agent = ctx.names.worker(&place.worker);
    if place.folder == ctx.env.repo_root {
        return format!(" It works with {agent} in your checkout, on a session of its own.");
    }
    let folder = ctx.env.display(&place.folder);
    let on_branch = place
        .branch
        .as_deref()
        .map_or_else(String::new, |branch| format!(" on {branch}"));
    format!(
        "It works with {agent} in the run's worktree in {folder}{on_branch}, not your checkout, on \
         a session of its own."
    )
}

/// A new conversation holding this one's journal and history, named `name` or after this one.
/// One that carries on an agent's session keeps working where it does, with that agent, but
/// on a fresh session of the agent's CLI, since two conversations never share one.
fn copy_of(
    ctx: &Ctx,
    conversation: &ConversationState,
    name: Option<String>,
    kind: &str,
) -> anyhow::Result<ConversationState> {
    let mut copy = ConversationState::new(ctx)?;
    conversation
        .store
        .copy_to(&copy.store)
        .with_context(|| format!("copying {} to {}", conversation.id, copy.id))?;
    let base = if conversation.title.is_empty() {
        "Conversation"
    } else {
        &conversation.title
    };
    copy.title = name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("{base} ({kind})"));
    copy.store.save_title(&copy.title)?;
    copy.turns = conversation.turns;
    copy.history = conversation.history.clone();
    copy.place = conversation.place.clone();
    Ok(copy)
}

/// `/export [file]`: the conversation as plain text, in `file` or a dated file in the repo.
/// Side questions were never part of it, so they stay out.
pub(crate) fn export(
    ctx: &mut Ctx,
    conversation: &ConversationState,
    file: Option<PathBuf>,
) -> anyhow::Result<Vec<Effect>> {
    let past = past::load(&ctx.env.conversation_dir(), &conversation.id)
        .filter(|past| !past.turns.is_empty());
    let Some(past) = past else {
        ctx.notice(NoteLevel::Info, "Nothing to export yet.");
        return Ok(Vec::new());
    };
    let now = clock::now_utc();
    let title = if conversation.title.is_empty() {
        past.summary.title.clone()
    } else {
        conversation.title.clone()
    };
    let path = match file {
        Some(file) => {
            let path = resolve_path(ctx, &file);
            if path.is_dir() {
                path.join(file_name(&title, now))
            } else {
                path
            }
        }
        None => free_path(&ctx.env.repo_root, &file_name(&title, now)),
    };
    std::fs::write(&path, plain_text(&past, &title, now))
        .with_context(|| format!("writing {}", ctx.env.display(&path)))?;
    ctx.notice(
        NoteLevel::Info,
        format!("Saved the conversation to {}.", ctx.env.display(&path)),
    );
    Ok(Vec::new())
}

fn plain_text(past: &PastConversation, title: &str, now: OffsetDateTime) -> String {
    let mut text = format!(
        "{title}\nArgonauts Code conversation {}, exported {} {:02}:{:02} UTC\n",
        past.summary.conversation,
        now.date(),
        now.hour(),
        now.minute()
    );
    for turn in &past.turns {
        text.push('\n');
        for line in turn.request.lines() {
            text.push_str(&format!("> {line}\n"));
        }
        text.push('\n');
        match &turn.reply {
            Some(reply) => {
                text.push_str(&format!("{} answered:\n\n", turn.worker));
                text.push_str(reply_text(reply).trim_end());
                text.push('\n');
            }
            None => text.push_str("(no answer)\n"),
        }
    }
    text
}

/// Such as `2026-10-01-rate-limiting-after-auth.txt`.
fn file_name(title: &str, now: OffsetDateTime) -> String {
    let mut slug = String::new();
    for word in title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        if slug.len() + word.len() > 40 {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word.to_lowercase());
    }
    if slug.is_empty() {
        slug.push_str("conversation");
    }
    format!("{}-{slug}.txt", now.date())
}

/// `name` in `dir`, or `name-2`, `name-3`, and so on when it's taken.
fn free_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let stem = name.strip_suffix(".txt").unwrap_or(name);
    (2..)
        .map(|n| dir.join(format!("{stem}-{n}.txt")))
        .find(|path| !path.exists())
        .unwrap_or(first)
}

/// `/copy` from an outside agent with no screen: the answer comes back as a report to copy from.
pub(crate) fn copy(
    ctx: &mut Ctx,
    conversation: &ConversationState,
    nth: Option<u32>,
) -> anyhow::Result<Vec<Effect>> {
    let nth = usize::try_from(nth.unwrap_or(1)).unwrap_or(usize::MAX);
    let Some(exchange) = conversation.history.iter().rev().nth(nth.saturating_sub(1)) else {
        let text = match conversation.history.len() {
            0 => "No answer to copy yet.".to_owned(),
            1 => "There's only 1 answer so far.".to_owned(),
            n => format!("There are only {n} answers so far."),
        };
        ctx.notice(NoteLevel::Info, text);
        return Ok(Vec::new());
    };
    ctx.send(AppEvent::Report {
        title: "Answer to copy".to_owned(),
        lines: exchange.answer().lines().map(str::to_owned).collect(),
    });
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::app::TurnReply;

    fn exchange(request: &str, answer: &str) -> Exchange {
        Exchange::new(
            request.to_owned(),
            &TurnReply::Answer {
                markdown: answer.to_owned(),
            },
        )
    }

    #[test]
    fn a_file_name_is_dated_and_made_of_the_titles_words() {
        let now = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
        assert_eq!(
            file_name("Rate limiting, after auth!", now),
            format!("{}-rate-limiting-after-auth.txt", now.date())
        );
        assert_eq!(
            file_name("", now),
            format!("{}-conversation.txt", now.date())
        );
    }

    #[test]
    fn a_taken_name_gets_a_number() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(free_path(dir.path(), "a.txt"), dir.path().join("a.txt"));
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        std::fs::write(dir.path().join("a-2.txt"), "").unwrap();
        assert_eq!(free_path(dir.path(), "a.txt"), dir.path().join("a-3.txt"));
    }

    #[test]
    fn a_forked_run_is_told_the_end_of_the_conversation() {
        let history: Vec<Exchange> = (1..=6)
            .map(|n| exchange(&format!("question {n}"), &format!("answer {n}")))
            .collect();
        let request = forked_request("write the migration", &history);
        assert!(request.starts_with("write the migration\n\nFrom the conversation"));
        assert!(!request.contains("question 2\n"), "{request}");
        assert!(
            request.contains("Asked: question 3\nAnswered: answer 3\n"),
            "{request}"
        );
        assert_eq!(forked_request("just this", &[]), "just this");
    }
}
