//! Reversals: your verdicts against the crew's work, as lines in the outcome log. They count
//! against the agents involved, and a canary's tripwire counts those against its runs. One is
//! written when you turn the work down at an approval, when a spot check finds an
//! auto-approval wrong, and when a canary run's pull request is closed unmerged or its merge
//! reverted.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;
use time::OffsetDateTime;

use argonauts_core::base::layout::{repo_state, run_files};
use argonauts_core::runs::events::approval::{AnswerAction, ApprovalKind};
use argonauts_core::runs::events::run::RunOutcome;
use argonauts_core::runs::events::{MergeEvent, RunEvent, RunEventKind};
use argonauts_core::{RunId, TaskId};
use argonauts_learning::Store;
use argonauts_learning::evolution::Evolution;
use argonauts_learning::outcomes::Reversal;

use super::outcomes::{self, LogLine, learned};
use crate::divide;
use crate::engine::error::EngineResult;
use crate::engine::host::Host;

/// Whether your answer turns down the crew's work, rather than steering a run's mechanics.
pub fn rejects(kind: ApprovalKind, action: &AnswerAction) -> bool {
    let about_the_work = matches!(
        kind,
        ApprovalKind::Understand
            | ApprovalKind::Plan
            | ApprovalKind::Merge
            | ApprovalKind::ChangeRequest
            | ApprovalKind::GoalChange
    );
    about_the_work
        && matches!(
            action,
            AnswerAction::SendBack
                | AnswerAction::Stop
                | AnswerAction::RequestChanges
                | AnswerAction::Abandon
        )
}

/// One line for a reversal at an approval, such as "you answered send_back at the plan
/// approval".
pub fn at_approval(kind: ApprovalKind, action: &AnswerAction) -> String {
    format!(
        "you answered {} at the {} approval",
        word(action),
        word(&kind)
    )
}

fn word<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(word)) => word.replace('_', " "),
        Ok(serde_json::Value::Object(map)) => map
            .keys()
            .next()
            .map_or_else(String::new, |key| key.replace('_', " ")),
        _ => String::new(),
    }
}

/// Logs a reversal against this run. The log is the evidence, never the run's result, so a
/// failure here is only a warning.
pub fn note(host: &Host, task: Option<&TaskId>, what: String) {
    let line = LogLine::Reversal(Reversal {
        run: host.run_id().clone(),
        task: task.cloned(),
        what,
        at: host.now(),
    });
    let state_dir = divide::repo_state_dir(host);
    if let Err(err) = outcomes::append_lines(&state_dir, host.run_id(), &[line]) {
        tracing::warn!(run = %host.run_id(), "a reversal wasn't logged: {err}");
    }
}

/// Where a pull request stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullState {
    Open,
    Closed,
    /// Merged as `commit`.
    Merged {
        commit: String,
    },
}

/// Looks up the pull request opened from a branch.
pub trait PullRequests {
    fn state(&self, root: &Path, branch: &str) -> Option<PullState>;
}

/// `gh pr view`, where `gh` and a remote exist. Anything it can't say is no answer.
pub struct Gh;

impl PullRequests for Gh {
    fn state(&self, root: &Path, branch: &str) -> Option<PullState> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct View {
            state: String,
            merge_commit: Option<Commit>,
        }
        #[derive(Deserialize)]
        struct Commit {
            oid: String,
        }
        which::which("gh").ok()?;
        // No remote, no pull request to ask about, and nothing leaves this machine.
        let remotes = Command::new("git")
            .arg("remote")
            .current_dir(root)
            .output()
            .ok()?;
        if remotes.stdout.trim_ascii().is_empty() {
            return None;
        }
        let out = Command::new("gh")
            .args(["pr", "view", branch, "--json", "state,mergeCommit"])
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let view: View = serde_json::from_slice(&out.stdout).ok()?;
        match view.state.as_str() {
            "OPEN" => Some(PullState::Open),
            "CLOSED" => Some(PullState::Closed),
            "MERGED" => Some(PullState::Merged {
                commit: view.merge_commit?.oid,
            }),
            _ => None,
        }
    }
}

/// For each run of a canary still going that opened a pull request and has no reversal yet: a
/// reversal when you closed the pull request unmerged or reverted its merge. Returns the new
/// lines, already appended.
pub fn canary_pull_requests(
    state_dir: &Path,
    repo_root: &Path,
    prs: &dyn PullRequests,
    now: OffsetDateTime,
) -> EngineResult<Vec<Reversal>> {
    let store = Store::new(state_dir, repo_root);
    let canaries = learned(Evolution::open(&store).and_then(|evolution| evolution.canaries()))?;
    let reversed: BTreeSet<RunId> = outcomes::read_all(state_dir)?
        .into_iter()
        .filter_map(|line| match line {
            LogLine::Reversal(reversal) => Some(reversal.run),
            _ => None,
        })
        .collect();
    let mut found = Vec::new();
    for canary in canaries.values() {
        for run in &canary.runs {
            if run.outcome != RunOutcome::PullRequest || reversed.contains(&run.run) {
                continue;
            }
            let Some(branch) = pull_request_branch(state_dir, &run.run) else {
                continue;
            };
            let what = match prs.state(repo_root, &branch) {
                Some(PullState::Closed) => "you closed its pull request without merging it",
                Some(PullState::Merged { commit }) if reverted(repo_root, &commit) => {
                    "you reverted its merge"
                }
                _ => continue,
            };
            let reversal = Reversal {
                run: run.run.clone(),
                task: None,
                what: format!(
                    "{what} ({} v{} canary)",
                    canary.learned_workflow, canary.version
                ),
                at: now,
            };
            outcomes::append_lines(state_dir, &run.run, &[LogLine::Reversal(reversal.clone())])?;
            found.push(reversal);
        }
    }
    Ok(found)
}

/// The branch the run's pull request was opened from, as its log recorded it.
fn pull_request_branch(state_dir: &Path, run: &RunId) -> Option<String> {
    let journal = state_dir
        .join(repo_state::RUNS)
        .join(run.as_str())
        .join(run_files::EVENTS);
    let text = std::fs::read_to_string(journal).ok()?;
    text.lines()
        .filter_map(|line| serde_json::from_str::<RunEvent>(line).ok())
        .find_map(|event| match event.kind {
            RunEventKind::Merge(MergeEvent::PullRequest { branch, .. }) => Some(branch),
            _ => None,
        })
}

/// Whether any branch has a commit that reverts `commit`.
fn reverted(root: &Path, commit: &str) -> bool {
    let grep = format!("This reverts commit {commit}");
    Command::new("git")
        .args([
            "log",
            "--all",
            "--fixed-strings",
            "--format=%H",
            "--grep",
            &grep,
        ])
        .current_dir(root)
        .output()
        .is_ok_and(|out| out.status.success() && !out.stdout.trim_ascii().is_empty())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use argonauts_core::runs::events::run::Party;
    use argonauts_learning::RunRecord;
    use argonauts_learning::evolution::state::{self, Canary};

    use super::*;

    struct Fake(RefCell<BTreeMap<String, PullState>>);

    impl PullRequests for Fake {
        fn state(&self, _root: &Path, branch: &str) -> Option<PullState> {
            self.0.borrow().get(branch).cloned()
        }
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@local"])
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn run(n: u32, outcome: RunOutcome) -> RunRecord {
        RunRecord {
            run: RunId::new(format!("run-{n:03}")).unwrap(),
            at: OffsetDateTime::UNIX_EPOCH,
            task_type: "backend".to_owned(),
            learned_workflow: None,
            workflow: None,
            outcome,
            stuck_tasks: 0,
            final_checks_passed: true,
            cost_usd: 1.0,
            minutes: 5.0,
            canary: true,
        }
    }

    /// Writes the journal of a run that opened its pull request from `branch`.
    fn opened_from(state_dir: &Path, run: &RunId, branch: &str) {
        let event = RunEvent {
            seq: 1,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: run.clone(),
            task_id: None,
            session: None,
            source: "argonauts-code".to_owned(),
            kind: RunEventKind::Merge(MergeEvent::PullRequest {
                branch: branch.to_owned(),
                url: None,
                command: None,
            }),
        };
        let dir = state_dir.join("runs").join(run.as_str());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("events.jsonl"),
            format!("{}\n", serde_json::to_string(&event).unwrap()),
        )
        .unwrap();
    }

    /// A canary run's pull request closed unmerged, or merged and then reverted, is a
    /// reversal against that run, written once. One still open, merged and kept, or a run
    /// that merged without one, is not.
    #[test]
    fn closed_or_reverted_canary_pull_requests_are_reversals() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "first"]);
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "the canary's change"]);
        let merged = git(&root, &["rev-parse", "HEAD"]);
        git(&root, &["revert", "--no-edit", &merged]);
        std::fs::write(root.join("c.txt"), "c\n").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "another canary's change"]);
        let kept = git(&root, &["rev-parse", "HEAD"]);

        let runs = vec![
            run(1, RunOutcome::PullRequest),
            run(2, RunOutcome::PullRequest),
            run(3, RunOutcome::PullRequest),
            run(4, RunOutcome::PullRequest),
            run(5, RunOutcome::Merged),
        ];
        let branch = |run: &RunRecord| format!("argonauts/{}/merged", run.run);
        for record in &runs {
            opened_from(&state_dir, &record.run, &branch(record));
        }
        let fake = Fake(RefCell::new(BTreeMap::from([
            (branch(&runs[0]), PullState::Closed),
            (
                branch(&runs[1]),
                PullState::Merged {
                    commit: merged.clone(),
                },
            ),
            (branch(&runs[2]), PullState::Open),
            (branch(&runs[3]), PullState::Merged { commit: kept }),
            (branch(&runs[4]), PullState::Closed),
        ])));
        let canaries = BTreeMap::from([(
            "api-feature".to_owned(),
            Canary {
                learned_workflow: "api-feature".to_owned(),
                candidate: "api-feature-v2".to_owned(),
                version: 2,
                parent: 1,
                started: OffsetDateTime::UNIX_EPOCH,
                by: Party::Rules,
                runs: runs.clone(),
            },
        )]);
        state::write(
            &state_dir.join("evolution").join(state::CANARIES),
            &canaries,
        )
        .unwrap();

        let now = OffsetDateTime::UNIX_EPOCH;
        let found = canary_pull_requests(&state_dir, &root, &fake, now).unwrap();
        let reversed: Vec<(&str, &str)> = found
            .iter()
            .map(|reversal| (reversal.run.as_str(), reversal.what.as_str()))
            .collect();
        assert_eq!(reversed.len(), 2, "{reversed:?}");
        assert_eq!(reversed[0].0, "run-001");
        assert!(reversed[0].1.contains("closed"), "{reversed:?}");
        assert_eq!(reversed[1].0, "run-002");
        assert!(reversed[1].1.contains("reverted"), "{reversed:?}");
        let logged = outcomes::read_all(&state_dir).unwrap();
        assert_eq!(
            logged
                .iter()
                .filter(|line| matches!(line, LogLine::Reversal(_)))
                .count(),
            2
        );
        // Once each: the next run review finds nothing new.
        assert!(
            canary_pull_requests(&state_dir, &root, &fake, now)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn only_turning_the_work_down_is_a_reversal() {
        assert!(rejects(ApprovalKind::Plan, &AnswerAction::SendBack));
        assert!(rejects(ApprovalKind::Merge, &AnswerAction::Abandon));
        assert!(rejects(ApprovalKind::ChangeRequest, &AnswerAction::Stop));
        assert!(!rejects(ApprovalKind::Merge, &AnswerAction::Merge));
        assert!(!rejects(ApprovalKind::Limit, &AnswerAction::Stop));
        assert!(!rejects(ApprovalKind::AgentDown, &AnswerAction::Stop));
    }
}
