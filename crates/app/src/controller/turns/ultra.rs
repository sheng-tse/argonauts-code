//! What ultracode and the typed `ultracode` do with one request. They
//! change how often a workflow is used, never what one is:
//! - with a crew workflow in use, the word sends this request to it, and the setting sends
//!   every real request, small ones too (the Captain-or-crew rules' own);
//! - in Captain on Claude Code, the main session runs Claude Code's own workflow;
//! - in Captain on a CLI with no workflow of its own, Argonauts Code runs one in its place, with every
//!   agent on that same CLI and model.

use argonauts_core::agents::session_runner::CliWorkflow;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::ForcedMode;
use argonauts_core::settings::sessions::{FanoutSwitch, NativeFanout};
use argonauts_crew::workflow::ultra::has_own_workflow;

/// Where one request goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    /// As the settings say, since nothing asks for a workflow beyond them.
    AsUsual,
    /// The main session, with what its CLI's own workflow does for this message.
    MainSession(CliWorkflow),
    /// A workflow Argonauts Code runs in the main agent's place, every agent on its CLI and model.
    SameModel,
    /// The crew workflow in use runs this request.
    Crew,
}

/// `typed_keyword`: you typed `ultracode` with this message and didn't ignore it.
pub(crate) fn route(settings: &Settings, typed_keyword: bool) -> Route {
    if !captain(settings) {
        return if typed_keyword {
            Route::Crew
        } else {
            Route::AsUsual
        };
    }
    if has_own_workflow(&settings.crew.main) {
        return Route::MainSession(CliWorkflow {
            ultracode: settings.ultracode,
            typed_keyword,
            ..CliWorkflow::default()
        });
    }
    if typed_keyword {
        Route::SameModel
    } else {
        Route::AsUsual
    }
}

/// What the main session's CLI does with its own workflow, for a turn that isn't a request of
/// yours, such as a reply to a question: ultracode stays on in Captain on Claude Code.
pub(crate) fn main_session(settings: &Settings) -> CliWorkflow {
    CliWorkflow {
        ultracode: captain(settings) && has_own_workflow(&settings.crew.main) && settings.ultracode,
        typed_keyword: false,
        ..CliWorkflow::default()
    }
}

/// Captain with ultracode on, on a CLI with no workflow of its own: a run the main session starts
/// for real work is a workflow on that same model, as Claude Code's would be.
pub(crate) fn runs_on_the_same_model(settings: &Settings) -> bool {
    captain(settings) && settings.ultracode && !has_own_workflow(&settings.crew.main)
}

/// The main session's standing line while [`runs_on_the_same_model`]: real work goes to a
/// workflow, and questions and small talk are answered.
pub(crate) fn standing_line(settings: &Settings) -> Option<&'static str> {
    runs_on_the_same_model(settings).then_some(ULTRACODE_LINE)
}

const ULTRACODE_LINE: &str = "Ultracode is on: for every substantive task, call \
    argonauts_start_workflow with the user's request instead of doing the work yourself, even \
    though they didn't ask for a crew. It runs a workflow of agents on your own model. Answer questions \
    and conversational or trivial turns yourself.";

/// The request's settings for a workflow on the main agent's model alone: the orchestrator
/// writes the steps, and every role is that model, its own reviewer included.
pub(crate) fn same_model(settings: &Settings) -> Vec<String> {
    let main = toml_list(std::slice::from_ref(&settings.crew.main));
    vec![
        "workflow=\"orchestrator\"".to_owned(),
        "crew.orchestrator=\"main\"".to_owned(),
        format!("crew.worker={main}"),
        format!("crew.reviewer={main}"),
        format!("crew.judge={main}"),
        "review.other_provider=false".to_owned(),
    ]
}

/// Why Claude Code can't run its own workflow in the main session, when it can't: with
/// fan-out off it starts with workflows turned off.
pub(crate) fn unavailable(settings: &Settings) -> Option<&'static str> {
    let fanout = settings
        .roles
        .get(&RoleName::Main)
        .and_then(|role| role.native_fanout)
        .unwrap_or(settings.native_fanout);
    let off = matches!(
        fanout,
        NativeFanout::Switch(FanoutSwitch::Off) | NativeFanout::Limit { max: 0 }
    );
    off.then_some(
        "Ultracode is unavailable: native_fanout is off, so Claude Code can't run a workflow.",
    )
}

fn captain(settings: &Settings) -> bool {
    settings.captain_or_crew.force == Some(ForcedMode::Captain)
}

fn toml_list(workers: &[ModelRef]) -> String {
    let items: Vec<String> = workers
        .iter()
        .map(|worker| toml::Value::String(worker.to_string()).to_string())
        .collect();
    format!("[{}]", items.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::base::test_support::{CliOptions, TestDriver};
    use argonauts_core::app::{AppEvent, ConversationEvent, UiCommand};
    use argonauts_testkit::FakeSession;

    /// The test driver with `config` as your global settings.
    fn with_config(config: &'static str) -> CliOptions {
        CliOptions {
            prepare: Some(Box::new(move |repo: &argonauts_testkit::TempRepo| {
                std::fs::write(repo.global_config(), config).unwrap();
            })),
            ..CliOptions::default()
        }
    }

    const CAPTAIN: &str = "[captain_or_crew]\nforce = \"captain\"\n";

    fn typed(text: &str) -> UiCommand {
        UiCommand::Submit {
            text: text.to_owned(),
            workflow_requested: true,
            pictures: Vec::new(),
        }
    }

    fn turn_ended(event: &AppEvent) -> bool {
        matches!(
            event,
            AppEvent::Conversation {
                event: ConversationEvent::TurnFinished { .. }
                    | ConversationEvent::TurnFailed { .. }
            }
        )
    }

    /// Captain on Claude Code: the message you typed the word in goes to the main session marked
    /// as yours, so Claude Code's own keyword runs its workflow. With ultracode on, every turn
    /// has Claude Code's own ultracode.
    #[tokio::test]
    async fn captain_on_claude_code_hands_the_word_and_the_switch_to_claude_code() {
        let mut driver = TestDriver::start_with(with_config(CAPTAIN)).await;
        driver.agent.push(FakeSession::completed("Tidied."));
        driver.send(typed("ultracode tidy the date helpers")).await;
        driver.until(turn_ended).await;
        let spec = driver.agent.specs().remove(0);
        driver.exit().await;
        assert!(spec.cli_workflow.typed_keyword);
        assert!(!spec.cli_workflow.ultracode);
        assert_eq!(spec.prompt[0].text, "ultracode tidy the date helpers");

        let config = "ultracode = true\n[captain_or_crew]\nforce = \"captain\"\n";
        let mut driver = TestDriver::start_with(with_config(config)).await;
        driver.agent.push(FakeSession::completed("Tidied."));
        driver.submit("tidy the date helpers").await;
        driver.until(turn_ended).await;
        let spec = driver.agent.specs().remove(0);
        let started = driver.conductor.starts();
        driver.exit().await;
        assert!(spec.cli_workflow.ultracode);
        assert!(!spec.cli_workflow.typed_keyword);
        assert!(started.is_empty(), "Claude Code runs its own workflow");
    }

    /// With a crew workflow in use, the word sends this one request to it.
    #[tokio::test]
    async fn the_word_starts_the_crew_workflow_for_one_request() {
        let mut driver = TestDriver::start_with(CliOptions::default()).await;
        driver.send(typed("ultracode tidy the date helpers")).await;
        driver
            .until(|event| matches!(event, AppEvent::Attached { .. }))
            .await;
        let started = driver.conductor.starts();
        driver.exit().await;
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].request, "ultracode tidy the date helpers");
        assert_eq!(
            started[0].mode,
            argonauts_core::runs::events::run::RunMode::Crew
        );
    }

    /// Captain on Codex, which has no workflow of its own: Argonauts Code runs one with every agent
    /// on Codex's model.
    #[tokio::test]
    async fn captain_elsewhere_runs_the_workflow_on_the_main_agents_model() {
        let config = "crew.main = \"codex/gpt-5.6\"\n[captain_or_crew]\nforce = \"captain\"\n";
        let mut driver = TestDriver::start_with(with_config(config)).await;
        driver.send(typed("ultracode tidy the date helpers")).await;
        driver
            .until(|event| matches!(event, AppEvent::Attached { .. }))
            .await;
        let started = driver.conductor.starts();
        driver.exit().await;
        assert_eq!(started.len(), 1);
        assert!(
            started[0]
                .request_settings
                .contains(&"crew.worker=[\"codex/gpt-5.6\"]".to_owned()),
            "{:?}",
            started[0].request_settings
        );
    }

    fn settings(captain: bool, main: &str, ultracode: bool) -> Settings {
        let mut settings = Settings::default();
        settings.captain_or_crew.force = captain.then_some(ForcedMode::Captain);
        settings.crew.main = main.parse().unwrap();
        settings.ultracode = ultracode;
        settings
    }

    #[test]
    fn the_word_sends_one_request_to_a_crew_workflow() {
        let crew = settings(false, "codex", false);
        assert_eq!(route(&crew, true), Route::Crew);
        assert_eq!(route(&crew, false), Route::AsUsual);
        // The setting sends every real request through the Captain-or-crew rules' own instead.
        assert_eq!(
            route(&settings(false, "codex", true), false),
            Route::AsUsual
        );
    }

    #[test]
    fn captain_on_claude_code_is_claude_codes_own() {
        let on = settings(true, "claude-code/opus-5-5", true);
        assert_eq!(
            route(&on, false),
            Route::MainSession(CliWorkflow {
                ultracode: true,
                typed_keyword: false,
                ..CliWorkflow::default()
            })
        );
        let off = settings(true, "claude-code", false);
        assert_eq!(
            route(&off, true),
            Route::MainSession(CliWorkflow {
                ultracode: false,
                typed_keyword: true,
                ..CliWorkflow::default()
            })
        );
        assert_eq!(
            route(&off, false),
            Route::MainSession(CliWorkflow::default())
        );
        assert!(main_session(&on).ultracode);
        assert!(!main_session(&settings(false, "claude-code", true)).ultracode);
        assert!(unavailable(&on).is_none());
        let mut off = on.clone();
        off.native_fanout = NativeFanout::Switch(FanoutSwitch::Off);
        assert!(unavailable(&off).is_some());
    }

    #[test]
    fn captain_elsewhere_runs_a_workflow_on_that_same_model() {
        let codex = settings(true, "codex/gpt-5.6", true);
        assert_eq!(route(&codex, true), Route::SameModel);
        assert_eq!(route(&codex, false), Route::AsUsual);
        assert!(runs_on_the_same_model(&codex));
        assert!(standing_line(&codex).is_some());
        assert!(standing_line(&settings(true, "claude-code", true)).is_none());
        assert_eq!(
            same_model(&codex),
            [
                "workflow=\"orchestrator\"",
                "crew.orchestrator=\"main\"",
                "crew.worker=[\"codex/gpt-5.6\"]",
                "crew.reviewer=[\"codex/gpt-5.6\"]",
                "crew.judge=[\"codex/gpt-5.6\"]",
                "review.other_provider=false",
            ]
        );
    }
}
