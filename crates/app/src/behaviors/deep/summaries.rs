//! Writing the summaries an update needs: only actions without one, each distinct text once,
//! ten to a request, every request's summaries kept the moment they're back.

use std::collections::BTreeMap;
use std::io;
use std::time::{Duration, Instant};

use argonauts_core::safety::sanitize::sanitize_for_terminal;
use argonauts_tui::base::text::{At, cut};
use tokio_util::sync::CancellationToken;

use super::action::{ActionKey, DeepAction};
use super::prompt::{self, PER_REQUEST, PROMPT_VERSION};
use super::store::{Store, SummaryLine};
use super::summarize::Summarizer;

/// How far an update is, in actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Progress {
    pub done: usize,
    pub total: usize,
    pub elapsed: Duration,
    /// Distinct texts sent so far and still to send, which is what takes the time.
    pub sent: usize,
    pub to_send: usize,
}

impl Progress {
    /// The time left at the pace so far, once a request has come back.
    pub fn left(&self) -> Option<Duration> {
        if self.sent == 0 {
            return None;
        }
        let each = self.elapsed.as_secs_f64() / self.sent as f64;
        Some(Duration::from_secs_f64(
            each * self.to_send.saturating_sub(self.sent) as f64,
        ))
    }
}

/// What an update of the summaries did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Summarized {
    /// Actions that already had a summary.
    pub already: usize,
    /// Actions given the summary of an identical one, with no request.
    pub reused: usize,
    /// Actions summarized now.
    pub written: usize,
    pub requests: usize,
    /// Actions still without one: the update stopped, or the summarizer skipped them.
    pub missing: usize,
    pub stopped: bool,
    /// It sent as many texts as it was let, and more wait.
    pub paused: bool,
    /// Why the update stopped before its end, in a sentence naming the summarizer.
    pub failed: Option<String>,
}

/// The actions of `actions` that need a summary from `summarizer`, by the text it would read,
/// in the order they first appear.
pub(crate) struct Needed<'a> {
    pub already: usize,
    pub reused: Vec<SummaryLine>,
    pub texts: Vec<Pending<'a>>,
}

/// One text to summarize, and every action that reads as it.
pub(crate) struct Pending<'a> {
    pub key: String,
    pub text: String,
    pub actions: Vec<&'a ActionKey>,
}

pub(crate) fn needed<'a>(actions: &'a [DeepAction], store: &Store, summarizer: &str) -> Needed<'a> {
    let kept = store.summaries(summarizer, PROMPT_VERSION);
    let mut needed = Needed {
        already: 0,
        reused: Vec::new(),
        texts: Vec::new(),
    };
    let mut at: BTreeMap<String, usize> = BTreeMap::new();
    for action in actions {
        if kept.by_action.contains_key(&action.key) {
            needed.already += 1;
            continue;
        }
        let text = action.text();
        let key = super::action::text_key(&text);
        if let Some(summary) = kept.by_text.get(&key) {
            needed.reused.push(SummaryLine {
                action: action.key.clone(),
                summarizer: summarizer.to_owned(),
                prompt: PROMPT_VERSION.to_owned(),
                text: key,
                summary: summary.clone(),
            });
            continue;
        }
        match at.get(&key) {
            Some(&index) => needed.texts[index].actions.push(&action.key),
            None => {
                at.insert(key.clone(), needed.texts.len());
                needed.texts.push(Pending {
                    key,
                    text,
                    actions: vec![&action.key],
                });
            }
        }
    }
    needed
}

/// Summarizes every action of `actions` that has no summary from `summarizer` yet, sending at
/// most `most` texts when it's given. It stops between requests once `stop` is cancelled, and
/// when the summarizer fails or answers with no line it was asked for; what came back before
/// is kept either way.
pub(crate) async fn summarize(
    actions: &[DeepAction],
    store: &Store,
    summarizer: &dyn Summarizer,
    stop: &CancellationToken,
    progress: &mut (dyn FnMut(Progress) + Send),
    most: Option<usize>,
) -> io::Result<Summarized> {
    let id = summarizer.id().to_string();
    let needed = needed(actions, store, &id);
    store.add_summaries(&needed.reused)?;
    let mut done = Summarized {
        already: needed.already,
        reused: needed.reused.len(),
        ..Summarized::default()
    };
    let started = Instant::now();
    let total = actions.len();
    let to_send = needed.texts.len();
    let mut sent = 0;
    let report = |done: &Summarized, sent: usize, progress: &mut (dyn FnMut(Progress) + Send)| {
        progress(Progress {
            done: done.already + done.reused + done.written,
            total,
            elapsed: started.elapsed(),
            sent,
            to_send,
        });
    };
    report(&done, sent, progress);
    for batch in needed.texts.chunks(PER_REQUEST) {
        if stop.is_cancelled() {
            done.stopped = true;
            break;
        }
        if most.is_some_and(|most| sent >= most) {
            done.paused = true;
            break;
        }
        let texts: Vec<&str> = batch.iter().map(|pending| pending.text.as_str()).collect();
        let request = prompt::request(&texts);
        let reply = tokio::select! {
            reply = summarizer.answer(&request) => reply,
            () = stop.cancelled() => {
                done.stopped = true;
                break;
            }
        };
        done.requests += 1;
        let reply = match reply {
            Ok(reply) => reply,
            Err(why) => {
                done.failed = Some(why);
                break;
            }
        };
        let found = prompt::summaries(&reply, batch.len());
        if found.iter().all(Option::is_none) {
            done.failed = Some(unreadable(summarizer.name(), &reply));
            break;
        }
        let mut lines = Vec::new();
        for (pending, summary) in batch.iter().zip(found) {
            let Some(summary) = summary else {
                continue;
            };
            for action in &pending.actions {
                lines.push(SummaryLine {
                    action: (*action).clone(),
                    summarizer: id.clone(),
                    prompt: PROMPT_VERSION.to_owned(),
                    text: pending.key.clone(),
                    summary: summary.clone(),
                });
            }
        }
        store.add_summaries(&lines)?;
        done.written += lines.len();
        sent += batch.len();
        report(&done, sent, progress);
    }
    done.missing = total - done.already - done.reused - done.written;
    Ok(done)
}

/// How much of a reply's first line a reason quotes, in cells.
const QUOTED: usize = 80;

/// What a reply gave instead of numbered lines, by its first line.
fn unreadable(name: &str, reply: &str) -> String {
    let first = reply
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(sanitize_for_terminal)
        .unwrap_or_default();
    if first.is_empty() {
        return format!("{name} answered nothing");
    }
    format!(
        "{name} answered without the numbered lines asked for (\"{}\")",
        cut(&first, QUOTED, At::Word)
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::action::PatchCounts;
    use super::super::action::sample::shell;
    use super::super::summarize::fakes::{Echo, Says};
    use super::*;

    const QWEN: &str = "ollama/qwen3-coder:30b";

    fn actions(run: &str, count: usize) -> Vec<DeepAction> {
        (0..count)
            .map(|at| shell(run, &format!("c{at}"), &format!("command number {at}")))
            .collect()
    }

    async fn run(actions: &[DeepAction], store: &Store, summarizer: &dyn Summarizer) -> Summarized {
        summarize(
            actions,
            store,
            summarizer,
            &CancellationToken::new(),
            &mut |_| {},
            None,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn ten_actions_go_in_a_request_and_each_summary_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let echo = Echo::new(QWEN);
        let done = run(&actions("run-1", 23), &store, &echo).await;
        assert_eq!(echo.requests(), 3);
        assert_eq!((done.written, done.requests, done.missing), (23, 3, 0));
        let kept = store.summaries(QWEN, PROMPT_VERSION);
        assert_eq!(kept.by_action.len(), 23);
    }

    #[tokio::test]
    async fn a_second_update_summarizes_only_the_new_actions() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let first = Echo::new(QWEN);
        run(&actions("run-1", 12), &store, &first).await;
        let mut all = actions("run-1", 12);
        all.extend(actions("run-2", 3).into_iter().map(|mut action| {
            action.input = json!({ "command": format!("new {}", action.key.call) });
            action
        }));
        let second = Echo::new(QWEN);
        let done = run(&all, &store, &second).await;
        assert_eq!((done.already, done.written), (12, 3));
        assert_eq!(second.requests(), 1);
        assert_eq!(second.lines().len(), 3);
        let third = Echo::new(QWEN);
        let done = run(&all, &store, &third).await;
        assert_eq!((done.already, third.requests()), (15, 0));
    }

    #[tokio::test]
    async fn identical_actions_are_summarized_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let mut repeated = Vec::new();
        for at in 0..6 {
            repeated.push(shell("run-1", &format!("c{at}"), "git status"));
        }
        repeated.push(shell("run-1", "c9", "git log -5"));
        let echo = Echo::new(QWEN);
        let done = run(&repeated, &store, &echo).await;
        assert_eq!(echo.lines().len(), 2, "{:?}", echo.lines());
        assert_eq!(done.written, 7);
        // A later run's identical action takes the summary already written.
        let later = vec![shell("run-2", "c1", "git status")];
        let again = Echo::new(QWEN);
        let done = run(&later, &store, &again).await;
        assert_eq!((done.reused, again.requests()), (1, 0));
    }

    #[tokio::test]
    async fn an_interrupted_update_keeps_every_summary_it_finished() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let mut failing = Echo::new(QWEN);
        failing.fail_from = Some(2);
        let all = actions("run-1", 35);
        let done = run(&all, &store, &failing).await;
        assert_eq!((done.written, done.missing), (20, 15));
        assert!(done.failed.is_some());
        let rest = Echo::new(QWEN);
        let done = run(&all, &store, &rest).await;
        assert_eq!((done.already, done.written, rest.requests()), (20, 15, 2));
    }

    #[tokio::test]
    async fn a_reply_with_no_line_to_keep_stops_the_update_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        // Llama 3.2 numbered the one action it was sent 2.
        let says = Says::new(
            "ollama/llama3.2",
            "Llama 3.2",
            "2. Create a new file in place.",
        );
        let done = run(&actions("run-1", 1), &store, &says).await;
        assert_eq!((done.written, done.requests, done.missing), (0, 1, 1));
        assert_eq!(
            done.failed.as_deref(),
            Some(
                "Llama 3.2 answered without the numbered lines asked for (\"2. Create a new \
                 file in place.\")"
            )
        );
        let empty = Says::new("ollama/llama3.2", "Llama 3.2", "  \n");
        let done = run(&actions("run-1", 3), &store, &empty).await;
        assert_eq!(done.failed.as_deref(), Some("Llama 3.2 answered nothing"));
    }

    #[tokio::test]
    async fn stopping_ends_it_between_requests() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let stop = CancellationToken::new();
        let echo = Echo::new(QWEN);
        let mut seen = Vec::new();
        let all = actions("run-1", 30);
        let mut stop_at_ten = |progress: Progress| {
            seen.push(progress.done);
            if progress.done >= 10 {
                stop.cancel();
            }
        };
        let done = summarize(&all, &store, &echo, &stop, &mut stop_at_ten, None)
            .await
            .unwrap();
        assert!(done.stopped);
        assert_eq!((done.written, echo.requests()), (10, 1));
        assert_eq!(seen, vec![0, 10]);
    }

    #[tokio::test]
    async fn it_sends_no_more_than_it_is_let_and_says_more_wait() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let echo = Echo::new(QWEN);
        let all = actions("run-1", 35);
        let stop = CancellationToken::new();
        let first = summarize(&all, &store, &echo, &stop, &mut |_| {}, Some(20))
            .await
            .unwrap();
        assert!(first.paused);
        assert_eq!((first.written, echo.requests()), (20, 2));
        let rest = summarize(&all, &store, &echo, &stop, &mut |_| {}, Some(20))
            .await
            .unwrap();
        assert!(!rest.paused);
        assert_eq!((rest.already, rest.written), (20, 15));
    }

    #[tokio::test]
    async fn the_patch_counts_reach_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let mut append = shell("run-1", "c1", "");
        append.tool = "Edit".to_owned();
        append.input = json!({ "file_path": "tests/test_bucket.py" });
        append.patch = Some(PatchCounts {
            added: 12,
            removed: 0,
        });
        let echo = Echo::new(QWEN);
        run(&[append], &store, &echo).await;
        assert_eq!(
            echo.lines(),
            vec![
                "1. tool Edit: file_path: tests/test_bucket.py [patch: 12 lines added, 0 removed]"
            ]
        );
    }

    #[test]
    fn the_time_left_follows_the_pace_so_far() {
        let progress = Progress {
            done: 312,
            total: 562,
            elapsed: Duration::from_secs(60),
            sent: 300,
            to_send: 550,
        };
        assert_eq!(progress.left(), Some(Duration::from_secs(50)));
        let before = Progress {
            sent: 0,
            ..progress
        };
        assert_eq!(before.left(), None);
    }
}
