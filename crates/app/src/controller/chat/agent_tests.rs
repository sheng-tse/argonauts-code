//! A run's agents' sessions through the real controller loop, with fake CLIs: `/resume` lists
//! them under their run with the helpers inside them, and a session carries on only once its
//! run ended, only while its CLI and its branch are still here, with the same agent and its
//! session for every message and CLI command, in the run's copy for its `!` commands, reviews,
//! and `@` list too, and the copy made again for it goes once the conversation is done.

use std::path::{Path, PathBuf};

use argonauts_core::agent_sessions::{AgentSession, AgentSessionRef, Resume};
use argonauts_core::api::{RunState, RunSummary};
use argonauts_core::app::{
    AppEvent, AppMode, ChatEvent, ShellEvent, UiCommand, ViewData, ViewRequest,
};
use argonauts_core::event::AgentEvent;
use argonauts_core::events::card::CardEvent;
use argonauts_core::events::run::{Party, RunMode};
use argonauts_core::events::session::{
    EndReason, LaunchRecord, PromptVia, SessionEvent, SessionInput, SessionStarted, SubagentKind,
    SubagentStatus, SubagentUpdate,
};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::journal::RunLock;
use argonauts_core::layout::run_files;
use argonauts_core::names::{ChatId, RoleName, SessionKey, VendorId, WorkerRef};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::sessions::SessionLifetime;
use argonauts_core::steps::StepName;
use argonauts_core::vendor_commands::VendorCommandSet;
use argonauts_core::worker::{Access, Worker, WorkerCapabilities, WorkerKind};
use argonauts_core::{CardId, RunId, Spend};
use argonauts_testkit::worker::session_end;
use argonauts_testkit::{FakeSession, FakeWorker};
use time::OffsetDateTime;

use crate::chat_store::{ChatPlace, ChatStore};
use crate::controller::test_support::{Call, Harness, HarnessOptions, TestCtx};

const RUN: &str = "run-01";
const BRANCH: &str = "argonauts/run-01/task-001";
const OWNER_SESSION: &str = "0a1b2c3d-1111-4aaa-8bbb-000000000001";
const REVIEWER_SESSION: &str = "ses_review01";
const OWNER_SAID: &str = "Fixed both functions.\nAll 6 tests pass.";

fn run() -> RunId {
    RunId::new(RUN).unwrap()
}

/// Where the run's copy of card-001 was, gone since the run merged.
fn copy_of(harness: &Harness) -> PathBuf {
    harness
        .repo
        .home()
        .join("worktrees/repo")
        .join(RUN)
        .join("task-001")
}

/// A session as a run starts it, with the name the run gives its agent, such as `build:money`.
fn started(worker: &str, role: RoleName, step: StepName, access: Access) -> RunEventKind {
    let worker: WorkerRef = worker.parse().unwrap();
    let label = match step {
        StepName::Build => "build:money",
        _ => "review:money",
    };
    RunEventKind::Session(SessionEvent::Started(Box::new(SessionStarted {
        vendor: VendorId(worker.worker.clone()),
        family: worker.worker.clone(),
        worker,
        kind: WorkerKind::Harness,
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

fn ended(vendor_session: &str, last: &str) -> RunEventKind {
    let mut end = session_end(EndReason::Completed, Some(last), None);
    end.vendor_session = Some(vendor_session.to_owned());
    RunEventKind::Session(SessionEvent::Ended(Box::new(end)))
}

/// A run whose owner worked on card-001 in the run's copy, starting an explorer subagent, and
/// whose reviewer ran on `reviewer`, both finished. The run stands at `state`, with a
/// supervisor alive when `live`, and `more` goes on the end of its journal.
fn write_run_as(
    harness: &Harness,
    reviewer: &str,
    state: RunState,
    live: bool,
    more: Vec<(Option<SessionKey>, RunEventKind)>,
) {
    let card = CardId::new("task-001").unwrap();
    let (owner, review) = (SessionKey::numbered(1), SessionKey::numbered(2));
    let copy = copy_of(harness);
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
            RunEventKind::Card(CardEvent::Worktree {
                path: copy.clone(),
                branch: BRANCH.to_owned(),
                base: "main".to_owned(),
            }),
        ),
        (
            Some(owner.clone()),
            started(
                "claude-code/claude-opus-5-5",
                RoleName::Worker,
                StepName::Build,
                Access::Build,
            ),
        ),
        (Some(owner.clone()), input(&copy)),
        (
            Some(owner.clone()),
            RunEventKind::Session(SessionEvent::VendorSession {
                id: OWNER_SESSION.to_owned(),
                transcript: None,
            }),
        ),
        (
            Some(owner.clone()),
            RunEventKind::Agent(AgentEvent::Subagent { update: explorer }),
        ),
        (Some(owner.clone()), said(OWNER_SAID)),
        (Some(owner), ended(OWNER_SESSION, OWNER_SAID)),
        (
            Some(review.clone()),
            started(
                reviewer,
                RoleName::Reviewer,
                StepName::Review,
                Access::ReadOnly,
            ),
        ),
        (Some(review.clone()), input(&copy)),
        (Some(review.clone()), said("Looks right.")),
        (Some(review), ended(REVIEWER_SESSION, "Looks right.")),
    ];
    kinds.extend(more);
    let dir = harness.env.run_dir(&run());
    std::fs::create_dir_all(&dir).unwrap();
    let mut log = String::new();
    for (seq, (session, kind)) in kinds.into_iter().enumerate() {
        let event = RunEvent {
            seq: seq as u64,
            ts: OffsetDateTime::now_utc(),
            run_id: run(),
            card_id: Some(card.clone()),
            session,
            source: "conductor".to_owned(),
            kind,
        };
        log.push_str(&serde_json::to_string(&event).unwrap());
        log.push('\n');
    }
    std::fs::write(dir.join(run_files::EVENTS), log).unwrap();
    set_run(harness, state, live);
}

fn set_run(harness: &Harness, state: RunState, live: bool) {
    harness
        .conductor
        .set_runs(vec![summary(run(), state, live)]);
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
fn write_run(harness: &Harness) {
    write_run_as(harness, "opencode", RunState::Merged, false, Vec::new());
}

/// OpenCode, set up but unable to resume a session.
fn opencode_that_cant_resume(harness: &Harness) {
    let fake = FakeWorker::harness("opencode", "opencode");
    let capabilities = WorkerCapabilities {
        resume: false,
        ..fake.capabilities()
    };
    harness.directory.add(fake.with_capabilities(capabilities));
}

async fn start() -> Harness {
    let harness = Harness::start().await;
    write_run(&harness);
    opencode_that_cant_resume(&harness);
    harness
}

/// The card's branch, as the merged run left it, with a file its owner added on it.
fn card_branch(harness: &Harness) {
    let repo = &harness.repo;
    repo.git(&["checkout", "-q", "-b", BRANCH]).unwrap();
    repo.write("only-on-card.txt", "added on the task\n")
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

fn resume_chat(n: u32) -> UiCommand {
    UiCommand::Slash {
        command: SlashCommand::Resume {
            target: Some(ResumeTarget::Conversation(ChatId::numbered(n))),
        },
    }
}

fn slash(command: SlashCommand) -> UiCommand {
    UiCommand::Slash { command }
}

async fn notice(harness: &mut Harness, part: &str) -> String {
    match harness
        .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains(part)))
        .await
    {
        AppEvent::Notice { text, .. } => text,
        _ => unreachable!(),
    }
}

async fn listed(harness: &mut Harness) -> (Vec<AgentSession>, Vec<RunId>) {
    harness
        .send(UiCommand::Slash {
            command: SlashCommand::Resume { target: None },
        })
        .await;
    let AppEvent::View { data } = harness
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

fn owner_of(agents: &[AgentSession]) -> &AgentSession {
    agents
        .iter()
        .find(|agent| agent.session == SessionKey::numbered(1))
        .expect("the worker's session")
}

/// Carries the owner's session on and waits until it's on screen.
async fn carry_on_owner(harness: &mut Harness) -> String {
    harness.send(resume("s-001")).await;
    notice(harness, "Carrying on").await
}

/// Sends `command` and waits for its turn to end and the app to settle.
async fn turn(harness: &mut Harness, command: UiCommand) {
    harness.send(command).await;
    harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Chat {
                    event: ChatEvent::TurnFinished { .. } | ChatEvent::TurnFailed { .. }
                }
            )
        })
        .await;
    harness
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
    }
}

fn owner_turn(text: &str) -> FakeSession {
    FakeSession::completed(text).with_vendor_session(OWNER_SESSION)
}

#[tokio::test]
async fn resume_lists_each_agents_session_under_its_run() {
    let mut harness = start().await;
    harness.repo.git(&["branch", BRANCH]).unwrap();
    let (agents, read) = listed(&mut harness).await;
    assert_eq!(read, [run()]);
    let ids: Vec<String> = agents
        .iter()
        .map(|agent| agent.reference().to_string())
        .collect();
    assert_eq!(ids, ["run-01/s-001", "run-01/s-002"]);

    let owner = &agents[0];
    assert_eq!(owner.role, RoleName::Worker);
    // Its name in the run, as the chat and the panel call it, not its card.
    assert_eq!(owner.label.as_deref(), Some("build:money"));
    assert_eq!(owner.worker.to_string(), "claude-code/claude-opus-5-5");
    assert_eq!(
        owner.last_message.as_deref(),
        Some("Fixed both functions. All 6 tests pass.")
    );
    // Its copy is gone since the merge, but its branch makes it again.
    assert_eq!(owner.resume, Resume::Ready);
    // The explorer is counted inside the owner's session, never a session of its own.
    assert_eq!(owner.helpers.len(), 1);
    assert_eq!(owner.helpers[0].label.as_deref(), Some("explorer"));
    assert_eq!(owner.helper_counts(), "1 subagent");

    let Resume::Cannot { why } = &agents[1].resume else {
        panic!("{:?}", agents[1].resume)
    };
    assert_eq!(why, "OpenCode can't resume a session");

    // An older run's agents come when the picker opens it.
    harness
        .send(UiCommand::Request {
            view: ViewRequest::AgentSessions { run: run() },
        })
        .await;
    let AppEvent::View { data } = harness
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
    harness.exit().await;
}

/// Until its run ended, the run owns a finished agent's session, since its next round on the
/// card resumes that same CLI session: nothing carries it on, and no copy is made for it.
#[tokio::test]
async fn a_session_whose_run_hasnt_ended_stays_the_runs() {
    let mut harness = Harness::start().await;
    opencode_that_cant_resume(&harness);
    harness.repo.git(&["branch", BRANCH]).unwrap();
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
        (Some(working), input(&copy_of(&harness))),
    ];
    write_run_as(&harness, "opencode", RunState::Running, true, fixer);
    let (agents, _) = listed(&mut harness).await;
    assert_eq!(owner_of(&agents).resume, Resume::RunOwns);
    assert_eq!(agents[2].resume, Resume::Working);

    for (state, live) in [
        (RunState::Parked, false),
        (RunState::Crashed, false),
        (RunState::Pausing, true),
        // A supervisor still finishing the run holds it until it lets go.
        (RunState::Merged, true),
    ] {
        set_run(&harness, state, live);
        let (agents, _) = listed(&mut harness).await;
        assert_eq!(owner_of(&agents).resume, Resume::RunOwns, "{state:?}");
    }

    set_run(&harness, RunState::Parked, false);
    harness.send(resume("s-001")).await;
    let said = notice(&mut harness, "still owns").await;
    assert_eq!(
        said,
        "run-01 hasn't ended, so it still owns run-01/s-001. t in its agent view takes it over, \
         or it carries on here once the run ends."
    );
    assert!(!copy_of(&harness).exists());
    assert!(
        !harness
            .seen()
            .iter()
            .any(|event| matches!(event, AppEvent::ChatResumed { .. }))
    );
    harness.exit().await;
}

#[tokio::test]
async fn a_finished_owner_carries_on_in_its_copy_made_again_after_the_merge() {
    let mut harness = start().await;
    harness.repo.git(&["branch", BRANCH]).unwrap();
    let copy = copy_of(&harness);
    assert!(!copy.exists());
    harness.agent.push(owner_turn("Added a test."));

    harness.send(resume("s-001")).await;
    let AppEvent::ChatResumed {
        chat,
        worker,
        turns,
        ..
    } = harness
        .until(|event| matches!(event, AppEvent::ChatResumed { .. }))
        .await
    else {
        unreachable!()
    };
    assert_eq!(chat, ChatId::numbered(1));
    assert_eq!(worker, Some("claude-code/claude-opus-5-5".parse().unwrap()));
    assert_eq!(turns.len(), 1);
    assert!(matches!(turns[0].from, Party::Agent { .. }));
    assert_eq!(turns[0].text, OWNER_SAID);
    let said = notice(&mut harness, "Carrying on").await;
    assert!(
        said.starts_with("Carrying on build:money in run-01"),
        "{said}"
    );
    assert!(said.contains("through claude's own resume"), "{said}");
    assert!(
        said.contains(&format!(
            "It works on the run's copy in {} on {BRANCH}, not your checkout.",
            harness.env.display(&copy)
        )),
        "{said}"
    );
    assert!(said.contains("made again from the branch"), "{said}");
    // The copy is back where the session ran, on the card's branch.
    let head = harness
        .repo
        .git(&["-C", copy.to_str().unwrap(), "branch", "--show-current"])
        .unwrap();
    assert_eq!(head.trim(), BRANCH);

    turn(&mut harness, said_text("add a test for -0.5")).await;
    let spec = &harness.agent.specs()[0];
    // A direct turn of the owner's own session, through its CLI's resume, in the run's copy.
    assert_eq!(spec.resume.as_deref(), Some(OWNER_SESSION));
    assert_eq!(spec.workdir, copy);
    assert_eq!(spec.access, Access::Direct);
    assert_eq!(spec.worker.model.as_deref(), Some("claude-opus-5-5"));
    // A restart carries it on in the same folder.
    let store = ChatStore::open(&harness.env.chat_dir(), &ChatId::numbered(1));
    let place = store.place().unwrap();
    assert_eq!(place.folder, copy);
    assert!(place.made_again);
    assert_eq!(store.session().unwrap().vendor_session, OWNER_SESSION);

    // Done with it, the copy made again goes, and the branch is free again.
    harness.send(slash(SlashCommand::Clear)).await;
    let said = notice(&mut harness, "Removed the copy").await;
    assert_eq!(
        said,
        format!(
            "Removed the copy made again for run-01/s-001 in {}.",
            harness.env.display(&copy)
        )
    );
    assert!(!copy.exists());
    assert!(harness.repo.git(&["branch", "-f", BRANCH, "main"]).is_ok());

    // Coming back makes it again.
    harness.send(resume_chat(1)).await;
    let said = notice(&mut harness, "It works on the run's copy").await;
    assert!(
        said.ends_with("so it was made again from the branch."),
        "{said}"
    );
    assert!(copy.is_dir());
    harness.exit().await;
}

/// Work left uncommitted keeps the copy until it's committed, and the agent's commits are
/// named with how to bring them in.
#[tokio::test]
async fn a_copy_made_again_stays_while_it_holds_uncommitted_work() {
    let mut harness = start().await;
    harness.repo.git(&["branch", BRANCH]).unwrap();
    carry_on_owner(&mut harness).await;
    harness.agent.push(owner_turn("Noted."));
    turn(&mut harness, said_text("take notes")).await;
    let copy = copy_of(&harness);
    std::fs::write(copy.join("notes.txt"), "draft\n").unwrap();
    harness.send(slash(SlashCommand::Clear)).await;
    let said = notice(&mut harness, "not committed yet").await;
    assert!(said.contains("so it stays"), "{said}");
    assert!(copy.is_dir());

    let path = copy.to_str().unwrap();
    harness.repo.git(&["-C", path, "add", "notes.txt"]).unwrap();
    harness
        .repo
        .git(&["-C", path, "commit", "-qm", "notes"])
        .unwrap();
    harness.send(resume_chat(1)).await;
    notice(&mut harness, "It works on the run's copy").await;
    harness.send(slash(SlashCommand::Clear)).await;
    let said = notice(&mut harness, "Removed the copy").await;
    assert!(
        said.ends_with(&format!(
            "Its branch {BRANCH} keeps 1 commit your checkout doesn't have, and git merge \
             {BRANCH} brings it in."
        )),
        "{said}"
    );
    assert!(!copy.exists());
    harness.exit().await;
}

/// At start, the copies made again for earlier conversations go once nothing in them is
/// uncommitted, but never the one of the conversation on screen.
#[tokio::test]
async fn copies_made_again_for_earlier_conversations_go_at_start() {
    let (parts, _) = TestCtx::new(HarnessOptions::default());
    let repo = &parts.repo;
    let ctx = &parts.ctx;
    let base = repo.home().join("worktrees/repo/run-01");
    let (clean, dirty) = (base.join("task-001"), base.join("task-002"));
    for (n, folder) in [(1, &clean), (2, &dirty)] {
        let branch = format!("task-00{n}");
        repo.git(&["branch", &branch]).unwrap();
        repo.git(&["worktree", "add", folder.to_str().unwrap(), &branch])
            .unwrap();
        let store = ChatStore::open(&ctx.env.chat_dir(), &ChatId::numbered(n));
        store
            .save_place(&ChatPlace {
                agent: AgentSessionRef::parse(&format!("run-01/s-00{n}")).unwrap(),
                worker: "claude-code".parse().unwrap(),
                folder: folder.clone(),
                branch: Some(branch),
                made_again: true,
            })
            .unwrap();
    }
    std::fs::write(dirty.join("notes.txt"), "draft\n").unwrap();
    let chat = super::ChatState::new(ctx).unwrap();
    super::tidy_copies(ctx, &chat);
    assert!(!clean.exists());
    assert!(dirty.is_dir());
}

#[tokio::test]
async fn a_session_its_cli_cant_resume_shows_its_transcript() {
    let mut harness = start().await;
    harness.send(resume("s-002")).await;
    let said = notice(&mut harness, "can't carry on").await;
    assert_eq!(
        said,
        "run-01/s-002 can't carry on, since OpenCode can't resume a session. Here is its \
         transcript."
    );
    let asked = harness.conductor.calls().into_iter().any(|call| {
        matches!(call, Call::Raw { run, session } if run.as_str() == RUN && session.as_str() == "s-002")
    });
    assert!(asked, "{:?}", harness.conductor.calls());
    assert!(
        !harness
            .seen()
            .iter()
            .any(|event| matches!(event, AppEvent::ChatResumed { .. }))
    );
    harness.exit().await;
}

/// "Can continue" holds only while the copy or its branch is there and the CLI is installed,
/// and nothing is made again until both hold.
#[tokio::test]
async fn a_session_whose_branch_or_cli_is_gone_cant_carry_on() {
    let mut harness = start().await;
    let (agents, _) = listed(&mut harness).await;
    assert_eq!(
        owner_of(&agents).resume,
        Resume::Cannot {
            why: format!("its copy is gone, and so is its branch {BRANCH}")
        }
    );
    harness.send(resume("s-001")).await;
    let said = notice(&mut harness, "can't carry on").await;
    assert!(said.contains("so is its branch"), "{said}");
    assert!(!copy_of(&harness).exists());

    harness.repo.git(&["branch", BRANCH]).unwrap();
    harness.directory.uninstall("claude-code");
    let (agents, _) = listed(&mut harness).await;
    assert_eq!(
        owner_of(&agents).resume,
        Resume::Cannot {
            why: "claude isn't installed".to_owned()
        }
    );
    harness.send(resume("s-001")).await;
    let said = notice(&mut harness, "isn't installed").await;
    assert_eq!(
        said,
        "run-01/s-001 can't carry on, since claude isn't installed. Here is its transcript."
    );
    assert!(!copy_of(&harness).exists());
    harness.exit().await;
}

/// The run may go again after the session carried on, as a stopped run you resume does. Then
/// it owns the session again: the conversation waits, and coming back to it says why.
#[tokio::test]
async fn a_carried_on_conversation_waits_while_its_run_goes_again() {
    let mut harness = start().await;
    harness.repo.git(&["branch", BRANCH]).unwrap();
    carry_on_owner(&mut harness).await;
    harness.agent.push(owner_turn("Added a test."));
    turn(&mut harness, said_text("add a test")).await;
    let held = RunLock::try_acquire(&harness.env.run_dir(&run()))
        .unwrap()
        .expect("the run's lock");
    harness.submit("go on").await;
    let said = notice(&mut harness, "is going again").await;
    assert_eq!(
        said,
        "run-01 is going again, so it owns run-01/s-001 until it ends, and this conversation \
         waits. t in its agent view takes it over."
    );
    assert_eq!(
        harness.agent.specs().len(),
        1,
        "no second process on the session"
    );
    drop(held);

    set_run(&harness, RunState::Running, false);
    harness.send(slash(SlashCommand::Clear)).await;
    harness.send(resume_chat(1)).await;
    let said = notice(&mut harness, "chat-001 can't carry on").await;
    assert!(
        said.starts_with("chat-001 can't carry on: run-01 hasn't ended, so it still owns"),
        "{said}"
    );
    harness.exit().await;
}

/// `/branch` keeps working where the original does, with the same agent, on a session of its
/// own, and says so; `/fork` leaves a copy that does the same.
#[tokio::test]
async fn a_branch_works_with_the_same_agent_in_the_same_copy() {
    let mut harness = start().await;
    harness.repo.git(&["branch", BRANCH]).unwrap();
    carry_on_owner(&mut harness).await;
    harness.agent.push(owner_turn("Added a test."));
    turn(&mut harness, said_text("add a test for -0.5")).await;
    let copy = copy_of(&harness);

    harness
        .send(slash(SlashCommand::Branch { name: None }))
        .await;
    let said = notice(&mut harness, "Branched into").await;
    assert!(
        said.contains(&format!(
            " on Claude Code in the run's copy in {} on {BRANCH}, not your checkout, on a \
             session of its own.",
            harness.env.display(&copy)
        )),
        "{said}"
    );
    harness
        .agent
        .push(FakeSession::completed("Both pass.").with_vendor_session("branch-session"));
    turn(&mut harness, said_text("run the tests")).await;
    let spec = &harness.agent.specs()[1];
    assert_eq!(spec.resume, None, "two conversations never share a session");
    assert_eq!(spec.workdir, copy);
    assert_eq!(spec.access, Access::Direct);
    assert_eq!(spec.worker.to_string(), "claude-code/claude-opus-5-5");
    let primer: String = spec.prompt.iter().map(|part| part.text.as_str()).collect();
    assert!(primer.contains("add a test for -0.5"), "{primer}");
    let branched = ChatStore::open(&harness.env.chat_dir(), &ChatId::numbered(2));
    assert_eq!(branched.place().unwrap().folder, copy);
    assert_eq!(branched.session().unwrap().vendor_session, "branch-session");

    harness
        .send(slash(SlashCommand::Fork { prompt: None }))
        .await;
    let said = notice(&mut harness, "Forked this conversation").await;
    assert!(said.contains("in the run's copy"), "{said}");
    let forked = ChatStore::open(&harness.env.chat_dir(), &ChatId::numbered(3));
    assert_eq!(forked.place().unwrap().folder, copy);
    assert!(forked.session().is_none());
    harness.exit().await;
}

/// `!` commands, reviews, and the `@` list work where the conversation does: the run's copy,
/// not your checkout, and what the agent hears of a command names that folder.
#[tokio::test]
async fn shell_reviews_and_files_work_in_the_copy() {
    // No reviewer of another company is set up, so the main agent reviews.
    let mut harness = Harness::start().await;
    write_run(&harness);
    card_branch(&harness);
    carry_on_owner(&mut harness).await;
    let copy = copy_of(&harness);
    harness
        .until(|event| {
            matches!(event, AppEvent::RepoFiles { paths }
                if paths.iter().any(|path| path == "only-on-card.txt"))
        })
        .await;

    harness
        .send(UiCommand::Shell {
            command: "pwd".to_owned(),
        })
        .await;
    harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Shell {
                    event: ShellEvent::Finished { .. }
                }
            )
        })
        .await;
    let printed: String = harness
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
        copy.canonicalize().unwrap()
    );
    harness.agent.push(owner_turn("It's the copy."));
    turn(&mut harness, said_text("where did that run?")).await;
    let heard = harness.agent.specs()[0]
        .prompt
        .iter()
        .find(|part| part.label == "shell commands")
        .map(|part| part.text.clone())
        .expect("the command reaches the agent");
    assert!(
        heard.starts_with(&format!(
            "Before this message I ran these commands myself, in {}.",
            copy.display()
        )),
        "{heard}"
    );

    std::fs::write(copy.join("only-on-card.txt"), "changed in the copy\n").unwrap();
    harness
        .agent
        .push(FakeSession::structured(serde_json::json!({
            "kind": "answer", "answer_markdown": "Looks fine.", "estimate": null
        })));
    turn(&mut harness, slash(SlashCommand::Review { target: None })).await;
    let review: String = harness.agent.specs()[1]
        .prompt
        .iter()
        .map(|part| part.text.as_str())
        .collect();
    assert!(review.contains("only-on-card.txt"), "{review}");
    assert_eq!(harness.agent.specs()[1].workdir, copy);
    harness.exit().await;
}

/// The CLI's own commands go to the agent whose session the conversation carries on, through
/// that session, and a new main agent leaves the conversation with it.
#[tokio::test]
async fn cli_commands_and_a_new_main_agent_keep_the_carried_on_session() {
    let mut harness = start().await;
    harness.repo.git(&["branch", BRANCH]).unwrap();
    harness.directory.add(FakeWorker::harness("codex", "codex"));
    carry_on_owner(&mut harness).await;
    let copy = copy_of(&harness);

    harness.agent.push(owner_turn("Wrote CLAUDE.md."));
    turn(
        &mut harness,
        slash(SlashCommand::Vendor {
            name: "init".to_owned(),
            args: String::new(),
        }),
    )
    .await;
    let spec = &harness.agent.specs()[0];
    assert_eq!(spec.resume.as_deref(), Some(OWNER_SESSION));
    assert_eq!(spec.workdir, copy);
    assert_eq!(spec.access, Access::Direct);
    assert_eq!(spec.worker.model.as_deref(), Some("claude-opus-5-5"));

    harness
        .send(UiCommand::SetSetting {
            key: "main_agent".to_owned(),
            value: "\"codex\"".to_owned(),
            scope: SettingScope::Session,
        })
        .await;
    let said = notice(&mut harness, "stays with that agent").await;
    assert!(
        said.starts_with("This conversation carries on run-01/s-001 with ")
            && said
                .contains(" on Claude Code, so it stays with that agent. /clear starts one with ")
            && said.ends_with("Codex."),
        "{said}"
    );
    harness.agent.push(owner_turn("Done."));
    turn(&mut harness, said_text("go on")).await;
    let spec = &harness.agent.specs()[1];
    assert_eq!(spec.resume.as_deref(), Some(OWNER_SESSION));
    assert_eq!(spec.workdir, copy);
    let store = ChatStore::open(&harness.env.chat_dir(), &ChatId::numbered(1));
    assert_eq!(store.session().unwrap().vendor_session, OWNER_SESSION);
    harness.exit().await;
}

/// The `/` menu and the keywords the input draws are those of the CLI the conversation goes
/// to: Codex's while it carries on a Codex reviewer, though the main agent is Claude Code.
#[tokio::test]
async fn the_menu_and_keywords_follow_the_carried_on_cli() {
    let mut harness = Harness::start().await;
    harness.directory.add(FakeWorker::harness("codex", "codex"));
    write_run_as(&harness, "codex", RunState::Merged, false, Vec::new());
    harness.repo.git(&["branch", BRANCH]).unwrap();
    let main = vendor_set(&mut harness).await;
    assert_eq!(main.vendor.0, "claude-code");
    assert!(!main.keywords.is_empty(), "Claude Code draws ultrathink");

    harness.send(resume("s-002")).await;
    let reviewer = vendor_set(&mut harness).await;
    assert_eq!(reviewer.vendor.0, "codex");
    assert!(reviewer.keywords.is_empty(), "{:?}", reviewer.keywords);

    harness.send(slash(SlashCommand::Clear)).await;
    let back = vendor_set(&mut harness).await;
    assert_eq!(back.vendor.0, "claude-code");
    harness.exit().await;
}

async fn vendor_set(harness: &mut Harness) -> VendorCommandSet {
    let AppEvent::VendorCommands { set } = harness
        .until(|event| matches!(event, AppEvent::VendorCommands { .. }))
        .await
    else {
        unreachable!()
    };
    set
}

/// A carried-on session's turn works in the run's copy, so `/rewind` puts back what it changed
/// there, and the ignored file its tool calls wrote is named as one nothing kept, while your
/// checkout keeps its own files (`/rewind` with carried-on sessions).
#[tokio::test]
async fn rewind_puts_back_a_carried_on_turns_files_in_the_copy() {
    use argonauts_core::app::{AnswerTarget, ChoiceKind};
    use argonauts_core::events::checkpoint::AnswerAction;
    use argonauts_core::worker::{SessionSpec, WorkerEvent};
    use serde_json::json;

    let mut harness = start().await;
    let repo = &harness.repo;
    repo.git(&["checkout", "-q", "-b", BRANCH]).unwrap();
    repo.write(".gitignore", ".env\n").unwrap();
    repo.write("money.py", "sign = 1\n").unwrap();
    repo.commit_all("the task's work").unwrap();
    repo.git(&["checkout", "-q", "main"]).unwrap();
    let checkout = repo.root().to_owned();
    let copy = copy_of(&harness);
    carry_on_owner(&mut harness).await;
    assert_eq!(
        std::fs::read_to_string(copy.join("money.py")).unwrap(),
        "sign = 1\n"
    );

    harness.agent.respond(|spec: &SessionSpec| {
        let root = &spec.workdir;
        std::fs::write(root.join("money.py"), "sign = -1\n").unwrap();
        std::fs::write(root.join(".env"), "TOKEN=from-the-agent\n").unwrap();
        let tool = |id: &str, name: &str, path: &Path| {
            WorkerEvent::Agent(AgentEvent::ToolStarted {
                id: id.into(),
                name: name.into(),
                input: json!({"file_path": path.to_string_lossy()}),
                parent: None,
            })
        };
        owner_turn("Flipped the sign.")
            .with_event(tool("t1", "Edit", &root.join("money.py")))
            .with_event(tool("t2", "Write", &root.join(".env")))
    });
    turn(&mut harness, said_text("flip the sign")).await;
    assert_eq!(harness.agent.specs()[0].workdir, copy);

    harness
        .send(slash(SlashCommand::Rewind { card: None }))
        .await;
    let AppEvent::Choice { choice } = harness
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::RewindTurn))
        .await
    else {
        unreachable!()
    };
    let AnswerAction::Choose { value } = &choice.options[0].action else {
        panic!("{:?}", choice.options[0])
    };
    harness
        .send(UiCommand::Answer {
            to: AnswerTarget::Choice { id: choice.id },
            action: AnswerAction::Choose {
                value: value.clone(),
            },
            text: None,
            evidence_sha256: None,
            cards: Vec::new(),
            remember: false,
        })
        .await;
    let AppEvent::ChoiceClosed { outcome, .. } = harness
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
        std::fs::read_to_string(copy.join("money.py")).unwrap(),
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
    harness.exit().await;
}

/// `/plan` in a carried-on session plans first in that session, read-only in the run's copy,
/// and changes nothing until you go ahead, as plan first does in direct solo.
#[tokio::test]
async fn plan_in_a_carried_on_session_changes_nothing_until_you_go_ahead() {
    use argonauts_core::app::{AnswerTarget, ChoiceKind};
    use argonauts_core::events::checkpoint::AnswerAction;
    use argonauts_core::worker::SessionSpec;

    let mut harness = start().await;
    card_branch(&harness);
    let copy = copy_of(&harness);
    carry_on_owner(&mut harness).await;
    harness.agent.respond(|spec: &SessionSpec| {
        if spec.access == Access::Direct {
            std::fs::write(spec.workdir.join("planned.txt"), "done\n").unwrap();
            owner_turn("Added planned.txt.")
        } else {
            owner_turn("1. Add planned.txt.")
        }
    });
    harness
        .send(slash(SlashCommand::Plan {
            task: Some("add planned.txt".into()),
        }))
        .await;
    let AppEvent::Choice { choice } = harness
        .until(|event| matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::GoAhead))
        .await
    else {
        unreachable!()
    };
    assert_eq!(
        choice.body,
        ["Claude Code carries it out in the run's copy.".to_owned()]
    );
    let specs = harness.agent.specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].access, Access::ReadOnly);
    assert_eq!(specs[0].workdir, copy);
    assert_eq!(specs[0].resume.as_deref(), Some(OWNER_SESSION));
    assert!(!copy.join("planned.txt").exists());

    harness
        .send(UiCommand::Answer {
            to: AnswerTarget::Choice { id: choice.id },
            action: AnswerAction::Choose {
                value: "yes".to_owned(),
            },
            text: None,
            evidence_sha256: None,
            cards: Vec::new(),
            remember: false,
        })
        .await;
    harness
        .until(|event| {
            matches!(
                event,
                AppEvent::Chat {
                    event: ChatEvent::TurnFinished { .. }
                }
            )
        })
        .await;
    let specs = harness.agent.specs();
    assert_eq!(specs[1].access, Access::Direct);
    assert_eq!(specs[1].workdir, copy);
    assert!(copy.join("planned.txt").exists());
    assert!(!harness.repo.root().join("planned.txt").exists());
    harness.exit().await;
}
