//! Runs read back from the journal, and a parked run written by hand for commands that only
//! read journals.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::events::checkpoint::{
    AnswerAction, AnswerOption, CheckpointEvent, CheckpointKind, CheckpointOpened,
    CheckpointPacket, FreeText, QuestionPacket,
};
use argonauts_core::events::run::{ParkReason, RunLifecycle, RunMode, RunStarted};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::layout::run_files;
use argonauts_core::names::{CheckpointId, WorkerRef};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::{CardId, Paths, RepoId, RunId};
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

pub struct ParkedRun {
    pub run: RunId,
    pub dir: PathBuf,
    pub checkpoint: CheckpointId,
}

/// A solo run that parked at an agent's question `days_parked` days ago and has no
/// supervisor, as a run looks after its supervisor exited past the park grace.
pub fn fabricate_parked_run(scenario: &Scenario, days_parked: i64) -> anyhow::Result<ParkedRun> {
    let parked_at = OffsetDateTime::now_utc() - time::Duration::days(days_parked);
    let run = RunId::at(parked_at, 0x0a11);
    let repo = RepoId::for_repo(scenario.repo.root()).context("naming the test repo")?;
    let dir = Paths::new(scenario.repo.home()).run_dir(&repo, &run);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let card = CardId::numbered(1);
    let checkpoint = CheckpointId::numbered(1);
    let owner = WorkerRef::new("claude-code", None);

    let started = RunStarted {
        request: "rename the config loader everywhere".to_owned(),
        title: "Rename the config loader".to_owned(),
        mode: RunMode::Solo,
        forced: true,
        workflow: "solo".to_owned(),
        estimate: None,
        triage: None,
        chat_turn: None,
        continues: None,
        repo_root: scenario.repo.root().to_owned(),
        base_branch: Some("main".to_owned()),
        base_commit: scenario.repo.head()?,
        uncommitted: Vec::new(),
        uncommitted_included: false,
        settings_sha256: "0".repeat(64),
        settings: serde_json::json!({}),
        overrides: Vec::new(),
        harness_versions: BTreeMap::new(),
        version: "0.1.0".to_owned(),
        front_end: None,
        instructions: Vec::new(),
        required_steps: Vec::new(),
    };
    let opened = CheckpointOpened {
        id: checkpoint.clone(),
        kind: CheckpointKind::Question,
        title: "Question from claude-code · task-001".to_owned(),
        card: Some(card.clone()),
        packet: CheckpointPacket::Question(QuestionPacket {
            from: owner,
            card: card.clone(),
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
            Some(card.clone()),
            RunEventKind::Checkpoint(CheckpointEvent::Opened(Box::new(opened))),
        ),
        (
            None,
            RunEventKind::Run(RunLifecycle::Parked {
                reason: ParkReason::Checkpoint,
                waiting_on: vec![checkpoint.clone()],
            }),
        ),
    ];
    let path = dir.join(run_files::EVENTS);
    let mut file =
        fs::File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    for (seq, (card_id, kind)) in kinds.into_iter().enumerate() {
        let event = RunEvent {
            seq: seq as u64,
            ts: parked_at,
            run_id: run.clone(),
            card_id,
            session: None,
            source: "conductor".to_owned(),
            kind,
        };
        writeln!(file, "{}", serde_json::to_string(&event)?)?;
    }
    Ok(ParkedRun {
        run,
        dir,
        checkpoint,
    })
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

/// Branches the run made, such as its card branches.
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
