//! What the conductor sends the interface, and what the interface sends back.

use argonauts_core::{CardId, MergeCard, MergeDecision, RunEvent};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
pub enum UiEvent {
    /// Something that happened in the run, exactly as written to the event log.
    Run(Box<RunEvent>),
    MergeCardShown(Box<MergePrompt>),
}

impl From<RunEvent> for UiEvent {
    fn from(event: RunEvent) -> Self {
        Self::Run(Box::new(event))
    }
}

#[derive(Debug)]
pub struct MergePrompt {
    pub card_id: CardId,
    pub card: MergeCard,
    pub diff: Option<String>,
    /// Receives exactly one decision. If it is dropped unanswered, treat that as `Leave`.
    pub reply: oneshot::Sender<MergeDecision>,
}

impl MergePrompt {
    pub fn new(
        card_id: CardId,
        card: MergeCard,
        diff: Option<String>,
    ) -> (Self, oneshot::Receiver<MergeDecision>) {
        let (reply, answer) = oneshot::channel();
        let prompt = Self {
            card_id,
            card,
            diff,
            reply,
        };
        (prompt, answer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    /// Stop the run, keeping the agent's work on its branch.
    StopRun,
}

/// Returns `Leave` when the interface is gone or closes the card unanswered, so a closed
/// screen never merges anything.
pub async fn ask_merge(
    events: &mpsc::UnboundedSender<UiEvent>,
    card_id: CardId,
    card: MergeCard,
    diff: Option<String>,
) -> MergeDecision {
    let (prompt, answer) = MergePrompt::new(card_id, card, diff);
    if events
        .send(UiEvent::MergeCardShown(Box::new(prompt)))
        .is_err()
    {
        return MergeDecision::Leave;
    }
    answer.await.unwrap_or(MergeDecision::Leave)
}

#[cfg(test)]
mod tests {
    use argonauts_core::DiffStat;

    use super::*;

    fn card() -> MergeCard {
        MergeCard {
            branch: "argonauts/run-1/card-001".into(),
            target_branch: Some("main".into()),
            stat: DiffStat::default(),
            gates: Vec::new(),
            report: None,
            budget: None,
            stopped_by: None,
        }
    }

    #[tokio::test]
    async fn ask_merge_returns_the_users_answer() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let asking = tokio::spawn(async move {
            ask_merge(&tx, CardId::numbered(1), card(), Some("diff".into())).await
        });
        let Some(UiEvent::MergeCardShown(prompt)) = rx.recv().await else {
            panic!("expected a merge prompt");
        };
        assert_eq!(prompt.card_id.as_str(), "card-001");
        assert_eq!(prompt.diff.as_deref(), Some("diff"));
        prompt.reply.send(MergeDecision::Approve).unwrap();
        assert_eq!(asking.await.unwrap(), MergeDecision::Approve);
    }

    #[tokio::test]
    async fn ask_merge_leaves_when_the_ui_is_gone() {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        let decision = ask_merge(&tx, CardId::numbered(1), card(), None).await;
        assert_eq!(decision, MergeDecision::Leave);
    }

    #[tokio::test]
    async fn ask_merge_leaves_when_the_prompt_is_dropped() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let asking =
            tokio::spawn(async move { ask_merge(&tx, CardId::numbered(1), card(), None).await });
        drop(rx.recv().await);
        assert_eq!(asking.await.unwrap(), MergeDecision::Leave);
    }
}
