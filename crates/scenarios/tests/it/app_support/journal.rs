//! Runs read back from the journal, and a waiting run written by hand for commands that only
//! read journals.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::events::approval::{
    AnswerAction, AnswerOption, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
    FreeText, QuestionPacket,
};
use argonauts_core::events::run::{RunLifecycle, RunMode, RunStarted, WaitReason};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::layout::run_files;
use argonauts_core::names::{ApprovalId, ModelRef};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::{Paths, RepoId, RunId, TaskId};
use argonauts_scenarios::Scenario;
use serde_json::Value;
use time::OffsetDateTime;

pub fn run_events(run_dir: &Path) -> anyhow::Result<Vec<RunEvent>> {
    argonauts_testkit::journal::events(run_dir)
}

/// The resolved settings a run recorded when it started.
pub fn run_settings(run_dir: &Path) -> anyhow::Result<Value> {
    let path = run_dir.join(run_files::SETTINGS);
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub struct WaitingRun {
    pub run: RunId,
    pub dir: PathBuf,
    pub approval: ApprovalId,
}

/// A Captain run that stopped at an agent's question `days_waiting` days ago and has no
/// run host, as a run looks after its run host exited past the wait grace.
pub fn fabricate_waiting_run(scenario: &Scenario, days_waiting: i64) -> anyhow::Result<WaitingRun> {
    let waiting_at = OffsetDateTime::now_utc() - time::Duration::days(days_waiting);
    let run = RunId::at(waiting_at, 0x0a11);
    let repo = RepoId::for_repo(scenario.repo.root()).context("naming the test repo")?;
    let dir = Paths::new(scenario.repo.home()).run_dir(&repo, &run);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let task = TaskId::numbered(1);
    let approval = ApprovalId::numbered(1);
    let worker = ModelRef::new("claude-code", None);

    let started = RunStarted {
        request: "rename the config loader everywhere".to_owned(),
        title: "Rename the config loader".to_owned(),
        mode: RunMode::Captain,
        forced: true,
        workflow: "captain".to_owned(),
        estimate: None,
        captain_or_crew: None,
        conversation_turn: None,
        continues: None,
        repo_root: scenario.repo.root().to_owned(),
        base_branch: Some("main".to_owned()),
        base_commit: scenario.repo.head()?,
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: "0".repeat(64),
        settings: serde_json::json!({}),
        overrides: Vec::new(),
        cli_versions: BTreeMap::new(),
        version: "0.1.0".to_owned(),
        outside_agent: None,
        instructions: Vec::new(),
        required_steps: Vec::new(),
        starts_from: None,
    };
    let opened = ApprovalOpened {
        id: approval.clone(),
        kind: ApprovalKind::Question,
        title: "Question from claude-code · task-001".to_owned(),
        task: Some(task.clone()),
        packet: ApprovalPacket::Question(QuestionPacket {
            from: worker,
            task: task.clone(),
            text: "Keep the old name as an alias?".to_owned(),
            options: vec!["Yes".to_owned(), "No".to_owned()],
        }),
        evidence_sha256: "1".repeat(64),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options: vec![
            AnswerOption {
                key: 'a',
                label: "Yes".to_owned(),
                action: AnswerAction::Choose {
                    value: "Yes".to_owned(),
                },
                recommended: true,
                detail: None,
            },
            AnswerOption {
                key: 'b',
                label: "No".to_owned(),
                action: AnswerAction::Choose {
                    value: "No".to_owned(),
                },
                recommended: false,
                detail: None,
            },
        ],
        free_text: FreeText::Answer,
        needs_you: true,
    };
    let kinds = [
        (
            None,
            RunEventKind::Run(RunLifecycle::Started(Box::new(started))),
        ),
        (
            Some(task.clone()),
            RunEventKind::Approval(ApprovalEvent::Opened(Box::new(opened))),
        ),
        (
            None,
            RunEventKind::Run(RunLifecycle::Waiting {
                reason: WaitReason::Approval,
                waiting_on: vec![approval.clone()],
            }),
        ),
    ];
    let path = dir.join(run_files::EVENTS);
    let mut file =
        fs::File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    for (seq, (task_id, kind)) in kinds.into_iter().enumerate() {
        let event = RunEvent {
            seq: seq as u64,
            ts: waiting_at,
            run_id: run.clone(),
            task_id,
            session: None,
            source: "argonauts-code".to_owned(),
            kind,
        };
        writeln!(file, "{}", serde_json::to_string(&event)?)?;
    }
    Ok(WaitingRun { run, dir, approval })
}

/// Polls the run's journal until an event matches.
pub fn wait_journal(
    run_dir: &Path,
    what: &str,
    matches: impl Fn(&RunEvent) -> bool,
) -> anyhow::Result<RunEvent> {
    super::wait_until(what, || {
        // The run's folder shows up a moment before its journal does.
        if !run_dir.join(run_files::EVENTS).exists() {
            return Ok(None);
        }
        Ok(run_events(run_dir)?
            .into_iter()
            .find(|event| matches(event)))
    })
}

pub fn run_started(run_dir: &Path) -> anyhow::Result<RunStarted> {
    let started = wait_journal(run_dir, "the run to start", |event| {
        matches!(event.kind, RunEventKind::Run(RunLifecycle::Started(_)))
    })?;
    match started.kind {
        RunEventKind::Run(RunLifecycle::Started(started)) => Ok(*started),
        _ => unreachable!("matched a start"),
    }
}

pub fn run_id(run_dir: &Path) -> anyhow::Result<RunId> {
    let name = run_dir
        .file_name()
        .with_context(|| format!("{} has no name", run_dir.display()))?;
    Ok(RunId::new(name.to_string_lossy())?)
}

/// Branches the run made, such as its task branches.
pub fn run_branches(scenario: &Scenario, run: &RunId) -> anyhow::Result<Vec<String>> {
    let listed = scenario.repo.git(&[
        "branch",
        "--list",
        "--format=%(refname:short)",
        &format!("*{run}*"),
    ])?;
    Ok(listed.lines().map(str::to_owned).collect())
}

/// The files a branch holds.
pub fn branch_files(scenario: &Scenario, branch: &str) -> anyhow::Result<Vec<String>> {
    let listed = scenario
        .repo
        .git(&["ls-tree", "-r", "--name-only", branch])?;
    Ok(listed.lines().map(str::to_owned).collect())
}
