//! The conversations as rows of the agent panel.

use std::collections::BTreeSet;
use std::path::Path;

use argonauts_core::app::agent_panel::{CrewDetails, CrewRow, CrewState, CrewTarget};
use argonauts_core::app::views::ContextUse;
use argonauts_core::app::{ConversationSummary, TurnReply};

use super::past::{self, PastConversation};
use super::{ConversationState, settled};
use crate::conversation::store::ConversationStore;

/// The newest conversations the panel lists; the rest stay in `/resume`.
const MOST: usize = 20;

/// The newest conversations, `conversation` working while its turn runs.
pub(crate) fn rows(
    conversation_dir: &Path,
    conversation: &ConversationState,
    repo: &str,
) -> Vec<CrewRow> {
    past::list(conversation_dir, &[])
        .into_iter()
        .take(MOST)
        .filter_map(|summary| {
            let past = past::load(conversation_dir, &summary.conversation)?;
            let current = summary.conversation == conversation.id;
            let busy = current && !settled(conversation);
            let context = conversation
                .context
                .as_ref()
                .filter(|_| current)
                .map(|row| ContextUse {
                    used: row.used,
                    window: row.window,
                    cap: row.cap,
                });
            Some(row(&summary, &past, busy, context, repo))
        })
        .collect()
}

fn row(
    summary: &ConversationSummary,
    past: &PastConversation,
    busy: bool,
    context: Option<ContextUse>,
    repo: &str,
) -> CrewRow {
    let reply = past
        .turns
        .iter()
        .rev()
        .find_map(|turn| turn.reply.as_ref())
        .map(markdown);
    let latest = match &reply {
        _ if busy => "answering".to_owned(),
        Some(reply) => first_line(reply),
        None => String::new(),
    };
    CrewRow {
        target: CrewTarget::Conversation {
            conversation: summary.conversation.clone(),
        },
        state: if busy {
            CrewState::Working
        } else {
            CrewState::Ready
        },
        title: if summary.title.trim().is_empty() {
            "Untitled conversation".to_owned()
        } else {
            summary.title.clone()
        },
        label: None,
        tries: 0,
        repo: repo.to_owned(),
        run: None,
        worker: summary.worker.clone(),
        latest,
        updated: summary.updated,
        details: CrewDetails {
            last_message: reply,
            context,
            prompt: past.turns.first().map(|turn| turn.request.clone()),
            ..CrewDetails::default()
        },
    }
}

fn markdown(reply: &TurnReply) -> String {
    match reply {
        TurnReply::Answer { markdown }
        | TurnReply::Change { markdown, .. }
        | TurnReply::Unclear { markdown, .. } => markdown.clone(),
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(|line| line.trim().trim_start_matches('#').trim())
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// The CLI sessions the conversations ran in, which the panel lists as conversations only.
pub(crate) fn cli_sessions(conversation_dir: &Path) -> BTreeSet<String> {
    past::list(conversation_dir, &[])
        .iter()
        .filter_map(|summary| {
            ConversationStore::open(conversation_dir, &summary.conversation).session()
        })
        // A session a conversation only branches from is still the CLI's own.
        .filter(|session| !session.fork)
        .map(|session| session.cli_session)
        .collect()
}

#[cfg(test)]
mod tests {
    use argonauts_core::Spend;
    use argonauts_core::app::ConversationEvent;
    use argonauts_core::base::names::{ConversationId, TurnId};

    use super::*;

    fn write(conversation_dir: &Path, n: u32, request: &str, answer: Option<&str>) {
        let store = ConversationStore::open(conversation_dir, &ConversationId::numbered(n));
        store
            .append(&ConversationEvent::TurnStarted {
                turn: TurnId::numbered(1),
                text: request.to_owned(),
                worker: "codex/gpt-5.6".parse().unwrap(),
                read_only: true,
                commit: "abc".to_owned(),
                uncommitted: Vec::new(),
                settings_sha256: None,
                continues: false,
                opens_a_talk: false,
            })
            .unwrap();
        if let Some(answer) = answer {
            store
                .append(&ConversationEvent::TurnFinished {
                    turn: TurnId::numbered(1),
                    reply: TurnReply::Answer {
                        markdown: answer.to_owned(),
                    },
                    spend: Spend::default(),
                    elapsed_ms: 10,
                })
                .unwrap();
        }
    }

    #[test]
    fn a_conversation_is_ready_with_its_first_prompt_and_last_answer() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            1,
            "How does the export pick a format?",
            Some("## Short answer\n\nIt reads --format first."),
        );
        let past = past::load(dir.path(), &ConversationId::numbered(1)).unwrap();
        let row = row(&past.summary, &past, false, None, "~/app");
        assert_eq!(row.state, CrewState::Ready);
        assert_eq!(row.title, "How does the export pick a format?");
        assert_eq!(row.latest, "Short answer");
        assert_eq!(row.worker, Some("codex/gpt-5.6".parse().unwrap()));
        assert_eq!(
            row.details.prompt.as_deref(),
            Some("How does the export pick a format?")
        );
        assert!(
            row.details
                .last_message
                .as_deref()
                .is_some_and(|text| text.ends_with("It reads --format first."))
        );
        assert_eq!(
            row.target,
            CrewTarget::Conversation {
                conversation: ConversationId::numbered(1)
            }
        );

        let busy = super::row(&past.summary, &past, true, None, "~/app");
        assert_eq!(
            (busy.state, busy.latest.as_str()),
            (CrewState::Working, "answering")
        );
    }
}
