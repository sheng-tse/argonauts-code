//! A run's agents' sessions through the real controller loop, with fake CLIs: `/resume` lists
//! them under their run with the helpers inside them, and a session carries on only once its
//! run ended, only while its CLI and its branch are still here, with the same agent and its
//! session for every message and CLI command, in the run's worktree for its `!` commands, reviews,
//! and `@` list too, and the worktree made again for it goes once the conversation is done.

use std::path::{Path, PathBuf};

use argonauts_core::agent_sessions::{AgentSession, AgentSessionRef, Resume};
use argonauts_core::api::{RunState, RunSummary};
use argonauts_core::app::{
    AppEvent, AppMode, ConversationEvent, ShellEvent, UiCommand, ViewData, ViewRequest,
};
use argonauts_core::cli_commands::CliCommandSet;
use argonauts_core::event::AgentEvent;
use argonauts_core::events::run::{Party, RunMode};
use argonauts_core::events::session::{
    EndReason, LaunchRecord, PromptVia, SessionEvent, SessionInput, SessionStarted, SubagentKind,
    SubagentStatus, SubagentUpdate,
};
use argonauts_core::events::task::TaskEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::journal::RunLock;
use argonauts_core::layout::run_files;
use argonauts_core::names::{AgentId, ConversationId, ModelRef, RoleName, SessionKey};
use argonauts_core::session_runner::{Access, RunnerCapabilities, RunnerKind, SessionRunner};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::steps::StepName;
use argonauts_core::{RunId, Spend, TaskId};
use argonauts_testkit::runner::session_end;
use argonauts_testkit::{FakeRunner, FakeSession};
use time::OffsetDateTime;

use crate::controller::test_support::{Call, CliOptions, TestCtx, TestDriver};
use crate::conversation_store::{ConversationPlace, ConversationStore};

const RUN: &str = "run-01";
const BRANCH: &str = "argonauts/run-01/task-001";
const WORKER_SESSION: &str = "0a1b2c3d-1111-4aaa-8bbb-000000000001";
const REVIEWER_SESSION: &str = "ses_review01";
const WORKER_SAID: &str = "Fixed both functions.\nAll 6 tests pass.";

fn run() -> RunId {
    RunId::new(RUN).unwrap()
}

/// Where the run's worktree of task-001 was, gone since the run merged.
fn worktree_of(driver: &TestDriver) -> PathBuf {
    driver
        .repo
        .home()
        .join("worktrees/repo")
        .join(RUN)
        .join("task-001")
}

/// A session as a run starts it, with the name the run gives its agent, such as `build:money`.
fn started(worker: &str, role: RoleName, step: StepName, access: Access) -> RunEventKind {
    let worker: ModelRef = worker.parse().unwrap();
    let label = match step {
        StepName::Build => "build:money",
        _ => "review:money",
    };
    RunEventKind::Session(SessionEvent::Started(Box::new(SessionStarted {
        agent_id: AgentId(worker.agent.clone()),
        provider: worker.agent.clone(),
        worker,
        kind: RunnerKind::Cli,
        role,
        step: Some(step),
        access,
        lifetime: SessionLifetime::Task,
        resumed_from: None,
        follow_key: None,
        workflow_step: None,
        label: Some(label.to_owned()),
        asked: None,
    })))
}

fn input(cwd: &Path) -> RunEventKind {
    RunEventKind::Session(SessionEvent::Input(Box::new(SessionInput {
        launch: LaunchRecord {
            program: "cli".to_owned(),
            args: Vec::new(),
            cwd: cwd.to_owned(),
            env_set: Vec::new(),
            env_removed: Vec::new(),
            prompt_via: PromptVia::Stdin,
            sandbox: None,
        },
        parts: Vec::new(),
        queued: Vec::new(),
    })))
}

fn said(text: &str) -> RunEventKind {
    RunEventKind::Agent(AgentEvent::Message {
        text: text.to_owned(),
        parent: None,
    })
}

fn ended(cli_session: &str, last: &str) -> RunEventKind {
    let mut end = session_end(EndReason::Completed, Some(last), None);
    end.cli_session = Some(cli_session.to_owned());
    RunEventKind::Session(SessionEvent::Ended(Box::new(end)))
}

/// A run whose worker worked on task-001 in the run's worktree, starting an explorer subagent, and
/// whose reviewer ran on `reviewer`, both finished. The run stands at `state`, with a
/// run host alive when `live`, and `more` goes on the end of its journal.
fn write_run_as(
    driver: &TestDriver,
    reviewer: &str,
    state: RunState,
    live: bool,
    more: Vec<(Option<SessionKey>, RunEventKind)>,
) {
    let task = TaskId::new("task-001").unwrap();
    let (worker, review) = (SessionKey::numbered(1), SessionKey::numbered(2));
    let worktree = worktree_of(driver);
    let explorer = SubagentUpdate {
        id: "toolu_01explorer".to_owned(),
        parent: None,
        kind: SubagentKind::Subagent,
        label: Some("explorer".to_owned()),
        prompt: Some("Read the parser".to_owned()),
        status: SubagentStatus::Done,
        last_message: Some("The parser reads the sign last.".to_owned()),
        log_uri: None,
        steps_visible: true,
        tokens: None,
    };
    let mut kinds: Vec<(Option<SessionKey>, RunEventKind)> = vec![
        (
            None,
            RunEventKind::Task(TaskEvent::Worktree {
                path: worktree.clone(),
                branch: BRANCH.to_owned(),
                base: "main".to_owned(),
            }),
        ),
        (
            Some(worker.clone()),
            started(
                "claude-code/claude-opus-5-5",
                RoleName::Worker,
                StepName::Build,
                Access::Build,
            ),
        ),
        (Some(worker.clone()), input(&worktree)),
        (
            Some(worker.clone()),
            RunEventKind::Session(SessionEvent::CliSession {
                id: WORKER_SESSION.to_owned(),
                transcript: None,
            }),
        ),
        (
            Some(worker.clone()),
            RunEventKind::Agent(AgentEvent::Subagent { update: explorer }),
        ),
        (Some(worker.clone()), said(WORKER_SAID)),
        (Some(worker), ended(WORKER_SESSION, WORKER_SAID)),
        (
            Some(review.clone()),
            started(
                reviewer,
                RoleName::Reviewer,
                StepName::Review,
                Access::ReadOnly,
            ),
        ),
        (Some(review.clone()), input(&worktree)),
        (Some(review.clone()), said("Looks right.")),
        (Some(review), ended(REVIEWER_SESSION, "Looks right.")),
    ];
    kinds.extend(more);
    let dir = driver.env.run_dir(&run());
    std::fs::create_dir_all(&dir).unwrap();
    let mut log = String::new();
    for (seq, (session, kind)) in kinds.into_iter().enumerate() {
        let event = RunEvent {
            seq: seq as u64,
            ts: OffsetDateTime::now_utc(),
            run_id: run(),
            task_id: Some(task.clone()),
            session,
            source: "argonauts-code".to_owned(),
            kind,
        };
        log.push_str(&serde_json::to_string(&event).unwrap());
        log.push('\n');
    }
    std::fs::write(dir.join(run_files::EVENTS), log).unwrap();
    set_run(driver, state, live);
}

fn set_run(driver: &TestDriver, state: RunState, live: bool) {
    driver.conductor.set_runs(vec![summary(run(), state, live)]);
}

fn summary(run: RunId, state: RunState, live: bool) -> RunSummary {
    RunSummary {
        run,
        title: "Fix the money parser".to_owned(),
        mode: RunMode::Crew,
        workflow: "crew".to_owned(),
        state,
        stopped_at: "merged".to_owned(),
        updated: OffsetDateTime::now_utc(),
        spent: Spend::default(),
        sessions_to_resume: 0,
        waiting: Vec::new(),
        live,
        remind: false,
        name: None,
    }
}

/// A merged run whose reviewer ran on OpenCode.
fn write_run(driver: &TestDriver) {
    write_run_as(driver, "opencode", RunState::Merged, false, Vec::new());
}

/// OpenCode, set up but unable to resume a session.
fn opencode_that_cant_resume(driver: &TestDriver) {
    let fake = FakeRunner::cli("opencode", "opencode");
    let capabilities = RunnerCapabilities {
        resume: false,
        ..fake.capabilities()
    };
    driver.directory.add(fake.with_capabilities(capabilities));
}

async fn start() -> TestDriver {
    let driver = TestDriver::start().await;
    write_run(&driver);
    opencode_that_cant_resume(&driver);
    driver
}

/// The task's branch, as the merged run left it, with a file its worker added on it.
fn task_branch(driver: &TestDriver) {
    let repo = &driver.repo;
    repo.git(&["checkout", "-q", "-b", BRANCH]).unwrap();
    repo.write("only-on-task.txt", "added on the task\n")
        .unwrap();
    repo.commit_all("add a file on the task").unwrap();
    repo.git(&["checkout", "-q", "main"]).unwrap();
}

fn resume(session: &str) -> UiCommand {
    UiCommand::Slash {
        command: SlashCommand::Resume {
            target: Some(ResumeTarget::Agent(
                AgentSessionRef::parse(&format!("{RUN}/{session}")).unwrap(),
            )),
        },
    }
}

fn resume_conversation(n: u32) -> UiCommand {
    UiCommand::Slash {
        command: SlashCommand::Resume {
            target: Some(ResumeTarget::Conversation(ConversationId::numbered(n))),
        },
    }
}

fn slash(command: SlashCommand) -> UiCommand {
    UiCommand::Slash { command }
}

async fn notice(driver: &mut TestDriver, part: &str) -> String {
    match driver
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains(part)))
        .await
    {
        AppEvent::Notice { text, .. } => text,
        _ => unreachable!(),
    }
}

async fn listed(driver: &mut TestDriver) -> (Vec<AgentSession>, Vec<RunId>) {
    driver
        .send(UiCommand::Slash {
            command: SlashCommand::Resume { target: None },
        })
        .await;
    let AppEvent::View { data } = driver
        .until(|event| {
            matches!(event, AppEvent::View { data } if matches!(data.as_ref(), ViewData::Runs { .. }))
        })
        .await
    else {
        unreachable!()
    };
    let ViewData::Runs {
        agents,
        agents_read,
        ..
    } = *data
    else {
        unreachable!()
    };
    (agents, agents_read)
}

fn worker_of(agents: &[AgentSession]) -> &AgentSession {
    agents
        .iter()
        .find(|agent| agent.session == SessionKey::numbered(1))
        .expect("the worker's session")
}

/// Carries the worker's session on and waits until it's on screen.
async fn carry_on_worker(driver: &mut TestDriver) -> String {
    driver.send(resume("s-001")).await;
    notice(driver, "Carrying on").await
}

/// Sends `command` and waits for its turn to end and the app to settle.
async fn turn(driver: &mut TestDriver, command: UiCommand) {
    driver.send(command).await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::TurnFinished { .. }
                        | ConversationEvent::TurnFailed { .. }
                }
            )
        })
        .await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Mode {
                    mode: AppMode::Idle
                }
            )
        })
        .await;
}

fn said_text(text: &str) -> UiCommand {
    UiCommand::Submit {
        text: text.to_owned(),
        workflow_requested: false,
        pictures: Vec::new(),
    }
}

fn worker_turn(text: &str) -> FakeSession {
    FakeSession::completed(text).with_cli_session(WORKER_SESSION)
}

#[tokio::test]
async fn resume_lists_each_agents_session_under_its_run() {
    let mut driver = start().await;
    driver.repo.git(&["branch", BRANCH]).unwrap();
    let (agents, read) = listed(&mut driver).await;
    assert_eq!(read, [run()]);
    let ids: Vec<String> = agents
        .iter()
        .map(|agent| agent.reference().to_string())
        .collect();
    assert_eq!(ids, ["run-01/s-001", "run-01/s-002"]);

    let worker = &agents[0];
    assert_eq!(worker.role, RoleName::Worker);
    // Its name in the run, as the conversation and the panel call it, not its task.
    assert_eq!(worker.label.as_deref(), Some("build:money"));
    assert_eq!(worker.worker.to_string(), "claude-code/claude-opus-5-5");
    assert_eq!(
        worker.last_message.as_deref(),
        Some("Fixed both functions. All 6 tests pass.")
    );
    // Its worktree is gone since the merge, but its branch makes it again.
    assert_eq!(worker.resume, Resume::Ready);
    // The explorer is counted inside the worker's session, never a session of its own.
    assert_eq!(worker.helpers.len(), 1);
    assert_eq!(worker.helpers[0].label.as_deref(), Some("explorer"));
    assert_eq!(worker.helper_counts(), "1 subagent");

    let Resume::Cannot { why } = &agents[1].resume else {
        panic!("{:?}", agents[1].resume)
    };
    assert_eq!(why, "OpenCode can't resume a session");

    // An older run's agents come when the picker opens it.
    driver
        .send(UiCommand::Request {
            view: ViewRequest::AgentSessions { run: run() },
        })
        .await;
    let AppEvent::View { data } = driver
        .until(|event| {
            matches!(event, AppEvent::View { data }
                if matches!(data.as_ref(), ViewData::AgentSessions { .. }))
        })
        .await
    else {
        unreachable!()
    };
    let ViewData::AgentSessions { run: of, agents } = *data else {
        unreachable!()
    };
    assert_eq!(of, run());
    assert_eq!(agents.len(), 2);
    driver.exit().await;
}

/// Until its run ended, the run owns a finished agent's session, since its next round on the
/// task resumes that same CLI session: nothing carries it on, and no worktree is made for it.
#[tokio::test]
async fn a_session_whose_run_hasnt_ended_stays_the_runs() {
    let mut driver = TestDriver::start().await;
    opencode_that_cant_resume(&driver);
    driver.repo.git(&["branch", BRANCH]).unwrap();
    let working = SessionKey::numbered(3);
    let fixer = vec![
        (
            Some(working.clone()),
            started(
                "claude-code/claude-opus-5-5",
                RoleName::Worker,
                StepName::Build,
                Access::Build,
            ),
        ),
        (Some(working), input(&worktree_of(&driver))),
    ];
    write_run_as(&driver, "opencode", RunState::Running, true, fixer);
    let (agents, _) = listed(&mut driver).await;
    assert_eq!(worker_of(&agents).resume, Resume::RunOwns);
    assert_eq!(agents[2].resume, Resume::Working);

    for (state, live) in [
        (RunState::Waiting, false),
        (RunState::Crashed, false),
        (RunState::Pausing, true),
        // A run host still finishing the run holds it until it lets go.
        (RunState::Merged, true),
    ] {
        set_run(&driver, state, live);
        let (agents, _) = listed(&mut driver).await;
        assert_eq!(worker_of(&agents).resume, Resume::RunOwns, "{state:?}");
    }

    set_run(&driver, RunState::Waiting, false);
    driver.send(resume("s-001")).await;
    let said = notice(&mut driver, "still owns").await;
    assert_eq!(
        said,
        "run-01 hasn't ended, so it still owns run-01/s-001. t in its agent view takes it over, \
         or it carries on here once the run ends."
    );
    assert!(!worktree_of(&driver).exists());
    assert!(
        !driver
            .seen()
            .iter()
            .any(|event| matches!(event, AppEvent::ConversationResumed { .. }))
    );
    driver.exit().await;
}

#[tokio::test]
async fn a_finished_worker_carries_on_in_its_worktree_made_again_after_the_merge() {
    let mut driver = start().await;
    driver.repo.git(&["branch", BRANCH]).unwrap();
    let worktree = worktree_of(&driver);
    assert!(!worktree.exists());
    driver.agent.push(worker_turn("Added a test."));

    driver.send(resume("s-001")).await;
    let AppEvent::ConversationResumed {
        conversation,
        worker,
        turns,
        ..
    } = driver
        .until(|event| matches!(event, AppEvent::ConversationResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(conversation, ConversationId::numbered(1));
    assert_eq!(worker, Some("claude-code/claude-opus-5-5".parse().unwrap()));
    assert_eq!(turns.len(), 1);
    assert!(matches!(turns[0].from, Party::Agent { .. }));
    assert_eq!(turns[0].text, WORKER_SAID);
    let said = notice(&mut driver, "Carrying on").await;
    assert!(
        said.starts_with("Carrying on build:money in run-01"),
        "{said}"
    );
    assert!(said.contains("through claude's own resume"), "{said}");
    assert!(
        said.contains(&format!(
            "It works on the run's worktree in {} on {BRANCH}, not your checkout.",
            driver.env.display(&worktree)
        )),
        "{said}"
    );
    assert!(said.contains("made again from the branch"), "{said}");
    // The worktree is back where the session ran, on the task's branch.
    let head = driver
        .repo
        .git(&["-C", worktree.to_str().unwrap(), "branch", "--show-current"])
        .unwrap();
    assert_eq!(head.trim(), BRANCH);

    turn(&mut driver, said_text("add a test for -0.5")).await;
    let spec = &driver.agent.specs()[0];
    // A direct turn of the worker's own session, through its CLI's resume, in the run's worktree.
    assert_eq!(spec.resume.as_deref(), Some(WORKER_SESSION));
    assert_eq!(spec.workdir, worktree);
    assert_eq!(spec.access, Access::Direct);
    assert_eq!(spec.model.model.as_deref(), Some("claude-opus-5-5"));
    // A restart carries it on in the same folder.
    let store =
        ConversationStore::open(&driver.env.conversation_dir(), &ConversationId::numbered(1));
    let place = store.place().unwrap();
    assert_eq!(place.folder, worktree);
    assert!(place.made_again);
    assert_eq!(store.session().unwrap().cli_session, WORKER_SESSION);

    // Done with it, the worktree made again goes, and the branch is free again.
    driver.send(slash(SlashCommand::Clear)).await;
    let said = notice(&mut driver, "Removed the worktree").await;
    assert_eq!(
        said,
        format!(
            "Removed the worktree made again for run-01/s-001 in {}.",
            driver.env.display(&worktree)
        )
    );
    assert!(!worktree.exists());
    assert!(driver.repo.git(&["branch", "-f", BRANCH, "main"]).is_ok());

    // Coming back makes it again.
    driver.send(resume_conversation(1)).await;
    let said = notice(&mut driver, "It works on the run's worktree").await;
    assert!(
        said.ends_with("so it was made again from the branch."),
        "{said}"
    );
    assert!(worktree.is_dir());
    driver.exit().await;
}

/// Work left uncommitted keeps the worktree until it's committed, and the agent's commits are
/// named with how to bring them in.
#[tokio::test]
async fn a_worktree_made_again_stays_while_it_holds_uncommitted_work() {
    let mut driver = start().await;
    driver.repo.git(&["branch", BRANCH]).unwrap();
    carry_on_worker(&mut driver).await;
    driver.agent.push(worker_turn("Noted."));
    turn(&mut driver, said_text("take notes")).await;
    let worktree = worktree_of(&driver);
    std::fs::write(worktree.join("notes.txt"), "draft\n").unwrap();
    driver.send(slash(SlashCommand::Clear)).await;
    let said = notice(&mut driver, "not committed yet").await;
    assert!(said.contains("so it stays"), "{said}");
    assert!(worktree.is_dir());

    let path = worktree.to_str().unwrap();
    driver.repo.git(&["-C", path, "add", "notes.txt"]).unwrap();
    driver
        .repo
        .git(&["-C", path, "commit", "-qm", "notes"])
        .unwrap();
    driver.send(resume_conversation(1)).await;
    notice(&mut driver, "It works on the run's worktree").await;
    driver.send(slash(SlashCommand::Clear)).await;
    let said = notice(&mut driver, "Removed the worktree").await;
    assert!(
        said.ends_with(&format!(
            "Its branch {BRANCH} keeps 1 commit your checkout doesn't have, and git merge \
             {BRANCH} brings it in."
        )),
        "{said}"
    );
    assert!(!worktree.exists());
    driver.exit().await;
}

/// At start, the worktrees made again for earlier conversations go once nothing in them is
/// uncommitted, but never the one of the conversation on screen.
#[tokio::test]
async fn worktrees_made_again_for_earlier_conversations_go_at_start() {
    let (parts, _) = TestCtx::new(CliOptions::default());
    let repo = &parts.repo;
    let ctx = &parts.ctx;
    let base = repo.home().join("worktrees/repo/run-01");
    let (clean, dirty) = (base.join("task-001"), base.join("task-002"));
    for (n, folder) in [(1, &clean), (2, &dirty)] {
        let branch = format!("task-00{n}");
        repo.git(&["branch", &branch]).unwrap();
        repo.git(&["worktree", "add", folder.to_str().unwrap(), &branch])
            .unwrap();
        let store =
            ConversationStore::open(&ctx.env.conversation_dir(), &ConversationId::numbered(n));
        store
            .save_place(&ConversationPlace {
                agent: AgentSessionRef::parse(&format!("run-01/s-00{n}")).unwrap(),
                worker: "claude-code".parse().unwrap(),
                folder: folder.clone(),
                branch: Some(branch),
                made_again: true,
            })
            .unwrap();
    }
    std::fs::write(dirty.join("notes.txt"), "draft\n").unwrap();
    let conversation = super::ConversationState::new(ctx).unwrap();
    super::tidy_worktrees(ctx, &conversation);
    assert!(!clean.exists());
    assert!(dirty.is_dir());
}

#[tokio::test]
async fn a_session_its_cli_cant_resume_shows_its_transcript() {
    let mut driver = start().await;
    driver.send(resume("s-002")).await;
    let said = notice(&mut driver, "can't carry on").await;
    assert_eq!(
        said,
        "run-01/s-002 can't carry on, since OpenCode can't resume a session. Here is its \
         transcript."
    );
    let asked = driver.conductor.calls().into_iter().any(|call| {
        matches!(call, Call::Raw { run, session } if run.as_str() == RUN && session.as_str() == "s-002")
    });
    assert!(asked, "{:?}", driver.conductor.calls());
    assert!(
        !driver
            .seen()
            .iter()
            .any(|event| matches!(event, AppEvent::ConversationResumed { .. }))
    );
    driver.exit().await;
}

/// "Can continue" holds only while the worktree or its branch is there and the CLI is installed,
/// and nothing is made again until both hold.
#[tokio::test]
async fn a_session_whose_branch_or_cli_is_gone_cant_carry_on() {
    let mut driver = start().await;
    let (agents, _) = listed(&mut driver).await;
    assert_eq!(
        worker_of(&agents).resume,
        Resume::Cannot {
            why: format!("its worktree is gone, and so is its branch {BRANCH}")
        }
    );
    driver.send(resume("s-001")).await;
    let said = notice(&mut driver, "can't carry on").await;
    assert!(said.contains("so is its branch"), "{said}");
    assert!(!worktree_of(&driver).exists());

    driver.repo.git(&["branch", BRANCH]).unwrap();
    driver.directory.uninstall("claude-code");
    let (agents, _) = listed(&mut driver).await;
    assert_eq!(
        worker_of(&agents).resume,
        Resume::Cannot {
            why: "claude isn't installed".to_owned()
        }
    );
    driver.send(resume("s-001")).await;
    let said = notice(&mut driver, "isn't installed").await;
    assert_eq!(
        said,
        "run-01/s-001 can't carry on, since claude isn't installed. Here is its transcript."
    );
    assert!(!worktree_of(&driver).exists());
    driver.exit().await;
}

/// The run may go again after the session carried on, as a stopped run you resume does. Then
/// it owns the session again: the conversation waits, and coming back to it says why.
#[tokio::test]
async fn a_carried_on_conversation_waits_while_its_run_goes_again() {
    let mut driver = start().await;
    driver.repo.git(&["branch", BRANCH]).unwrap();
    carry_on_worker(&mut driver).await;
    driver.agent.push(worker_turn("Added a test."));
    turn(&mut driver, said_text("add a test")).await;
    let held = RunLock::try_acquire(&driver.env.run_dir(&run()))
        .unwrap()
        .expect("the run's lock");
    driver.submit("go on").await;
    let said = notice(&mut driver, "is going again").await;
    assert_eq!(
        said,
        "run-01 is going again, so it owns run-01/s-001 until it ends, and this conversation \
         waits. t in its agent view takes it over."
    );
    assert_eq!(
        driver.agent.specs().len(),
        1,
        "no second process on the session"
    );
    drop(held);

    set_run(&driver, RunState::Running, false);
    driver.send(slash(SlashCommand::Clear)).await;
    driver.send(resume_conversation(1)).await;
    let said = notice(&mut driver, "conversation-001 can't carry on").await;
    assert!(
        said.starts_with("conversation-001 can't carry on: run-01 hasn't ended, so it still owns"),
        "{said}"
    );
    driver.exit().await;
}

/// `/branch` keeps working where the original does, with the same agent, on a session of its
/// own, and says so; `/fork` leaves a copy that does the same.
#[tokio::test]
async fn a_branch_works_with_the_same_agent_in_the_same_worktree() {
    let mut driver = start().await;
    driver.repo.git(&["branch", BRANCH]).unwrap();
    carry_on_worker(&mut driver).await;
    driver.agent.push(worker_turn("Added a test."));
    turn(&mut driver, said_text("add a test for -0.5")).await;
    let worktree = worktree_of(&driver);

    driver
        .send(slash(SlashCommand::Branch { name: None }))
        .await;
    let said = notice(&mut driver, "Branched into").await;
    assert!(
        said.contains(&format!(
            " on Claude Code in the run's worktree in {} on {BRANCH}, not your checkout, on a \
             session of its own.",
            driver.env.display(&worktree)
        )),
        "{said}"
    );
    driver
        .agent
        .push(FakeSession::completed("Both pass.").with_cli_session("branch-session"));
    turn(&mut driver, said_text("run the tests")).await;
    let spec = &driver.agent.specs()[1];
    assert_eq!(spec.resume, None, "two conversations never share a session");
    assert_eq!(spec.workdir, worktree);
    assert_eq!(spec.access, Access::Direct);
    assert_eq!(spec.model.to_string(), "claude-code/claude-opus-5-5");
    let primer: String = spec.prompt.iter().map(|part| part.text.as_str()).collect();
    assert!(primer.contains("add a test for -0.5"), "{primer}");
    let branched =
        ConversationStore::open(&driver.env.conversation_dir(), &ConversationId::numbered(2));
    assert_eq!(branched.place().unwrap().folder, worktree);
    assert_eq!(branched.session().unwrap().cli_session, "branch-session");

    driver
        .send(slash(SlashCommand::Fork { prompt: None }))
        .await;
    let said = notice(&mut driver, "Forked this conversation").await;
    assert!(said.contains("in the run's worktree"), "{said}");
    let forked =
        ConversationStore::open(&driver.env.conversation_dir(), &ConversationId::numbered(3));
    assert_eq!(forked.place().unwrap().folder, worktree);
    assert!(forked.session().is_none());
    driver.exit().await;
}

/// `!` commands, reviews, and the `@` list work where the conversation does: the run's worktree,
/// not your checkout, and what the agent hears of a command names that folder.
#[tokio::test]
async fn shell_reviews_and_files_work_in_the_worktree() {
    // No reviewer of another provider is set up, so the main agent reviews.
    let mut driver = TestDriver::start().await;
    write_run(&driver);
    task_branch(&driver);
    carry_on_worker(&mut driver).await;
    let worktree = worktree_of(&driver);
    driver
        .until(|event| {
            matches!(event, AppEvent::RepoFiles { paths }
                if paths.iter().any(|path| path == "only-on-task.txt"))
        })
        .await;

    driver
        .send(UiCommand::Shell {
            command: "pwd".to_owned(),
        })
        .await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Shell {
                    event: ShellEvent::Finished { .. }
                }
            )
        })
        .await;
    let printed: String = driver
        .seen()
        .iter()
        .filter_map(|event| match event {
            AppEvent::Shell {
                event: ShellEvent::Output { text, .. },
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        Path::new(printed.trim()).canonicalize().unwrap(),
        worktree.canonicalize().unwrap()
    );
    driver.agent.push(worker_turn("It's the worktree."));
    turn(&mut driver, said_text("where did that run?")).await;
    let heard = driver.agent.specs()[0]
        .prompt
        .iter()
        .find(|part| part.label == "shell commands")
        .map(|part| part.text.clone())
        .expect("the command reaches the agent");
    assert!(
        heard.starts_with(&format!(
            "Before this message I ran these commands myself, in {}.",
            worktree.display()
        )),
        "{heard}"
    );

    std::fs::write(
        worktree.join("only-on-task.txt"),
        "changed in the worktree\n",
    )
    .unwrap();
    driver
        .agent
        .push(FakeSession::structured(serde_json::json!({
            "kind": "answer", "answer_markdown": "Looks fine.", "estimate": null
        })));
    turn(&mut driver, slash(SlashCommand::Review { target: None })).await;
    let review: String = driver.agent.specs()[1]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect();
    assert!(review.contains("only-on-task.txt"), "{review}");
    assert_eq!(driver.agent.specs()[1].workdir, worktree);
    driver.exit().await;
}

/// The CLI's own commands go to the agent whose session the conversation carries on, through
/// that session, and a new main agent leaves the conversation with it.
#[tokio::test]
async fn cli_commands_and_a_new_main_agent_keep_the_carried_on_session() {
    let mut driver = start().await;
    driver.repo.git(&["branch", BRANCH]).unwrap();
    driver.directory.add(FakeRunner::cli("codex", "codex"));
    carry_on_worker(&mut driver).await;
    let worktree = worktree_of(&driver);

    driver.agent.push(worker_turn("Wrote CLAUDE.md."));
    turn(
        &mut driver,
        slash(SlashCommand::Cli {
            name: "init".to_owned(),
            args: String::new(),
        }),
    )
    .await;
    let spec = &driver.agent.specs()[0];
    assert_eq!(spec.resume.as_deref(), Some(WORKER_SESSION));
    assert_eq!(spec.workdir, worktree);
    assert_eq!(spec.access, Access::Direct);
    assert_eq!(spec.model.model.as_deref(), Some("claude-opus-5-5"));

    driver
        .send(UiCommand::SetSetting {
            key: "crew.main".to_owned(),
            value: "\"codex\"".to_owned(),
            scope: SettingScope::Session,
        })
        .await;
    let said = notice(&mut driver, "stays with that agent").await;
    assert!(
        said.starts_with("This conversation carries on run-01/s-001 with ")
            && said
                .contains(" on Claude Code, so it stays with that agent. /clear starts one with ")
            && said.ends_with("Codex."),
        "{said}"
    );
    driver.agent.push(worker_turn("Done."));
    turn(&mut driver, said_text("go on")).await;
    let spec = &driver.agent.specs()[1];
    assert_eq!(spec.resume.as_deref(), Some(WORKER_SESSION));
    assert_eq!(spec.workdir, worktree);
    let store =
        ConversationStore::open(&driver.env.conversation_dir(), &ConversationId::numbered(1));
    assert_eq!(store.session().unwrap().cli_session, WORKER_SESSION);
    driver.exit().await;
}

/// The `/` menu and the keywords the input draws are those of the CLI the conversation goes
/// to: Codex's while it carries on a Codex reviewer, though the main agent is Claude Code.
#[tokio::test]
async fn the_menu_and_keywords_follow_the_carried_on_cli() {
    let mut driver = TestDriver::start().await;
    driver.directory.add(FakeRunner::cli("codex", "codex"));
    write_run_as(&driver, "codex", RunState::Merged, false, Vec::new());
    driver.repo.git(&["branch", BRANCH]).unwrap();
    let main = agent_set(&mut driver).await;
    assert_eq!(main.agent_id.0, "claude-code");
    assert!(!main.keywords.is_empty(), "Claude Code draws ultrathink");

    driver.send(resume("s-002")).await;
    let reviewer = agent_set(&mut driver).await;
    assert_eq!(reviewer.agent_id.0, "codex");
    assert!(reviewer.keywords.is_empty(), "{:?}", reviewer.keywords);

    driver.send(slash(SlashCommand::Clear)).await;
    let back = agent_set(&mut driver).await;
    assert_eq!(back.agent_id.0, "claude-code");
    driver.exit().await;
}

async fn agent_set(driver: &mut TestDriver) -> CliCommandSet {
    let AppEvent::CliCommands { set } = driver
        .until(|event| matches!(event, AppEvent::CliCommands { .. }))
        .await
    else {
        unreachable!()
    };
    set
}

/// A carried-on session's turn works in the run's worktree, so `/rewind` puts back what it changed
/// there, and the ignored file its tool calls wrote is named as one nothing kept, while your
/// checkout keeps its own files.
#[tokio::test]
async fn rewind_puts_back_a_carried_on_turns_files_in_the_worktree() {
    use argonauts_core::app::{AnswerTarget, ChoiceKind};
    use argonauts_core::events::approval::AnswerAction;
    use argonauts_core::session_runner::{RunnerEvent, SessionSpec};
    use serde_json::json;

    let mut driver = start().await;
    let repo = &driver.repo;
    repo.git(&["checkout", "-q", "-b", BRANCH]).unwrap();
    repo.write(".gitignore", ".env\n").unwrap();
    repo.write("money.py", "sign = 1\n").unwrap();
    repo.commit_all("the task's work").unwrap();
    repo.git(&["checkout", "-q", "main"]).unwrap();
    let checkout = repo.root().to_owned();
    let worktree = worktree_of(&driver);
    carry_on_worker(&mut driver).await;
    assert_eq!(
        std::fs::read_to_string(worktree.join("money.py")).unwrap(),
        "sign = 1\n"
    );

    driver.agent.respond(|spec: &SessionSpec| {
        let root = &spec.workdir;
        std::fs::write(root.join("money.py"), "sign = -1\n").unwrap();
        std::fs::write(root.join(".env"), "TOKEN=from-the-agent\n").unwrap();
        let tool = |id: &str, name: &str, path: &Path| {
            RunnerEvent::Agent(AgentEvent::ToolStarted {
                id: id.into(),
                name: name.into(),
                input: json!({"file_path": path.to_string_lossy()}),
                parent: None,
            })
        };
        worker_turn("Flipped the sign.")
            .with_event(tool("t1", "Edit", &root.join("money.py")))
            .with_event(tool("t2", "Write", &root.join(".env")))
    });
    turn(&mut driver, said_text("flip the sign")).await;
    assert_eq!(driver.agent.specs()[0].workdir, worktree);

    driver
        .send(slash(SlashCommand::Rewind { task: None }))
        .await;
    let AppEvent::Choice { choice } = driver
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::RewindTurn))
        .await
    else {
        unreachable!()
    };
    let AnswerAction::Choose { value } = &choice.options[0].action else {
        panic!("{:?}", choice.options[0])
    };
    driver
        .send(UiCommand::Answer {
            to: AnswerTarget::Choice { id: choice.id },
            action: AnswerAction::Choose {
                value: value.clone(),
            },
            text: None,
            evidence_sha256: None,
            tasks: Vec::new(),
            remember: false,
        })
        .await;
    let AppEvent::ChoiceClosed { outcome, .. } = driver
        .until(|event| matches!(event, AppEvent::ChoiceClosed { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(
        outcome,
        "Put back money.py · can't put back, as nothing kept how they were: .env"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("money.py")).unwrap(),
        "sign = 1\n"
    );
    assert!(
        !checkout.join("money.py").exists(),
        "your checkout is untouched"
    );
    assert!(
        !checkout.join(".env").exists(),
        "your checkout is untouched"
    );
    driver.exit().await;
}

/// `/plan` in a carried-on session plans first in that session, read-only in the run's worktree,
/// and changes nothing until you go ahead, as plan first does in direct Captain.
#[tokio::test]
async fn plan_in_a_carried_on_session_changes_nothing_until_you_go_ahead() {
    use argonauts_core::app::{AnswerTarget, ChoiceKind};
    use argonauts_core::events::approval::AnswerAction;
    use argonauts_core::session_runner::SessionSpec;

    let mut driver = start().await;
    task_branch(&driver);
    let worktree = worktree_of(&driver);
    carry_on_worker(&mut driver).await;
    driver.agent.respond(|spec: &SessionSpec| {
        if spec.access == Access::Direct {
            std::fs::write(spec.workdir.join("planned.txt"), "done\n").unwrap();
            worker_turn("Added planned.txt.")
        } else {
            worker_turn("1. Add planned.txt.")
        }
    });
    driver
        .send(slash(SlashCommand::Plan {
            task: Some("add planned.txt".into()),
        }))
        .await;
    let AppEvent::Choice { choice } = driver
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::GoAhead))
        .await
    else {
        unreachable!()
    };
    assert_eq!(
        choice.body,
        ["Claude Code carries it out in the run's worktree.".to_owned()]
    );
    let specs = driver.agent.specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].access, Access::ReadOnly);
    assert_eq!(specs[0].workdir, worktree);
    assert_eq!(specs[0].resume.as_deref(), Some(WORKER_SESSION));
    assert!(!worktree.join("planned.txt").exists());

    driver
        .send(UiCommand::Answer {
            to: AnswerTarget::Choice { id: choice.id },
            action: AnswerAction::Choose {
                value: "yes".to_owned(),
            },
            text: None,
            evidence_sha256: None,
            tasks: Vec::new(),
            remember: false,
        })
        .await;
    driver
        .until(|event| {
            matches!(
                event,
                AppEvent::Conversation {
                    event: ConversationEvent::TurnFinished { .. }
                }
            )
        })
        .await;
    let specs = driver.agent.specs();
    assert_eq!(specs[1].access, Access::Direct);
    assert_eq!(specs[1].workdir, worktree);
    assert!(worktree.join("planned.txt").exists());
    assert!(!driver.repo.root().join("planned.txt").exists());
    driver.exit().await;
}
