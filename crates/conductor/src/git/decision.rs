//! How the merge decision reaches Argonauts Code. A [`MergeDecider`] answers each merge question.

use std::future::Future;
use std::path::PathBuf;

use argonauts_core::{MergeDecision, MergeTask, RunId, TaskId};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, PartialEq)]
pub struct MergeRequest {
    pub run_id: RunId,
    pub task_id: TaskId,
    pub title: String,
    pub task: MergeTask,
    /// Against the base commit, cut short when very large. The full diff is at `diff_path`.
    pub diff: String,
    pub diff_truncated: bool,
    pub diff_path: PathBuf,
}

pub trait MergeDecider: Send {
    /// An error leaves the branch in place.
    fn decide(
        &mut self,
        request: MergeRequest,
    ) -> impl Future<Output = anyhow::Result<MergeDecision>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedDecider(pub MergeDecision);

impl MergeDecider for FixedDecider {
    async fn decide(&mut self, _request: MergeRequest) -> anyhow::Result<MergeDecision> {
        Ok(self.0.clone())
    }
}

#[derive(Debug)]
pub struct PendingDecision {
    pub request: MergeRequest,
    reply: oneshot::Sender<MergeDecision>,
}

impl PendingDecision {
    pub fn answer(self, decision: MergeDecision) {
        // Argonauts Code may have stopped waiting, for example after a stop request.
        let _ = self.reply.send(decision);
    }
}

#[derive(Debug, Clone)]
pub struct ChannelDecider {
    requests: mpsc::UnboundedSender<PendingDecision>,
}

pub fn decision_channel() -> (ChannelDecider, mpsc::UnboundedReceiver<PendingDecision>) {
    let (requests, receiver) = mpsc::unbounded_channel();
    (ChannelDecider { requests }, receiver)
}

impl MergeDecider for ChannelDecider {
    async fn decide(&mut self, request: MergeRequest) -> anyhow::Result<MergeDecision> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(PendingDecision { request, reply })
            .map_err(|_| anyhow::anyhow!("nothing is listening for the merge decision"))?;
        answer
            .await
            .map_err(|_| anyhow::anyhow!("the merge approval was closed without an answer"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::DiffStat;

    fn request() -> MergeRequest {
        MergeRequest {
            run_id: RunId::new("run-1").unwrap(),
            task_id: TaskId::numbered(1),
            title: "Add hello".into(),
            task: MergeTask {
                branch: "argonauts/run-1/task-001".into(),
                target_branch: Some("main".into()),
                stat: DiffStat::default(),
                checks: vec![],
                report: None,
                spend: None,
                stopped_by: None,
            },
            diff: String::new(),
            diff_truncated: false,
            diff_path: PathBuf::from("/tmp/diff.patch"),
        }
    }

    #[tokio::test]
    async fn fixed_decider_repeats_its_answer() {
        let mut decider = FixedDecider(MergeDecision::OpenPr);
        assert_eq!(
            decider.decide(request()).await.unwrap(),
            MergeDecision::OpenPr
        );
        assert_eq!(
            decider.decide(request()).await.unwrap(),
            MergeDecision::OpenPr
        );
    }

    #[tokio::test]
    async fn channel_decider_round_trips() {
        let (mut decider, mut pending) = decision_channel();
        let ui = tokio::spawn(async move {
            let task = pending.recv().await.unwrap();
            assert_eq!(task.request.title, "Add hello");
            task.answer(MergeDecision::Approve);
        });
        assert_eq!(
            decider.decide(request()).await.unwrap(),
            MergeDecision::Approve
        );
        ui.await.unwrap();
    }

    #[tokio::test]
    async fn channel_decider_errors_without_a_listener() {
        let (mut decider, pending) = decision_channel();
        drop(pending);
        assert!(decider.decide(request()).await.is_err());

        let (mut decider, mut pending) = decision_channel();
        tokio::spawn(async move {
            // Drop the question without answering.
            let _ = pending.recv().await;
        });
        let err = decider.decide(request()).await.unwrap_err();
        assert!(err.to_string().contains("without an answer"));
    }
}
