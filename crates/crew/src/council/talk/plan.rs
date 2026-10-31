//! The note keeper writes the plan from the settled points, and it goes on to your plan
//! approval like any other.

use argonauts_core::agents::session_runner::Access;
use argonauts_core::base::names::SeatId;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::council::{
    CouncilEventKind, FloorReason, HeldChoice, MessageKind, MissedTurn, Point,
};
use argonauts_core::runs::events::plan::PlanEvent;
use argonauts_core::workflows::steps::plan::{PlanChoice, PlanDraft};
use argonauts_core::workflows::steps::talk::TalkTurn;
use argonauts_core::workflows::workflow::StepRef;

use super::missed::missed;
use super::{
    Council, Landing, PLAN, Table, Turn, ask, council_now, handed, land, record, taken_again,
};
use crate::council::relay::{self, Relay, YourMove};
use crate::council::{deliver, private};
use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::Host;
use crate::plan::{context, decisions, drafts};
use crate::state::plan::MERGED_LABEL;
use crate::workflow::StepCall;

pub(super) async fn write_plan(
    host: &Host,
    call: &StepCall,
    council: &Council,
    keeper: &SeatId,
    table: &mut Table,
) -> EngineResult<()> {
    let talk = &call.at.step;
    let Some(seat) = council.seat(keeper) else {
        return Err(EngineError::Refused(format!("{keeper} doesn't sit")));
    };
    record(
        host,
        talk,
        CouncilEventKind::Writing {
            seat: keeper.clone(),
            why: FloorReason::Notes,
        },
    )?;
    let mut text = handed(host, table, council, keeper, YourMove::Write)?;
    text.push('\n');
    text.push_str(&deliver::settled(&table.relay, council, keeper));
    if let Some(left) = sat_out(&table.relay, council, keeper) {
        text.push_str("\n\n");
        text.push_str(&left);
    }
    // Every task reads the plan, so a plan that repeats what you told the note keeper alone
    // waits for you, as one of its messages would, before it is sealed.
    let (answered, plan) = loop {
        let turn = Turn {
            seat,
            template: PLAN,
            access: Access::ReadOnly,
            text: text.clone(),
        };
        let answered = match ask(host, call, council, table, turn).await {
            Ok(answered) => answered,
            Err(err) if someone_else_can_plan(host, talk, council, keeper) => {
                return missed(host, talk, council, keeper, MissedTurn::Plan, None, err);
            }
            Err(err) => return Err(err),
        };
        table
            .sessions
            .insert(keeper.clone(), answered.session.clone());
        let plan = answered
            .output
            .plan
            .clone()
            .unwrap_or_else(|| plan_from_points(&council.points));
        // Every later step and task reads the plan, and none may learn which model wrote it.
        let plan = relay::plan_without_who_wrote(table.relay.lexicon(), plan);
        let now = council_now(host, talk).unwrap_or_else(|| council.clone());
        let words = plan_words(&plan);
        let Some(hold) = private::held(host, &now, keeper, &words)? else {
            break (answered, plan);
        };
        match private::decide_plan(host, call, &now, keeper, &hold, &words).await? {
            HeldChoice::Rewrite => text.push_str(
                "\nYour plan repeated what the user told you in private, so no task reads it. \
                 Write it again without that.",
            ),
            _ => break (answered, plan),
        }
    };
    let round = host.read(|state| state.plan.blind_round(false));
    let id = drafts::seal(
        host,
        MERGED_LABEL,
        &answered.worker,
        round,
        &plan,
        Some(&answered.session),
    )?;
    host.record(
        None,
        None,
        RunEventKind::Plan(PlanEvent::FastPath {
            merged_by: answered.worker.clone(),
        }),
    )?;
    let sealed = host
        .read(|state| {
            state
                .plan
                .drafts
                .iter()
                .find(|draft| draft.draft == id)
                .cloned()
        })
        .ok_or_else(|| EngineError::Refused(format!("the plan {id} was not recorded")))?;
    decisions::record_for(host, &sealed).await?;
    let summary = format!("The council agreed on {} points", council.points.len());
    context::phase_finished(host, &summary)?;
    let words = if answered.output.message.trim().is_empty() {
        plan.summary.clone()
    } else {
        answered.output.message.clone()
    };
    let notes = TalkTurn {
        message: words,
        ..TalkTurn::default()
    };
    // The plan is sealed already, so its words go out as code strips them.
    let taken = taken_again(&table.relay, keeper, notes.message.trim());
    let now = council_now(host, talk).unwrap_or_else(|| council.clone());
    land(
        host,
        &table.relay,
        &now,
        seat,
        &notes,
        Landing::of(MessageKind::Notes, taken),
    )
}

/// Every word of the plan a task reads, for checking against what you said in private.
fn plan_words(plan: &PlanDraft) -> String {
    let mut words = vec![plan.summary.clone(), plan.approach.clone()];
    for piece in &plan.pieces {
        words.push(piece.name.clone());
        words.push(piece.description.clone());
    }
    for choice in &plan.choices {
        words.extend([
            choice.topic.clone(),
            choice.choice.clone(),
            choice.rationale.clone(),
        ]);
    }
    words.extend(plan.risks.iter().cloned());
    words.push(plan.test_strategy.clone());
    words.join("\n")
}

/// The plan a note keeper that gave none would have written: the settled points as choices.
fn plan_from_points(settled: &[Point]) -> PlanDraft {
    // The plan reaches the seats that build it, so a seat a point names is no one in particular.
    let words: Vec<String> = settled
        .iter()
        .map(|point| relay::draw_markers(&point.words, |_| "a seat".to_owned()))
        .collect();
    PlanDraft {
        summary: words.join(". "),
        approach: words
            .iter()
            .map(|words| format!("- {words}"))
            .collect::<Vec<_>>()
            .join("\n"),
        pieces: Vec::new(),
        choices: settled
            .iter()
            .zip(&words)
            .map(|(point, words)| PlanChoice {
                topic: format!("Point {}", point.number),
                choice: words.clone(),
                rationale: "The council settled it".to_owned(),
            })
            .collect(),
        risks: Vec::new(),
        test_strategy: String::new(),
        new_dependencies: Vec::new(),
    }
}

/// Whether the plan still has a writer once `keeper` misses its turn: the keeper itself, with a
/// miss left, or another seat still in. The talk ends only when nobody is left to write it.
fn someone_else_can_plan(host: &Host, talk: &StepRef, council: &Council, keeper: &SeatId) -> bool {
    let now = council_now(host, talk).unwrap_or_else(|| council.clone());
    let keeper_stays = now.missed.get(keeper).is_none_or(|misses| *misses == 0);
    keeper_stays
        || now
            .seats()
            .iter()
            .any(|seat| seat.id != *keeper && !now.left.contains(&seat.id))
}

/// The seats that missed two turns and sat out the rest of the talk, as `keeper` knows them,
/// for its plan to give them no task and to say who sat out and why.
fn sat_out(relay: &Relay, council: &Council, keeper: &SeatId) -> Option<String> {
    if council.left.is_empty() {
        return None;
    }
    let mut lines = vec!["Seats that sat out the rest of the talk:".to_owned()];
    for seat in &council.left {
        let missed = council.missed.get(seat).copied().unwrap_or(2);
        lines.push(format!(
            "- {}, after missing {missed} turns",
            deliver::seat_name(relay, keeper, seat)
        ));
    }
    lines.push("Give them no task, and say in the plan's summary who sat out and why.".to_owned());
    Some(lines.join("\n"))
}
