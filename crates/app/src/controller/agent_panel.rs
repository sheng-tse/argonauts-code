//! The agent panel's rows the screen can't build itself: the conversations, the agent CLIs' own
//! sessions in this repo, and the runs of this repo and of every repo with a live run host.
//! The screen adds its own agents, live, in place of the run it holds.

use std::collections::BTreeSet;

use argonauts_core::agent_panel::{
    AgentPanelRows, CrewDetails, CrewRow, CrewRun, CrewState, CrewTarget, RunStands,
};
use argonauts_core::api::{RunState, RunSummary};
use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::cli_sessions::{CliScan, CliSession};
use argonauts_core::events::RunEvent;
use argonauts_core::hidden_git::MergedInto;
use argonauts_core::journal;
use argonauts_core::layout::run_files;
use argonauts_core::names::ModelRef;

use super::conversation::{self, ConversationState};
use super::{Ctx, Effect, run_commands};

/// Each CLI's newest sessions the panel lists.
const SESSIONS_PER_CLI: usize = 20;

/// This repo's newest runs whose agents the panel lists under them. Each costs a read of its
/// journal, so an older run is listed whole, and its workflow view shows its agents.
const RUNS_WITH_AGENTS: usize = 5;

pub(crate) async fn show(
    ctx: &mut Ctx,
    conversation: &ConversationState,
) -> anyhow::Result<Vec<Effect>> {
    let repo = ctx.env.display_repo();
    let conversation_dir = ctx.env.conversation_dir();
    let mut rows = conversation::crew_rows(&conversation_dir, conversation, &repo);
    let own = conversation::crew_cli_sessions(&conversation_dir);
    rows.extend(
        cli_sessions(ctx)
            .await
            .iter()
            .filter(|session| !own.contains(&session.id))
            .map(|session| session_row(session, &repo)),
    );
    for (at, listed) in run_commands::all_repos(ctx).await?.iter().enumerate() {
        // This repo comes first, and its journals are the ones in reach.
        let with_agents = if at == 0 { RUNS_WITH_AGENTS } else { 0 };
        let agents = run_agents(ctx, &listed.runs[..with_agents.min(listed.runs.len())]).await;
        for (n, run) in listed.runs.iter().enumerate() {
            match agents.get(n).filter(|agents| !agents.is_empty()) {
                Some(events) => rows.extend(crate::views::crew_rows(
                    events,
                    &crew_run(run),
                    &listed.repo,
                    run.live,
                )),
                None => rows.push(run_row(run, &listed.repo)),
            }
        }
    }
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Crew {
            panel: AgentPanelRows { repo, rows },
        }),
    });
    Ok(Vec::new())
}

/// Every agent CLI's own sessions that ran in this repo, read on a blocking thread.
async fn cli_sessions(ctx: &Ctx) -> Vec<CliSession> {
    let Some(home) = ctx.env.user_home.clone() else {
        return Vec::new();
    };
    let adapters = match crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved()) {
        Ok(adapters) => adapters,
        Err(err) => {
            tracing::warn!("listing the agent CLIs for the agent panel: {err:#}");
            return Vec::new();
        }
    };
    let scan = CliScan::new(ctx.env.repo_root.clone(), home);
    let listed = tokio::task::spawn_blocking(move || {
        let mut seen = BTreeSet::new();
        let mut sessions = Vec::new();
        for adapter in adapters {
            // A CLI can be both built in and set up again as a CLI.
            if !seen.insert(adapter.id().to_owned()) {
                continue;
            }
            match adapter.cli_sessions(&scan, SESSIONS_PER_CLI) {
                Ok(found) => sessions.extend(found),
                Err(err) => tracing::warn!("listing {}'s sessions: {err:#}", adapter.id()),
            }
        }
        sessions
    })
    .await;
    listed.unwrap_or_default()
}

/// A CLI session takes more, so it's ready.
pub(crate) fn session_row(session: &CliSession, repo: &str) -> CrewRow {
    CrewRow {
        target: CrewTarget::CliSession {
            session: session.reference(),
        },
        state: CrewState::Ready,
        title: session.title.clone(),
        label: None,
        tries: 0,
        repo: repo.to_owned(),
        run: None,
        worker: Some(ModelRef::new(
            session.agent_id.0.clone(),
            session.model.clone(),
        )),
        latest: session.last_message.clone().unwrap_or_default(),
        updated: session.updated,
        details: CrewDetails {
            last_message: session.last_message.clone(),
            branch: session.branch.clone(),
            prompt: session.prompt.clone(),
            ..CrewDetails::default()
        },
    }
}

/// The events of each of `runs`, in order, read on a blocking thread. A journal that can't be
/// read gives none, and its run is listed whole.
async fn run_agents(ctx: &Ctx, runs: &[RunSummary]) -> Vec<Vec<RunEvent>> {
    let journals: Vec<_> = runs
        .iter()
        .map(|run| ctx.env.run_dir(&run.run).join(run_files::EVENTS))
        .collect();
    let read = tokio::task::spawn_blocking(move || {
        journals
            .iter()
            .map(|path| match journal::read_all(path) {
                Ok(back) => back.events,
                Err(err) => {
                    tracing::warn!("reading {} for the agent panel: {err}", path.display());
                    Vec::new()
                }
            })
            .collect()
    })
    .await;
    read.unwrap_or_default()
}

/// A whole run, as one row that is its own heading.
pub(crate) fn run_row(run: &RunSummary, repo: &str) -> CrewRow {
    CrewRow {
        target: CrewTarget::Run {
            run: run.run.clone(),
        },
        state: run_state(run),
        title: run.title.clone(),
        label: None,
        tries: 0,
        repo: repo.to_owned(),
        run: Some(crew_run(run)),
        worker: None,
        latest: run.stopped_at.clone(),
        updated: run.updated,
        details: CrewDetails {
            spent: Some(run.spent),
            ..CrewDetails::default()
        },
    }
}

/// The run as its heading names it: its own name, where it stands, and what it spent.
pub(crate) fn crew_run(run: &RunSummary) -> CrewRun {
    let spent = run.spent;
    CrewRun {
        id: run.run.clone(),
        title: run.title.clone(),
        name: run
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
        stands: Some(RunStands {
            state: run_state(run),
            word: stands_word(run),
            since: run.updated,
            spent: (spent.total_tokens() > 0 || spent.cost_usd > 0.0).then_some(spent),
        }),
    }
}

/// Where the run stands, in the heading's words. One that waits on you says what for.
fn stands_word(run: &RunSummary) -> String {
    if run.needs_you() {
        return match run.stopped_at.trim() {
            "" => "needs you".to_owned(),
            at => at.to_owned(),
        };
    }
    // No branch took a merge in a folder without git, and the run's summary says so.
    let written = MergedInto::YourFolder.ended();
    if run.state == RunState::Merged && run.stopped_at == written {
        return written;
    }
    match run.state {
        RunState::Running if run.live => "running",
        RunState::Pausing if run.live => "pausing",
        RunState::Running | RunState::Pausing | RunState::Waiting => "waiting",
        RunState::Crashed => "stopped",
        RunState::NeverStarted => "never started",
        RunState::FromAnotherBuild => "from an older Argonauts",
        RunState::Merged => "merged",
        RunState::PullRequest => "pull request",
        RunState::BranchLeft => "branch left",
        RunState::Stopped => "stopped",
        RunState::Failed => "failed",
        RunState::Abandoned => "abandoned",
    }
    .to_owned()
}

/// A waiting or crashed run takes more once resumed, so it's ready; one that waits on a question
/// needs you. A stopped run resumes; an abandoned one is over, as a merged one is, and only a
/// follow-up builds on it.
fn run_state(run: &RunSummary) -> CrewState {
    if !run.waiting.is_empty() {
        return CrewState::NeedsYou;
    }
    match run.state {
        RunState::Running | RunState::Pausing if run.live => CrewState::Working,
        RunState::Running | RunState::Pausing | RunState::Waiting | RunState::Crashed => {
            CrewState::Ready
        }
        // Nothing of these can go on.
        RunState::NeverStarted | RunState::FromAnotherBuild => CrewState::Stopped,
        RunState::Merged | RunState::PullRequest | RunState::BranchLeft | RunState::Abandoned => {
            CrewState::Done
        }
        RunState::Stopped => CrewState::Stopped,
        RunState::Failed => CrewState::Failed,
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::Paths;
    use argonauts_core::app::{ConversationEvent, TurnReply, UiCommand, ViewRequest};
    use argonauts_core::events::run::RunMode;
    use argonauts_core::names::{AgentId, ApprovalId, ConversationId, TurnId};
    use argonauts_core::{RunId, Spend};
    use time::OffsetDateTime;

    use super::*;
    use crate::controller::test_support::{CliOptions, TestDriver};
    use crate::conversation_store::ConversationStore;
    use crate::env::AppEnv;

    fn summary(state: RunState, live: bool, waiting: bool) -> RunSummary {
        RunSummary {
            run: RunId::new("run-07").unwrap(),
            title: "Fix the login redirect".into(),
            mode: RunMode::Crew,
            workflow: "council".into(),
            state,
            stopped_at: "plan approval".into(),
            updated: OffsetDateTime::UNIX_EPOCH,
            spent: Spend {
                output_tokens: 9_000,
                cost_usd: 0.46,
                ..Spend::default()
            },
            sessions_to_resume: 1,
            waiting: if waiting {
                vec![ApprovalId::numbered(1)]
            } else {
                Vec::new()
            },
            live,
            remind: false,
            name: None,
        }
    }

    #[test]
    fn a_run_written_into_a_folder_without_git_never_reads_as_merged() {
        let mut run = summary(RunState::Merged, false, false);
        run.stopped_at = "merged".into();
        assert_eq!(stands_word(&run), "merged");

        run.stopped_at = "written into your folder".into();
        assert_eq!(stands_word(&run), "written into your folder");
        assert_eq!(run_row(&run, "~/todo").latest, "written into your folder");
    }

    #[test]
    fn runs_read_as_the_tab_they_belong_to() {
        let state = |state, live, waiting| run_state(&summary(state, live, waiting));
        assert_eq!(state(RunState::Waiting, false, true), CrewState::NeedsYou);
        assert_eq!(state(RunState::Running, true, false), CrewState::Working);
        assert_eq!(state(RunState::Running, false, false), CrewState::Ready);
        assert_eq!(state(RunState::Waiting, false, false), CrewState::Ready);
        assert_eq!(state(RunState::Crashed, false, false), CrewState::Ready);
        assert_eq!(state(RunState::Merged, false, false), CrewState::Done);
        assert_eq!(state(RunState::Abandoned, false, false), CrewState::Done);
        assert_eq!(state(RunState::Stopped, false, false), CrewState::Stopped);
        assert_eq!(state(RunState::Failed, false, false), CrewState::Failed);

        let row = run_row(&summary(RunState::Waiting, false, true), "~/web");
        assert_eq!(row.repo, "~/web");
        assert_eq!(row.latest, "plan approval");
        assert_eq!(row.run.as_ref().map(|run| run.id.as_str()), Some("run-07"));
        assert_eq!(
            row.details.spent.map(|spent| spent.output_tokens),
            Some(9_000)
        );
    }

    #[tokio::test]
    async fn the_panel_lists_conversations_and_runs_with_their_states() {
        let mut driver = TestDriver::start_with(CliOptions {
            prepare: Some(Box::new(|repo| {
                let env = AppEnv::for_repo(Paths::new(repo.home()), repo.root(), true).unwrap();
                let store =
                    ConversationStore::open(&env.conversation_dir(), &ConversationId::numbered(4));
                for event in [
                    ConversationEvent::TurnStarted {
                        turn: TurnId::numbered(1),
                        text: "How does the export pick a format?".to_owned(),
                        worker: "claude-code".parse().unwrap(),
                        read_only: true,
                        commit: "abc".to_owned(),
                        uncommitted: Vec::new(),
                        settings_sha256: None,
                        continues: false,
                    },
                    ConversationEvent::TurnFinished {
                        turn: TurnId::numbered(1),
                        reply: TurnReply::Answer {
                            markdown: "It reads --format first.".to_owned(),
                        },
                        spend: Spend::default(),
                        elapsed_ms: 10,
                    },
                ] {
                    store.append(&event).unwrap();
                }
            })),
            ..CliOptions::default()
        })
        .await;
        driver
            .conductor
            .set_runs(vec![summary(RunState::Waiting, false, true)]);
        driver
            .send(UiCommand::Request {
                view: ViewRequest::Crew,
            })
            .await;
        let AppEvent::View { data } = driver
            .until(|event| {
                matches!(event, AppEvent::View { data } if matches!(**data, ViewData::Crew { .. }))
            })
            .await
        else {
            unreachable!()
        };
        let ViewData::Crew { panel } = *data else {
            unreachable!()
        };
        let conversation = panel
            .rows
            .iter()
            .find(|row| matches!(&row.target, CrewTarget::Conversation { conversation } if *conversation == ConversationId::numbered(4)))
            .expect("the conversation");
        assert_eq!(conversation.state, CrewState::Ready);
        assert_eq!(conversation.title, "How does the export pick a format?");
        assert_eq!(conversation.latest, "It reads --format first.");
        let run = panel
            .rows
            .iter()
            .find(|row| matches!(row.target, CrewTarget::Run { .. }))
            .expect("this repo's run");
        assert_eq!(run.state, CrewState::NeedsYou);
        assert_eq!(run.repo, panel.repo);
        driver.exit().await;
    }

    /// A run of this repo that the screen doesn't hold lists its agents under its heading,
    /// read from its journal, so its title shows once.
    #[tokio::test]
    async fn a_run_of_this_repo_lists_its_agents_under_its_heading() {
        use crate::client::test_support::{session_ended, session_started, write_journal};
        use argonauts_core::events::RunEventKind;
        use argonauts_core::events::session::{EndReason, SessionEvent};
        use argonauts_core::names::{RoleName, SessionKey};

        let mut driver = TestDriver::start_with(CliOptions {
            prepare: Some(Box::new(|repo| {
                let env = AppEnv::for_repo(Paths::new(repo.home()), repo.root(), true).unwrap();
                let dir = env.run_dir(&RunId::new("run-07").unwrap());
                std::fs::create_dir_all(&dir).unwrap();
                let key = SessionKey::numbered(1);
                let mut started = session_started(0, &key, None, RoleName::Worker, "codex");
                if let RunEventKind::Session(SessionEvent::Started(start)) = &mut started.kind {
                    start.label = Some("build:login".to_owned());
                }
                let ended = session_ended(1, &key, EndReason::Completed, 1_000);
                write_journal(&dir.join(run_files::EVENTS), &[started, ended]);
            })),
            ..CliOptions::default()
        })
        .await;
        let mut merged = summary(RunState::Merged, false, false);
        merged.name = Some("fix-login-redirect".to_owned());
        driver.conductor.set_runs(vec![merged]);
        driver
            .send(UiCommand::Request {
                view: ViewRequest::Crew,
            })
            .await;
        let AppEvent::View { data } = driver
            .until(|event| {
                matches!(event, AppEvent::View { data } if matches!(**data, ViewData::Crew { .. }))
            })
            .await
        else {
            unreachable!()
        };
        let ViewData::Crew { panel } = *data else {
            unreachable!()
        };
        let of_run: Vec<&CrewRow> = panel.rows.iter().filter(|row| row.run.is_some()).collect();
        assert_eq!(of_run.len(), 1, "{of_run:#?}");
        let agent = of_run[0];
        assert!(matches!(agent.target, CrewTarget::Agent { .. }));
        assert_eq!(agent.label.as_deref(), Some("build:login"));
        assert_eq!(
            (agent.state, agent.latest.as_str()),
            (CrewState::Done, "done")
        );
        let heading = agent.run.as_ref().unwrap();
        assert_eq!(heading.name.as_deref(), Some("fix-login-redirect"));
        let stands = heading.stands.as_ref().unwrap();
        assert_eq!(
            (stands.state, stands.word.as_str()),
            (CrewState::Done, "merged")
        );
        driver.exit().await;
    }

    #[test]
    fn a_cli_session_is_ready_with_its_model_and_branch() {
        let session = CliSession {
            agent_id: AgentId("codex".into()),
            id: "019a2f".into(),
            title: "Explain the input handling".into(),
            prompt: Some("Explain the input handling in detail".into()),
            folder: "/w".into(),
            updated: OffsetDateTime::UNIX_EPOCH,
            model: Some("gpt-5.6".into()),
            branch: Some("main".into()),
            last_message: Some("It reads keys in one loop.".into()),
            turns: 0,
        };
        let row = session_row(&session, "~/app");
        assert_eq!(row.state, CrewState::Ready);
        assert_eq!(
            row.worker.as_ref().map(ToString::to_string).as_deref(),
            Some("codex/gpt-5.6")
        );
        assert_eq!(row.latest, "It reads keys in one loop.");
        assert_eq!(row.details.branch.as_deref(), Some("main"));
        let CrewTarget::CliSession { session } = &row.target else {
            panic!("{:?}", row.target);
        };
        assert_eq!(session.to_string(), "codex:019a2f");
    }
}
