//! A seat's turn in a council's talk. Every turn answers in the same shape, and the turn's move
//! says which parts count: a first answer and a message write `message`, a tapback gives only
//! `stances`, the note keeper's list fills `points` and its plan `plan`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::plan::{GoalBrief, PlanDraft};
use super::{Step, StepName};
use crate::runs::events::council::Stance;

pub struct Talk;

impl Step for Talk {
    const NAME: StepName = StepName::Talk;
    type Input = TalkInput;
    type Output = TalkTurn;
}

/// What every turn carries: the goal the council answers. Who the seat is, and its letters
/// for the others, come in its seat's frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TalkInput {
    pub goal: GoalBrief,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TalkTurn {
    /// What you write to the council, in plain words. Empty when your move asks for none.
    pub message: String,
    /// Whom you answer: "Seat T", or "the user".
    pub reply_to: Option<String>,
    /// The words of their message you answer, copied exactly.
    pub quote: Option<String>,
    /// Your stance on each point your message or tapback touches, by its number.
    pub stances: Vec<TalkStance>,
    /// Points the council should also agree on, each in one sentence.
    pub new_points: Vec<String>,
    /// New words for an open point.
    pub reword: Vec<Reworded>,
    /// The note keeper's list after the first answers: each point and its sides.
    pub points: Vec<ListedPoint>,
    /// The note keeper's plan, once every point is settled.
    pub plan: Option<PlanDraft>,
}

/// A stance on one point. A changed stance names what moved it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TalkStance {
    pub point: u32,
    pub stance: Stance,
    /// Why, when this changes your stance: what you read or ran, in a few words.
    pub because: Option<String>,
    /// Whose words moved you: "Seat T", or "the user".
    pub message: Option<String>,
    /// Those words, copied exactly.
    pub quote: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Reworded {
    pub point: u32,
    pub words: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListedPoint {
    /// The point in one sentence.
    pub words: String,
    /// Where the first answers disagree on it: each side, and the seats that took it.
    pub sides: Vec<ListedSide>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListedSide {
    pub words: String,
    /// The seats that took this side, by letter, as "Seat T".
    pub seats: Vec<String>,
    /// Its best reason, in one sentence, as the seats that took it gave it.
    #[serde(default)]
    pub why: String,
}
