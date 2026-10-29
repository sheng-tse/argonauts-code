//! Council, `[council]`: who sits and in what role, how each task may be done, who checks a
//! task when no other maker can, what happens when you write mid-talk, and what stops a talk
//! that doesn't reach agreement. Council's talk and its plan read these.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Span;
use super::ways::Ways;
use crate::events::council::SeatRole;
use crate::names::ModelRef;

/// What Council's talk and its plan follow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct CouncilSettings {
    /// The seats in order, each a model and its role. A model may sit more than once, each
    /// seat its own session. Unset, each of the crew's workers sits once as a proposer
    #[serde(rename = "seat", skip_serializing_if = "Vec::is_empty")]
    pub seats: Vec<CouncilSeat>,
    /// How each task may be done. The note keeper picks one way per task from these and gives
    /// its reason
    pub ways: Ways,
    /// When no other maker is set up to check a task, a fresh session of the same model checks
    /// it, marked Same maker: `allow` it, or `ask` you once per council first
    pub same_model_check: SameModelCheck,
    /// When you write mid-talk while a model is writing: it may `finish` its message, then read
    /// yours, or `restart` with yours
    pub when_you_write: WhenYouWrite,
    /// Who settles the points still open when the talk stops without agreement
    pub unsettled: Unsettled,
    /// When a talk that doesn't reach agreement stops, whichever comes first
    pub stop_after: TalkStop,
}

impl Default for CouncilSettings {
    fn default() -> Self {
        Self {
            seats: Vec::new(),
            ways: Ways::default(),
            same_model_check: SameModelCheck::Allow,
            when_you_write: WhenYouWrite::Finish,
            unsettled: Unsettled::You,
            stop_after: TalkStop::default(),
        }
    }
}

/// One `[[council.seat]]`. Two entries may name the same model: each is its own seat, with its
/// own session and letters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CouncilSeat {
    /// The CLI and model that sits, such as `claude-code/opus`
    pub model: ModelRef,
    /// What it does in the talk: `proposer`, `skeptic` or `checker`
    #[serde(default)]
    pub role: SeatRole,
}

/// `council.same_model_check`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SameModelCheck {
    /// The fresh session checks, and the plan and each check's line say Same maker.
    #[default]
    Allow,
    /// You're asked once per council whether such a check may go ahead.
    Ask,
}

/// `council.when_you_write`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WhenYouWrite {
    /// The model writing finishes its message, then reads yours as soon as its CLI can take it.
    #[default]
    Finish,
    /// The model writing stops, and starts its message again with yours.
    Restart,
}

/// `council.unsettled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Unsettled {
    /// You decide each open point. In Autopilot, which asks nothing, the council votes.
    #[default]
    You,
    /// The council votes on each open point, even when you're there to decide.
    Vote,
}

/// `[council.stop_after]`. A talk ends by itself once every model agrees on every point; these
/// stop one that doesn't.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TalkStop {
    /// Messages in a row that change nobody's stance on any point
    pub messages_that_move_nobody: u32,
    /// Tokens the talk spends, every model's together
    pub tokens: u64,
    /// The talk's working time, such as "15m"
    pub working_time: Span,
}

impl Default for TalkStop {
    fn default() -> Self {
        Self {
            messages_that_move_nobody: 6,
            tokens: 200_000,
            working_time: Span::new("15m"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::settings::ways::{MostAgents, WorkflowsAllowed};

    #[test]
    fn the_talk_stops_after_six_unmoving_messages_200k_tokens_or_15_minutes() {
        let council = CouncilSettings::default();
        assert!(council.seats.is_empty());
        assert_eq!(council.same_model_check, SameModelCheck::Allow);
        assert_eq!(council.when_you_write, WhenYouWrite::Finish);
        assert_eq!(council.unsettled, Unsettled::You);
        assert_eq!(council.stop_after.messages_that_move_nobody, 6);
        assert_eq!(council.stop_after.tokens, 200_000);
        assert_eq!(
            council.stop_after.working_time.parse(),
            Ok(Some(Duration::from_secs(15 * 60)))
        );
        assert_eq!(council.ways, Ways::default());
    }

    #[test]
    fn each_key_reads_its_words() {
        let council: CouncilSettings = toml::from_str(
            r#"
same_model_check = "ask"
when_you_write = "restart"
unsettled = "vote"

[ways]
own_session = false
workflows = ["maker-checker"]
agents = 3

[stop_after]
messages_that_move_nobody = 4
tokens = 100000
working_time = "30m"
"#,
        )
        .unwrap();
        assert_eq!(council.same_model_check, SameModelCheck::Ask);
        assert_eq!(council.when_you_write, WhenYouWrite::Restart);
        assert_eq!(council.unsettled, Unsettled::Vote);
        assert!(!council.ways.own_session);
        assert_eq!(
            council.ways.workflows,
            WorkflowsAllowed::These(vec!["maker-checker".to_owned()])
        );
        assert_eq!(council.ways.agents, MostAgents::try_from(3).unwrap());
        assert_eq!(council.stop_after.messages_that_move_nobody, 4);
        assert_eq!(council.stop_after.tokens, 100_000);
        assert_eq!(council.stop_after.working_time, Span::new("30m"));
        for wrong in [
            "same_model_check = \"never\"",
            "when_you_write = \"wait\"",
            "unsettled = \"judge\"",
        ] {
            assert!(toml::from_str::<CouncilSettings>(wrong).is_err(), "{wrong}");
        }
    }

    #[test]
    fn a_model_may_take_more_than_one_seat() {
        let council: CouncilSettings = toml::from_str(
            r#"
[[seat]]
model = "claude-code/opus"

[[seat]]
model = "claude-code/opus"
role = "skeptic"

[[seat]]
model = "codex"
role = "checker"
"#,
        )
        .unwrap();
        let seats: Vec<(String, SeatRole)> = council
            .seats
            .iter()
            .map(|seat| (seat.model.to_string(), seat.role))
            .collect();
        assert_eq!(
            seats,
            [
                ("claude-code/opus".to_owned(), SeatRole::Proposer),
                ("claude-code/opus".to_owned(), SeatRole::Skeptic),
                ("codex".to_owned(), SeatRole::Checker),
            ]
        );

        let written = toml::to_string(&council).unwrap();
        assert_eq!(written.matches("[[seat]]").count(), 3, "{written}");
        let unset = toml::to_string(&CouncilSettings::default()).unwrap();
        assert!(!unset.contains("seat"), "{unset}");

        for wrong in [
            "[[seat]]\nrole = \"skeptic\"",
            "[[seat]]\nmodel = \"codex\"\nrole = \"judge\"",
            "[[seat]]\nmodel = \"codex\"\nletter = \"K\"",
        ] {
            assert!(toml::from_str::<CouncilSettings>(wrong).is_err(), "{wrong}");
        }
    }
}
