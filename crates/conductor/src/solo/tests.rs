use std::time::Duration;

use argonauts_core::{BudgetUnit, Capabilities, EventLog, ReportStatus, RunEvent, StopCause};
use tokio::sync::mpsc;

use super::*;
use crate::decision::FixedDecider;
use crate::testutil::{ScriptAdapter, TestRepo, final_line, git_test_env, kind_name, usage_line};

const TASK: &str = "Create a file named hello.txt containing the word hi";

const REPORT: &str = r#"{"type":"result","text":null,"structured":{"status":"done","summary":"Created hello.txt.","files_changed":["hello.txt"],"tests_run":[],"concerns":[]},"is_error":false}"#;

fn hello_agent() -> String {
    format!(
        r#"case "$1" in *"{TASK}"*) ;; *) echo 'prompt is missing the task' >&2; exit 9 ;; esac
echo '{{"type":"init","session_id":"s-1","model":"tiny","tools":["Write"]}}'
echo '{{"type":"tool_started","id":"t1","name":"Write","input":{{"path":"hello.txt"}},"parent":null}}'
echo hi > hello.txt
echo '{{"type":"tool_finished","id":"t1","ok":true,"output":null,"parent":null}}'
echo '{usage}'
echo '{REPORT}'"#,
        usage = usage_line(1200, 80, Some(0.02)),
    )
}

struct Setup {
    repo: TestRepo,
    home: tempfile::TempDir,
    config: Config,
    settings: ConductorSettings,
}

impl Setup {
    fn new() -> Self {
        let mut config = Config::default();
        config.checks.commands = vec![argv(&["test", "-f", "hello.txt"])];
        Self {
            repo: TestRepo::new(),
            home: tempfile::tempdir().unwrap(),
            config,
            settings: ConductorSettings {
                exit_grace: Duration::from_millis(300),
                git_env: git_test_env(),
                ..ConductorSettings::default()
            },
        }
    }

    fn solo(&self, script: &str) -> SoloRun {
        SoloRun::new(
            TASK,
            &self.repo.root,
            Arc::new(ScriptAdapter::new(script)),
            self.config.clone(),
            Paths::new(self.home.path()),
        )
        .settings(self.settings.clone())
        .run_id(RunId::new("run-test").unwrap())
    }

    async fn run<D: MergeDecider>(
        &self,
        script: &str,
        decider: &mut D,
    ) -> (SoloOutcome, Vec<RunEvent>) {
        let outcome = self.solo(script).run(decider).await.unwrap();
        let events = EventLog::read(&outcome.event_log).unwrap().events;
        (outcome, events)
    }

    fn worktree(&self) -> PathBuf {
        let repo_id = RepoId::from_canonical_path(&self.repo.root);
        Paths::new(self.home.path()).worktree_dir(
            &repo_id,
            &RunId::new("run-test").unwrap(),
            &argonauts_core::CardId::numbered(1),
        )
    }
}

const BRANCH: &str = "argonauts/run-test/task-001";

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

/// Pipeline structure without the agent's own events.
fn trace(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::Phase(p) => Some(format!("{}:{}", name_of(&p.phase), name_of(&p.status))),
            RunEventKind::Agent(_)
            | RunEventKind::Budget(_)
            | RunEventKind::SessionStarted(_)
            | RunEventKind::SessionEnded(_)
            | RunEventKind::Note(_) => None,
            other => Some(kind_name(other)),
        })
        .collect()
}

fn name_of<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

fn notes(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::Note(note) => Some(note.text.clone()),
            _ => None,
        })
        .collect()
}

fn merge_card(events: &[RunEvent]) -> MergeCard {
    events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::Checkpoint(Checkpoint::Merge(card)) => Some(card.clone()),
            _ => None,
        })
        .expect("no merge approval")
}

fn decision(events: &[RunEvent]) -> DecisionRecord {
    events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::Decision(record) => Some(record.clone()),
            _ => None,
        })
        .expect("no decision")
}

struct RecordingDecider {
    answer: MergeDecision,
    seen: Vec<MergeRequest>,
}

impl RecordingDecider {
    fn new(answer: MergeDecision) -> Self {
        Self {
            answer,
            seen: Vec::new(),
        }
    }
}

impl MergeDecider for RecordingDecider {
    async fn decide(&mut self, request: MergeRequest) -> anyhow::Result<MergeDecision> {
        self.seen.push(request);
        Ok(self.answer.clone())
    }
}

struct ActThenDecide<F: FnMut() + Send> {
    before: F,
    answer: MergeDecision,
}

impl<F: FnMut() + Send> MergeDecider for ActThenDecide<F> {
    async fn decide(&mut self, _request: MergeRequest) -> anyhow::Result<MergeDecision> {
        (self.before)();
        Ok(self.answer.clone())
    }
}

const ISOLATED_GIT: &str = "GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git -c user.name=T -c user.email=t@example.com";

struct NeverAsked;

impl MergeDecider for NeverAsked {
    async fn decide(&mut self, _request: MergeRequest) -> anyhow::Result<MergeDecision> {
        panic!("the merge approval should not have been shown")
    }
}

#[tokio::test]
async fn approve_merges_into_the_starting_branch() {
    let setup = Setup::new();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut decider = RecordingDecider::new(MergeDecision::Approve);
    let outcome = setup
        .solo(&hello_agent())
        .on_event(move |event| {
            let _ = tx.send(event);
        })
        .run(&mut decider)
        .await
        .unwrap();
    let events = EventLog::read(&outcome.event_log).unwrap().events;

    assert_eq!(outcome.outcome, RunOutcome::Merged, "{}", outcome.detail);
    let tip = setup.repo.git(&["rev-parse", BRANCH]);
    assert_eq!(
        outcome.detail,
        format!("Merged {BRANCH} into main at {}.", &tip[..7])
    );
    assert_eq!(setup.repo.read("hello.txt").as_deref(), Some("hi\n"));
    assert_eq!(outcome.branch.as_deref(), Some(BRANCH));
    assert_eq!(outcome.commit, Some(setup.repo.git(&["rev-parse", BRANCH])));
    assert_eq!(outcome.worktree, None);
    assert!(!setup.worktree().exists());

    assert_eq!(
        trace(&events),
        [
            "run_started",
            "intake:finished",
            "goal:finished",
            "worktree:started",
            "worktree:finished",
            "agent:started",
            "report",
            "agent:finished",
            "commit:started",
            "commit",
            "commit:finished",
            "gates:started",
            "gate_result",
            "gates:finished",
            "checkpoint",
            "decision:started",
            "decision",
            "decision:finished",
            "merge:started",
            "merge:finished",
            "cleanup:started",
            "cleanup:finished",
            "run_finished",
        ]
    );

    let mut forwarded = Vec::new();
    while let Ok(event) = rx.try_recv() {
        forwarded.push(event);
    }
    assert_eq!(forwarded, events);

    let RunEventKind::RunStarted(started) = &events[0].kind else {
        panic!("the log must start with run_started")
    };
    assert_eq!(started.mode, "solo");
    assert_eq!(started.agent, "fake");
    assert_eq!(started.base_branch.as_deref(), Some("main"));
    assert_eq!(started.repo_root, setup.repo.root);
    assert_eq!(started.config_hash, setup.config.resolved_hash());

    let prompt = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::SessionStarted(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(prompt.cwd, setup.worktree());
    assert!(prompt.prompt.contains(TASK));
    assert!(prompt.prompt.contains("\"files_changed\""));
    assert_eq!(
        prompt.goal_hash,
        Some(argonauts_core::sha256_hex(TASK.as_bytes()))
    );

    let request = &decider.seen[0];
    assert_eq!(request.card.target_branch.as_deref(), Some("main"));
    assert_eq!(request.card.stat.files_changed, 1);
    assert!(request.card.gates[0].passed);
    assert_eq!(
        request.card.report.as_ref().unwrap().status,
        ReportStatus::Done
    );
    assert!(request.diff.contains("+hi"));
    assert!(!request.diff_truncated);
    assert_eq!(
        std::fs::read_to_string(&request.diff_path).unwrap(),
        request.diff
    );
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&request.diff_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let message = setup.repo.git(&["log", "-1", "--format=%B", BRANCH]);
    assert!(message.starts_with(TASK), "{message}");
    assert!(message.contains("Created hello.txt."));
    assert!(message.contains("Argonauts-Run: run-test"));
    assert!(message.contains("Argonauts-Agent: fake"));
    assert_eq!(decision(&events).by, DecidedBy::User);
    assert_eq!(decision(&events).commit, Some(tip));
}

#[tokio::test]
async fn failing_gate_shows_on_the_merge_card() {
    let mut setup = Setup::new();
    setup.config.checks.commands = vec![
        argv(&["test", "-f", "missing.txt"]),
        argv(&["test", "-f", "hello.txt"]),
    ];
    let (outcome, events) = setup
        .run(&hello_agent(), &mut FixedDecider(MergeDecision::Leave))
        .await;

    assert_eq!(outcome.outcome, RunOutcome::BranchLeft);
    let card = merge_card(&events);
    assert!(!card.gates[0].passed);
    assert_eq!(card.gates[0].exit_code, Some(1));
    assert!(card.gates[1].passed);
    assert!(trace(&events).contains(&"gates:failed".to_owned()));
    assert_eq!(setup.repo.read("hello.txt"), None);
    assert_eq!(
        setup.repo.git(&["show", &format!("{BRANCH}:hello.txt")]),
        "hi"
    );
}

#[tokio::test]
async fn a_dirty_tree_blocks_the_merge() {
    let setup = Setup::new();
    setup.repo.write("README.md", "local edit\n");
    let (outcome, events) = setup
        .run(&hello_agent(), &mut FixedDecider(MergeDecision::Approve))
        .await;

    assert_eq!(outcome.outcome, RunOutcome::BranchLeft);
    assert!(
        outcome.detail.contains("uncommitted changes to 1 file"),
        "{}",
        outcome.detail
    );
    assert!(outcome.detail.contains(&format!("git merge {BRANCH}")));
    assert_eq!(
        setup.repo.read("README.md").as_deref(),
        Some("local edit\n")
    );
    assert_eq!(setup.repo.read("hello.txt"), None);
    assert_eq!(outcome.branch.as_deref(), Some(BRANCH));
    assert!(trace(&events).contains(&"merge:skipped".to_owned()));
    assert!(notes(&events)[0].contains("uncommitted changes are not part of this run"));
}

#[tokio::test]
async fn budget_stop_keeps_the_work_so_far() {
    let mut setup = Setup::new();
    setup.config.budget.per_task = 0.05;
    let script = format!(
        "echo hi > hello.txt
         echo '{}'
         sleep 30",
        usage_line(5000, 500, Some(0.10))
    );
    let started = std::time::Instant::now();
    let (outcome, events) = setup
        .run(&script, &mut FixedDecider(MergeDecision::Leave))
        .await;

    assert!(started.elapsed() < Duration::from_secs(10));
    let ended = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::SessionEnded(end) => Some(end.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(ended.stopped_by, Some(StopCause::Budget));
    let card = merge_card(&events);
    assert_eq!(card.budget.unwrap().level, BudgetAction::Stop);
    assert_eq!(card.report, None);
    assert_eq!(card.stopped_by, Some(StopCause::Budget));
    assert!(trace(&events).contains(&"agent:failed".to_owned()));
    assert_eq!(outcome.outcome, RunOutcome::BranchLeft);
    assert_eq!(
        setup.repo.git(&["show", &format!("{BRANCH}:hello.txt")]),
        "hi"
    );
    assert!(
        notes(&events)
            .iter()
            .any(|n| n.contains("did not send a valid report"))
    );
}

#[tokio::test]
async fn no_changes_fails_and_removes_the_empty_branch() {
    let setup = Setup::new();
    let (outcome, events) = setup
        .run(&format!("echo '{REPORT}'"), &mut NeverAsked)
        .await;

    assert_eq!(outcome.outcome, RunOutcome::Failed);
    assert_eq!(
        outcome.detail,
        "The agent made no changes. It reported: Created hello.txt."
    );
    assert_eq!(outcome.branch, None);
    assert_eq!(outcome.commit, None);
    assert!(
        setup
            .repo
            .git(&["branch", "--list", "argonauts/*"])
            .is_empty()
    );
    assert!(!setup.worktree().exists());
    let trace = trace(&events);
    assert!(trace.contains(&"commit:skipped".to_owned()));
    assert!(trace.contains(&"gates:skipped".to_owned()));
    assert!(!trace.contains(&"checkpoint".to_owned()));
}

#[tokio::test]
async fn an_agent_that_cannot_start_fails_the_run() {
    let setup = Setup::new();
    let adapter = ScriptAdapter::new("true").with_program("argonauts-no-such-agent");
    let outcome = SoloRun::new(
        TASK,
        &setup.repo.root,
        Arc::new(adapter),
        setup.config.clone(),
        Paths::new(setup.home.path()),
    )
    .settings(setup.settings.clone())
    .run(&mut NeverAsked)
    .await
    .unwrap();
    assert_eq!(outcome.outcome, RunOutcome::Failed);
    assert!(
        outcome.detail.contains("argonauts-no-such-agent"),
        "{}",
        outcome.detail
    );
    assert!(outcome.run_id.as_str().starts_with("run-"));
    assert!(
        setup
            .repo
            .git(&["branch", "--list", "argonauts/*"])
            .is_empty()
    );
}

#[tokio::test]
async fn merge_mode_leave_does_not_ask() {
    let mut setup = Setup::new();
    setup.config.merge.mode = MergeMode::Leave;
    let (outcome, events) = setup.run(&hello_agent(), &mut NeverAsked).await;
    assert_eq!(outcome.outcome, RunOutcome::BranchLeft);
    assert_eq!(
        decision(&events),
        DecisionRecord {
            checkpoint: CheckpointKind::Merge,
            decision: MergeDecision::Leave,
            by: DecidedBy::Config,
            commit: Some(setup.repo.git(&["rev-parse", BRANCH])),
        }
    );
    assert!(trace(&events).contains(&"checkpoint".to_owned()));
}

#[tokio::test]
async fn stopping_mid_session_cancels_but_commits_the_work() {
    let setup = Setup::new();
    let stop = StopHandle::new();
    let trigger = stop.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        trigger.stop();
    });
    let outcome = setup
        .solo("echo hi > hello.txt; sleep 30")
        .stop_handle(stop)
        .run(&mut NeverAsked)
        .await
        .unwrap();
    assert_eq!(outcome.outcome, RunOutcome::Cancelled);
    assert_eq!(
        outcome.detail,
        format!("Stopped. The work so far is committed on {BRANCH}.")
    );
    assert_eq!(
        setup.repo.git(&["show", &format!("{BRANCH}:hello.txt")]),
        "hi"
    );
    assert!(!setup.worktree().exists());
}

#[tokio::test]
async fn a_stop_before_the_agent_starts_never_launches_it() {
    let setup = Setup::new();
    let stop = StopHandle::new();
    stop.stop();
    let outcome = setup
        .solo(&hello_agent())
        .stop_handle(stop)
        .run(&mut NeverAsked)
        .await
        .unwrap();
    let events = EventLog::read(&outcome.event_log).unwrap().events;
    assert_eq!(outcome.outcome, RunOutcome::Cancelled);
    assert_eq!(outcome.detail, "Stopped before the agent changed anything.");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, RunEventKind::SessionStarted(_)))
    );
    assert_eq!(outcome.branch, None);
    assert!(
        setup
            .repo
            .git(&["branch", "--list", "argonauts/*"])
            .is_empty()
    );
    assert!(!setup.worktree().exists());
}

#[tokio::test]
async fn stopping_at_the_merge_card_leaves_the_branch() {
    let setup = Setup::new();
    let stop = StopHandle::new();
    let (mut decider, mut pending) = crate::decision::decision_channel();
    let trigger = stop.clone();
    tokio::spawn(async move {
        let card = pending.recv().await.unwrap();
        trigger.stop();
        tokio::time::sleep(Duration::from_millis(50)).await;
        card.answer(MergeDecision::Approve);
    });
    let outcome = setup
        .solo(&hello_agent())
        .stop_handle(stop)
        .run(&mut decider)
        .await
        .unwrap();
    assert_eq!(outcome.outcome, RunOutcome::Cancelled);
    assert_eq!(setup.repo.read("hello.txt"), None);
    assert_eq!(outcome.branch.as_deref(), Some(BRANCH));
}

#[tokio::test]
async fn open_pr_suggests_the_commands() {
    let setup = Setup::new();
    let (outcome, events) = setup
        .run(&hello_agent(), &mut FixedDecider(MergeDecision::OpenPr))
        .await;
    assert_eq!(outcome.outcome, RunOutcome::BranchLeft);
    let notes = notes(&events);
    let suggestion = notes.iter().find(|n| n.contains("pull request")).unwrap();
    assert!(suggestion.contains(&format!("git push -u origin {BRANCH}")));
    assert!(suggestion.contains(&format!("gh pr create --head {BRANCH} --base main --fill")));
}

#[tokio::test]
async fn request_changes_is_recorded_and_the_branch_left() {
    let setup = Setup::new();
    let answer = MergeDecision::RequestChanges {
        note: Some("say hello instead".into()),
    };
    let (outcome, events) = setup
        .run(&hello_agent(), &mut FixedDecider(answer.clone()))
        .await;
    assert_eq!(outcome.outcome, RunOutcome::BranchLeft);
    assert!(outcome.detail.contains("(say hello instead)"));
    assert_eq!(decision(&events).decision, answer);
}

#[tokio::test]
async fn a_detached_start_has_nothing_to_merge_into() {
    let setup = Setup::new();
    setup.repo.git(&["checkout", "--quiet", "--detach"]);
    let (outcome, events) = setup
        .run(&hello_agent(), &mut FixedDecider(MergeDecision::Approve))
        .await;
    assert_eq!(outcome.outcome, RunOutcome::BranchLeft);
    assert!(outcome.detail.starts_with("No branch was checked out"));
    assert_eq!(merge_card(&events).target_branch, None);
}

#[tokio::test]
async fn interrupted_wrap_up_resumes_for_the_report() {
    let mut setup = Setup::new();
    setup.config.budget.per_task = 1.0;
    let script = format!(
        r#"if [ -z "$2" ]; then
  trap 'exit 130' INT
  echo '{{"type":"init","session_id":"s-1","model":"tiny","tools":[]}}'
  echo hi > hello.txt
  echo '{first}'
  while :; do sleep 0.1; done
else
  printf '{{"type":"message","text":"%s","parent":null}}\n' "$1"
  echo '{second}'
  echo '{REPORT}'
fi"#,
        first = usage_line(1000, 100, Some(0.9)),
        // A resumed session reports the whole conversation's totals, earlier runs included.
        second = usage_line(1100, 110, Some(0.95)),
    );
    let (outcome, events) = setup
        .run(&script, &mut FixedDecider(MergeDecision::Leave))
        .await;

    let sessions: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::SessionStarted(s) => Some(s.prompt.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[1], WRAP_UP_MESSAGE);
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        RunEventKind::Agent(argonauts_core::AgentEvent::Message { text, .. }) if text == WRAP_UP_MESSAGE
    )));
    let card = merge_card(&events);
    assert_eq!(card.report.unwrap().status, ReportStatus::Done);
    let budget = card.budget.unwrap();
    assert!((budget.spend.cost_usd - 0.95).abs() < 1e-9, "{budget:?}");
    assert_eq!(budget.spend.input_tokens, 1100);
    assert_eq!(budget.level, BudgetAction::WrapUp);
    assert!(trace(&events).contains(&"agent:finished".to_owned()));
    assert_eq!(outcome.outcome, RunOutcome::BranchLeft);

    let args: Vec<Vec<String>> = events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::SessionStarted(s) => Some(s.args.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(args[1].last().map(String::as_str), Some("s-1"));
}

/// Interrupted at the wrap-up point, after writing `path` in its worktree.
fn wrap_up_agent_writing(path: &str) -> String {
    format!(
        r#"if [ -z "$2" ]; then
  trap 'exit 130' INT
  echo '{{"type":"init","session_id":"s-1","model":"tiny","tools":[]}}'
  mkdir -p "$(dirname '{path}')" && echo '{{}}' > '{path}'
  echo hi > hello.txt
  echo '{usage}'
  while :; do sleep 0.1; done
else
  echo '{REPORT}'
fi"#,
        usage = usage_line(1000, 100, Some(0.9)),
    )
}

#[tokio::test]
async fn no_resume_after_the_agent_changed_agent_config() {
    let mut setup = Setup::new();
    setup.config.budget.per_task = 1.0;
    let (outcome, events) = setup
        .run(
            &wrap_up_agent_writing(".claude/settings.json"),
            &mut FixedDecider(MergeDecision::Leave),
        )
        .await;
    let sessions = events
        .iter()
        .filter(|e| matches!(e.kind, RunEventKind::SessionStarted(_)))
        .count();
    assert_eq!(sessions, 1);
    let notes = notes(&events);
    assert!(
        notes
            .iter()
            .any(|n| n.contains("Not resuming") && n.contains(".claude/settings.json")),
        "{notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|n| n.contains("off limits") && n.contains(".claude/settings.json")),
        "{notes:?}"
    );
    assert_eq!(outcome.outcome, RunOutcome::BranchLeft);
}

#[tokio::test]
async fn usage_with_the_final_result_does_not_stop_or_fail_the_agent() {
    let mut setup = Setup::new();
    setup.config.budget.per_task = 0.05;
    // Like Codex, usage arrives only with the result, after the work is done.
    let script = format!(
        "echo hi > hello.txt
         echo '{last}'
         sleep 0.2",
        last = final_line(&usage_line(5000, 500, Some(0.10)), REPORT),
    );
    let (outcome, events) = setup
        .run(&script, &mut FixedDecider(MergeDecision::Leave))
        .await;
    let ended = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::SessionEnded(end) => Some(end.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(ended.stopped_by, None);
    assert_eq!(ended.exit_code, Some(0));
    assert!(trace(&events).contains(&"agent:finished".to_owned()));
    let card = merge_card(&events);
    assert_eq!(card.stopped_by, None);
    assert_eq!(card.budget.unwrap().level, BudgetAction::Stop);
    assert_eq!(card.report.unwrap().status, ReportStatus::Done);
    assert!(
        notes(&events)
            .iter()
            .any(|n| n == "The agent finished, but it used up the limit of $0.05.")
    );
    assert_eq!(outcome.outcome, RunOutcome::BranchLeft);
}

#[tokio::test]
async fn changes_under_argonauts_dir_are_flagged() {
    let setup = Setup::new();
    let script = format!(
        "mkdir -p .argonauts && echo 'x = 1' > .argonauts/config.toml
         echo hi > hello.txt
         echo '{REPORT}'"
    );
    let (_, events) = setup
        .run(&script, &mut FixedDecider(MergeDecision::Leave))
        .await;
    assert!(
        notes(&events)
            .iter()
            .any(|n| n.contains("off limits: .argonauts/config.toml"))
    );
}

#[tokio::test]
async fn a_worktree_moved_off_its_branch_is_kept() {
    let setup = Setup::new();
    let script = "git checkout --quiet -b elsewhere && echo hi > hello.txt";
    let (outcome, events) = setup.run(script, &mut NeverAsked).await;
    assert_eq!(outcome.outcome, RunOutcome::Failed);
    assert!(
        outcome.detail.contains("no longer on"),
        "{}",
        outcome.detail
    );
    assert_eq!(outcome.worktree, Some(setup.worktree()));
    assert!(setup.worktree().join("hello.txt").exists());
    assert!(trace(&events).contains(&"commit:failed".to_owned()));
    assert!(trace(&events).contains(&"cleanup:skipped".to_owned()));
}

#[tokio::test]
async fn setup_errors_are_returned() {
    let setup = Setup::new();
    let not_a_repo = tempfile::tempdir().unwrap();
    let solo = |task: &str, repo: &Path| {
        SoloRun::new(
            task,
            repo,
            Arc::new(ScriptAdapter::new("true")),
            setup.config.clone(),
            Paths::new(setup.home.path()),
        )
        .settings(setup.settings.clone())
    };
    let err = solo(TASK, not_a_repo.path())
        .run(&mut NeverAsked)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("not inside a git repository"),
        "{err:#}"
    );

    let err = solo("   ", &setup.repo.root)
        .run(&mut NeverAsked)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("empty"));

    let empty = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(empty.path())
        .envs(git_test_env())
        .status()
        .unwrap();
    let err = solo(TASK, empty.path())
        .run(&mut NeverAsked)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no commits yet"), "{err:#}");
}

#[test]
fn the_run_future_can_be_spawned() {
    fn assert_send<T: Send>(_: &T) {}
    let setup = Setup::new();
    let mut decider = FixedDecider(MergeDecision::Leave);
    let future = setup.solo("true").run(&mut decider);
    assert_send(&future);
    let (mut channel, _pending) = crate::decision::decision_channel();
    let future = setup.solo("true").run(&mut channel);
    assert_send(&future);
}

#[test]
fn budget_units_other_than_usd_have_no_native_cap() {
    let mut config = Config::default();
    config.budget.unit = BudgetUnit::Tokens;
    assert_eq!(native_cap(config.budget.per_task_limit(), 0.0), None);
}

#[test]
fn dollar_budgets_need_dollars() {
    let tokens_only = ScriptAdapter::new("true").with_capabilities(Capabilities {
        reports_model: false,
        ..Capabilities::default()
    });
    let mut config = Config::default();
    let problem = budget_problem(&config, &tokens_only).unwrap();
    assert!(problem.contains("doesn't say which model"), "{problem}");
    assert!(problem.contains("--model tiny"), "{problem}");

    config.agents.insert(
        "fake".into(),
        argonauts_core::AgentConfig {
            model: Some("mystery-1".into()),
            ..Default::default()
        },
    );
    let problem = budget_problem(&config, &tokens_only).unwrap();
    assert!(problem.contains("no price for mystery-1"), "{problem}");

    config.agents.get_mut("fake").unwrap().model = Some("gpt-6-luna".into());
    assert_eq!(budget_problem(&config, &tokens_only), None);

    config.budget.unit = BudgetUnit::Tokens;
    config.agents.get_mut("fake").unwrap().model = Some("mystery-1".into());
    assert_eq!(budget_problem(&config, &tokens_only), None);

    let reports_cost = ScriptAdapter::new("true");
    assert_eq!(budget_problem(&Config::default(), &reports_cost), None);
}

#[tokio::test]
async fn a_dollar_budget_counts_tokens_at_the_model_price() {
    let mut setup = Setup::new();
    setup.config.budget.per_task = 0.05;
    setup.config.agents.insert(
        "fake".into(),
        argonauts_core::AgentConfig {
            model: Some("gpt-6-luna".into()),
            ..Default::default()
        },
    );
    // A million input tokens at $0.10 per million, with no cost in the usage event.
    let usage = serde_json::to_string(&argonauts_core::AgentEvent::Usage {
        input_tokens: 1_000_000,
        output_tokens: 0,
        cached_tokens: 0,
        cost_usd: None,
        cumulative: false,
    })
    .unwrap();
    let script = format!("echo hi > hello.txt; echo '{usage}'; sleep 30");
    let adapter = ScriptAdapter::new(&script).with_capabilities(Capabilities::default());
    let started = std::time::Instant::now();
    let outcome = SoloRun::new(
        TASK,
        &setup.repo.root,
        Arc::new(adapter),
        setup.config.clone(),
        Paths::new(setup.home.path()),
    )
    .settings(setup.settings.clone())
    .run_id(RunId::new("run-test").unwrap())
    .run(&mut FixedDecider(MergeDecision::Leave))
    .await
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(10));
    let events = EventLog::read(&outcome.event_log).unwrap().events;
    let card = merge_card(&events);
    assert_eq!(card.stopped_by, Some(StopCause::Budget));
    let spend = card.budget.unwrap().spend;
    assert!((spend.cost_usd - 0.10).abs() < 1e-9, "{spend:?}");
    assert!(spend.cost_estimated);
}

#[tokio::test]
async fn an_unenforceable_budget_is_refused_before_anything_starts() {
    let setup = Setup::new();
    let adapter = ScriptAdapter::new("true").with_capabilities(Capabilities {
        reports_model: false,
        ..Capabilities::default()
    });
    let err = SoloRun::new(
        TASK,
        &setup.repo.root,
        Arc::new(adapter),
        setup.config.clone(),
        Paths::new(setup.home.path()),
    )
    .settings(setup.settings.clone())
    .run(&mut NeverAsked)
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("can't enforce a dollar limit"),
        "{err}"
    );
    assert!(
        setup
            .repo
            .git(&["branch", "--list", "argonauts/*"])
            .is_empty()
    );
}

#[tokio::test]
async fn a_rejected_command_fails_before_the_worktree_exists() {
    let mut setup = Setup::new();
    setup.config.agents.insert(
        "fake".into(),
        argonauts_core::AgentConfig {
            model: Some("--oops".into()),
            ..Default::default()
        },
    );
    let (outcome, events) = setup.run(&hello_agent(), &mut NeverAsked).await;
    assert_eq!(outcome.outcome, RunOutcome::Failed);
    assert!(
        outcome.detail.contains("could not be built"),
        "{}",
        outcome.detail
    );
    assert_eq!(outcome.worktree, None);
    assert!(!setup.worktree().exists());
    assert!(
        setup
            .repo
            .git(&["branch", "--list", "argonauts/*"])
            .is_empty()
    );
    assert!(!trace(&events).contains(&"worktree:started".to_owned()));
}

#[tokio::test]
async fn switching_branches_before_approving_blocks_the_merge() {
    let setup = Setup::new();
    let root = setup.repo.root.clone();
    let mut decider = ActThenDecide {
        before: || {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["checkout", "--quiet", "-b", "elsewhere"])
                .envs(git_test_env())
                .status()
                .unwrap();
            assert!(status.success());
        },
        answer: MergeDecision::Approve,
    };
    let (outcome, _) = setup.run(&hello_agent(), &mut decider).await;
    assert_eq!(outcome.outcome, RunOutcome::BranchLeft);
    assert!(
        outcome.detail.contains("now on elsewhere, not main"),
        "{}",
        outcome.detail
    );
    assert_eq!(setup.repo.read("hello.txt"), None);
}

#[tokio::test]
async fn a_conflicting_merge_is_undone_and_the_branch_left() {
    let setup = Setup::new();
    let script = format!(
        "echo theirs > README.md
         cd '{root}' && echo ours > README.md && {ISOLATED_GIT} commit --quiet -am ours
         echo '{REPORT}'",
        root = setup.repo.root.display()
    );
    let (outcome, events) = setup
        .run(&script, &mut FixedDecider(MergeDecision::Approve))
        .await;
    assert_eq!(outcome.outcome, RunOutcome::BranchLeft);
    assert!(
        outcome.detail.contains("merge failed and was undone"),
        "{}",
        outcome.detail
    );
    assert_eq!(setup.repo.read("README.md").as_deref(), Some("ours\n"));
    assert!(setup.repo.git(&["status", "--porcelain"]).is_empty());
    assert!(
        notes(&events)
            .iter()
            .any(|n| n.starts_with("main moved since the run started"))
    );
}

#[tokio::test]
async fn untracked_files_in_the_main_tree_do_not_block_the_merge() {
    let setup = Setup::new();
    setup.repo.write("notes.txt", "mine");
    let (outcome, _) = setup
        .run(&hello_agent(), &mut FixedDecider(MergeDecision::Approve))
        .await;
    assert_eq!(outcome.outcome, RunOutcome::Merged, "{}", outcome.detail);
    assert_eq!(setup.repo.read("hello.txt").as_deref(), Some("hi\n"));
    assert_eq!(setup.repo.read("notes.txt").as_deref(), Some("mine"));
}

#[tokio::test]
async fn a_gate_that_commits_to_the_card_branch_blocks_the_merge() {
    let mut setup = Setup::new();
    let sneaky = format!(
        "echo x > sneaky.txt && {ISOLATED_GIT} add sneaky.txt && {ISOLATED_GIT} commit --quiet -m sneaky"
    );
    setup.config.checks.commands = vec![argv(&["/bin/sh", "-c", &sneaky])];
    let (outcome, events) = setup
        .run(&hello_agent(), &mut FixedDecider(MergeDecision::Approve))
        .await;
    assert_eq!(
        outcome.outcome,
        RunOutcome::BranchLeft,
        "{}",
        outcome.detail
    );
    assert!(
        outcome.detail.contains("task branch moved"),
        "{}",
        outcome.detail
    );
    assert_eq!(setup.repo.read("hello.txt"), None);
    assert_eq!(setup.repo.read("sneaky.txt"), None);
    assert!(merge_card(&events).gates[0].passed);
}

#[tokio::test]
async fn a_gate_that_moves_the_target_branch_blocks_the_merge() {
    let mut setup = Setup::new();
    let moved = format!("{ISOLATED_GIT} update-ref refs/heads/main HEAD");
    setup.config.checks.commands = vec![argv(&["/bin/sh", "-c", &moved])];
    let (outcome, _) = setup
        .run(&hello_agent(), &mut FixedDecider(MergeDecision::Approve))
        .await;
    assert_eq!(
        outcome.outcome,
        RunOutcome::BranchLeft,
        "{}",
        outcome.detail
    );
    assert!(
        outcome.detail.contains("main moved while the checks ran"),
        "{}",
        outcome.detail
    );
}

#[tokio::test]
async fn a_rewritten_dot_git_file_stops_the_commit() {
    let setup = Setup::new();
    let script = "echo hi > hello.txt; echo 'gitdir: .fake' > .git";
    let (outcome, events) = setup.run(script, &mut NeverAsked).await;
    assert_eq!(outcome.outcome, RunOutcome::Failed);
    assert!(
        outcome.detail.contains(".git file was changed"),
        "{}",
        outcome.detail
    );
    assert_eq!(outcome.worktree, Some(setup.worktree()));
    assert!(trace(&events).contains(&"commit:failed".to_owned()));
}

#[test]
fn truncation_respects_char_boundaries() {
    assert_eq!(truncate("héllo".into(), 2), ("h".to_owned(), true));
    assert_eq!(truncate("hello".into(), 10), ("hello".to_owned(), false));
}

#[test]
fn small_helpers() {
    assert_eq!(plural(1, "file"), "1 file");
    assert_eq!(plural(3, "file"), "3 files");
    assert_eq!(short("0123456789abcdef"), "0123456");
    assert_eq!(short("abc"), "abc");
    assert_eq!(first_line("\n  first \nsecond"), "first");
    let cap = native_cap(BudgetLimit::Usd(3.0), 1.0).unwrap();
    assert!((cap - 2.2).abs() < 1e-9, "{cap}");
    assert_eq!(native_cap(BudgetLimit::Usd(1.0), 2.0), None);
    assert!(is_protected(".argonauts/config.toml"));
    assert!(is_protected(".mcp.json"));
    assert!(is_protected(".claude"));
    assert!(!is_protected(".claudeignore"));
    assert!(!is_protected("src/.mcp.json"));
    let stat = DiffStat {
        files_changed: 2,
        insertions: 5,
        deletions: 1,
    };
    assert_eq!(describe_stat(&stat), "2 files +5 −1");
    assert_eq!(
        decision_label(&MergeDecision::OpenPr),
        "open a pull request"
    );
}
