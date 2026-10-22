//! What the orchestrator of a run your words start begins from. When it runs on the main
//! session's CLI and that CLI has a fork of its own, it is a copy of the main session: it has the
//! whole conversation, and the brief arrives as its first message. Anywhere else it starts from
//! the brief, as every worker does. `crew.orchestrator_context = "brief"` keeps it to the brief.

use std::path::PathBuf;

use argonauts_core::events::run::{BriefFrom, ForkFrom, StartsFrom};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::OrchestratorContext;

use super::ConversationState;
use crate::controller::Ctx;

/// The main session as the orchestrator's fork copies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MainSession {
    pub worker: ModelRef,
    /// The CLI's own id for it.
    pub cli_session: String,
    /// Where it ran, which is where its CLI resumes it: Claude Code keeps sessions by folder.
    pub folder: PathBuf,
}

/// How the orchestrator starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Start {
    Fork(MainSession),
    Brief(Brief),
}

/// Why the orchestrator starts from the brief alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Brief {
    /// `crew.orchestrator_context = "brief"`.
    Chosen,
    /// It runs on another CLI than the main session, which can't read that CLI's sessions.
    OtherCli { orchestrator: String, main: String },
    /// Its CLI, the main session's own, has no fork.
    NoFork { cli: String },
    /// The main session has no session of its CLI's to copy yet.
    NoSession,
}

impl Brief {
    /// Why the orchestrator can't carry the conversation, with each CLI as `cli` names it:
    /// `agy can't carry a Claude Code conversation`. None when you kept it to the brief, or
    /// there was nothing to carry yet.
    fn why(&self, cli: impl Fn(&str) -> String) -> Option<String> {
        match self {
            Self::Chosen | Self::NoSession => None,
            Self::OtherCli { orchestrator, main } => Some(format!(
                "{} can't carry a {} conversation",
                cli(orchestrator),
                cli(main)
            )),
            Self::NoFork { cli: id } => Some(format!("{} can't copy a conversation", cli(id))),
        }
    }
}

/// How the orchestrator of a run with `settings` starts, from this conversation.
pub(crate) fn start(ctx: &Ctx, conversation: &ConversationState, settings: &Settings) -> Start {
    let Some(orchestrator) = orchestrator(ctx, settings) else {
        return Start::Brief(Brief::NoSession);
    };
    start_on(ctx, conversation, settings, &orchestrator)
}

/// How `orchestrator` starts, from this conversation.
pub(crate) fn start_on(
    ctx: &Ctx,
    conversation: &ConversationState,
    settings: &Settings,
    orchestrator: &ModelRef,
) -> Start {
    if settings.crew.orchestrator_context == OrchestratorContext::Brief {
        return Start::Brief(Brief::Chosen);
    }
    let Some(session) = &conversation.session else {
        return Start::Brief(Brief::NoSession);
    };
    if session.worker.agent != orchestrator.agent {
        return Start::Brief(Brief::OtherCli {
            orchestrator: orchestrator.agent.clone(),
            main: session.worker.agent.clone(),
        });
    }
    let forks = ctx
        .services
        .workers
        .get(orchestrator)
        .is_ok_and(|runner| runner.capabilities().fork);
    if !forks {
        return Start::Brief(Brief::NoFork {
            cli: orchestrator.agent.clone(),
        });
    }
    Start::Fork(MainSession {
        worker: session.worker.clone(),
        cli_session: session.cli_session.clone(),
        folder: conversation.folder(ctx).to_path_buf(),
    })
}

/// The run's orchestrator, as the engine picks it: the first of the role's models that can
/// run here, else the first of them.
fn orchestrator(ctx: &Ctx, settings: &Settings) -> Option<ModelRef> {
    let models = argonauts_crew::roles::candidates(settings, RoleName::Orchestrator);
    let available = ctx.services.workers.available();
    models
        .iter()
        .find(|model| available.contains(model))
        .or(models.first())
        .cloned()
}

impl Start {
    /// What the Starting line says after the orchestrator's name: `carrying this
    /// conversation`, or `starting from the brief, since agy can't carry a Claude Code
    /// conversation`, with each CLI as `cli` names it.
    pub(crate) fn words(&self, cli: impl Fn(&str) -> String) -> String {
        const BRIEF: &str = "starting from the brief";
        match self {
            Self::Fork(_) => "carrying this conversation".to_owned(),
            Self::Brief(brief) => match brief.why(cli) {
                Some(why) => format!("{BRIEF}, since {why}"),
                None => BRIEF.to_owned(),
            },
        }
    }

    /// What the run records it starts from: the session it copies, with the `tokens` the
    /// conversation holds, or the brief `main` wrote and why the orchestrator has only that.
    pub(crate) fn starts_from(
        &self,
        main: &ModelRef,
        tokens: Option<u64>,
        cli: impl Fn(&str) -> String,
    ) -> StartsFrom {
        match self {
            Self::Fork(session) => StartsFrom::Fork(ForkFrom {
                worker: session.worker.clone(),
                cli_session: session.cli_session.clone(),
                folder: session.folder.clone(),
                tokens,
            }),
            Self::Brief(brief) => StartsFrom::Brief(BriefFrom {
                written_by: main.clone(),
                why: brief.why(cli),
            }),
        }
    }

    pub(crate) fn forks(&self) -> bool {
        matches!(self, Self::Fork(_))
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::display_names::DisplayNames;
    use argonauts_core::session_runner::{RunnerCapabilities, SessionRunner as _};
    use argonauts_testkit::FakeRunner;

    use super::*;
    use crate::controller::test_support::{CliOptions, TestCtx};
    use crate::conversation_store::ConversationSession;

    fn model(text: &str) -> ModelRef {
        text.parse().unwrap()
    }

    /// The orchestrator copies the main session only on that session's CLI, when the CLI has
    /// a fork and you left `crew.orchestrator_context` as it is.
    #[test]
    fn only_the_main_sessions_own_cli_with_a_fork_copies_it() {
        let (parts, _) = TestCtx::new(CliOptions::default());
        let runner = FakeRunner::cli("claude-code/opus-5-5", "anthropic");
        let capabilities = RunnerCapabilities {
            fork: true,
            ..runner.capabilities()
        };
        parts.directory.add(runner.with_capabilities(capabilities));
        let mut conversation = ConversationState::new(&parts.ctx).unwrap();
        let settings = Settings::default();
        let opus = model("claude-code/opus-5-5");
        let start = |conversation: &ConversationState, settings: &Settings, worker: &str| {
            start_on(&parts.ctx, conversation, settings, &model(worker))
        };
        assert_eq!(
            start(&conversation, &settings, "claude-code/opus-5-5"),
            Start::Brief(Brief::NoSession)
        );
        conversation.session = Some(ConversationSession {
            worker: opus.clone(),
            cli_session: "main-session-1".to_owned(),
            fork: false,
        });
        assert_eq!(
            start(&conversation, &settings, "claude-code/opus-5-5"),
            Start::Fork(MainSession {
                worker: opus,
                cli_session: "main-session-1".to_owned(),
                folder: parts.repo.root().to_path_buf(),
            })
        );
        assert_eq!(
            start(&conversation, &settings, "antigravity-cli"),
            Start::Brief(Brief::OtherCli {
                orchestrator: "antigravity-cli".to_owned(),
                main: "claude-code".to_owned(),
            })
        );
        // The test's own Claude Code, which any other model on it runs, has no fork.
        assert_eq!(
            start(&conversation, &settings, "claude-code/sonnet-5"),
            Start::Brief(Brief::NoFork {
                cli: "claude-code".to_owned()
            })
        );
        let mut brief = Settings::default();
        brief.crew.orchestrator_context = OrchestratorContext::Brief;
        assert_eq!(
            start(&conversation, &brief, "claude-code/opus-5-5"),
            Start::Brief(Brief::Chosen)
        );
    }

    /// The words for each start read as the Starting line's picture has them.
    #[test]
    fn each_start_says_what_the_orchestrator_begins_from() {
        let names = DisplayNames::default();
        let cli = |id: &str| names.cli(id);
        let fork = Start::Fork(MainSession {
            worker: "claude-code/opus-5-5".parse().unwrap(),
            cli_session: "11111111-2222-4333-8444-555555555555".to_owned(),
            folder: PathBuf::from("/repo"),
        });
        assert_eq!(fork.words(cli), "carrying this conversation");
        assert!(fork.forks());
        let other = Start::Brief(Brief::OtherCli {
            orchestrator: "antigravity-cli".to_owned(),
            main: "claude-code".to_owned(),
        });
        assert_eq!(
            other.words(cli),
            "starting from the brief, since agy can't carry a Claude Code conversation"
        );
        assert!(!other.forks());
        assert_eq!(
            Start::Brief(Brief::Chosen).words(cli),
            "starting from the brief"
        );
        assert_eq!(
            Start::Brief(Brief::NoFork {
                cli: "antigravity-cli".to_owned()
            })
            .words(cli),
            "starting from the brief, since agy can't copy a conversation"
        );
    }

    /// The run records the session a fork copies, with the conversation's size, or the brief,
    /// who wrote it and why the orchestrator has only that.
    #[test]
    fn each_start_is_kept_with_the_run_for_its_views() {
        let names = DisplayNames::default();
        let cli = |id: &str| names.cli(id);
        let opus: ModelRef = "claude-code/opus-5-5".parse().unwrap();
        let session = MainSession {
            worker: opus.clone(),
            cli_session: "main-1".to_owned(),
            folder: PathBuf::from("/repo"),
        };
        assert_eq!(
            Start::Fork(session).starts_from(&opus, Some(48_200), cli),
            StartsFrom::Fork(ForkFrom {
                worker: opus.clone(),
                cli_session: "main-1".to_owned(),
                folder: PathBuf::from("/repo"),
                tokens: Some(48_200),
            })
        );
        let other = Start::Brief(Brief::OtherCli {
            orchestrator: "antigravity-cli".to_owned(),
            main: "claude-code".to_owned(),
        });
        assert_eq!(
            other.starts_from(&opus, Some(48_200), cli),
            StartsFrom::Brief(BriefFrom {
                written_by: opus.clone(),
                why: Some("agy can't carry a Claude Code conversation".to_owned()),
            })
        );
        assert_eq!(
            Start::Brief(Brief::Chosen).starts_from(&opus, None, cli),
            StartsFrom::Brief(BriefFrom {
                written_by: opus,
                why: None,
            })
        );
    }
}
