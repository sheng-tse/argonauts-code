//! A conversation that comes back on screen is drawn by the printers that drew it live, as Claude
//! Code feeds a resumed session's messages to its own screen: its record in order, and after the
//! turn that started each of its runs, that run's own record, all of it, between `PastBegins`
//! and `PastDrawn` so nothing in it happens again. A command you typed while a run went on goes
//! between the run's events from before it and after it, as it printed then. The screen leaves
//! out what it already shows.

use std::collections::VecDeque;

use argonauts_core::RunId;
use argonauts_core::app::{AppEvent, ConversationEvent};
use argonauts_core::base::layout::run_files;
use argonauts_core::base::names::TurnId;
use argonauts_core::runs::events::run::RunLifecycle;
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::runs::journal;

use crate::controller::Ctx;
use crate::conversation::store::ConversationStore;
use crate::views::required;

/// `store`'s record, with each of `runs` after the turn that started it. A run whose turn the
/// record lacks, as one a command started, comes after the rest.
pub(crate) fn conversation(ctx: &Ctx, store: &ConversationStore, runs: &[RunId]) {
    let runs: Vec<Vec<RunEvent>> = runs.iter().map(|run| record_of(ctx, run)).collect();
    ctx.send(AppEvent::PastBegins);
    for past in in_order(store.every_event(), runs) {
        match past {
            Past::Said(event) => ctx.send(AppEvent::Conversation { event }),
            Past::Ran(event) => send_one(ctx, &event),
        }
    }
    ctx.send(AppEvent::PastDrawn);
}

/// One event of a past drawn again, the conversation's or a run's.
#[derive(Debug)]
enum Past {
    Said(ConversationEvent),
    Ran(RunEvent),
}

/// The conversation's record with each run's after the turn that started it, a command you
/// typed between the run's events from before and after it, and a later turn of yours after
/// all the runs before it did.
fn in_order(
    record: impl IntoIterator<Item = ConversationEvent>,
    runs: Vec<Vec<RunEvent>>,
) -> Vec<Past> {
    let mut left: Vec<(Option<TurnId>, Vec<RunEvent>)> = runs
        .into_iter()
        .map(|events| (started_in(&events), events))
        .collect();
    // The runs whose turn has ended, with the events of theirs not drawn yet.
    let mut going: Vec<VecDeque<RunEvent>> = Vec::new();
    let mut out = Vec::new();
    for event in record {
        match &event {
            // An older record's command has no time, so the runs before it draw first, as
            // they always did.
            ConversationEvent::CommandTyped { at, .. } => {
                let before = |next: &mut RunEvent| at.is_none_or(|at| next.ts <= at);
                for events in &mut going {
                    while let Some(next) = events.pop_front_if(before) {
                        out.push(Past::Ran(next));
                    }
                }
            }
            ConversationEvent::TurnStarted { .. } => {
                out.extend(going.drain(..).flatten().map(Past::Ran));
            }
            _ => {}
        }
        let ended = ended_turn(&event).cloned();
        out.push(Past::Said(event));
        let Some(turn) = ended else {
            continue;
        };
        let (now, later) = left
            .into_iter()
            .partition(|(started, _)| started.as_ref() == Some(&turn));
        left = later;
        going.extend(now.into_iter().map(|(_, events)| VecDeque::from(events)));
    }
    out.extend(going.into_iter().flatten().map(Past::Ran));
    out.extend(
        left.into_iter()
            .flat_map(|(_, events)| events)
            .map(Past::Ran),
    );
    out
}

/// A run alone, as one that comes on screen without the conversation that started it, or before
/// it is attached again: the screen draws what of it it hasn't shown yet.
pub(crate) fn run(ctx: &Ctx, run: &RunId) {
    let events = record_of(ctx, run);
    if events.is_empty() {
        return;
    }
    ctx.send(AppEvent::PastBegins);
    send_run(ctx, events);
    ctx.send(AppEvent::PastDrawn);
}

fn send_run(ctx: &Ctx, events: impl IntoIterator<Item = RunEvent>) {
    for event in events {
        send_one(ctx, &event);
    }
}

fn send_one(ctx: &Ctx, event: &RunEvent) {
    ctx.send(AppEvent::Run {
        event: Box::new(required::for_screen(event, &ctx.env)),
    });
}

/// Every event of `run`'s journal. One that can't be read draws nothing, and the conversation
/// around it still draws.
fn record_of(ctx: &Ctx, run: &RunId) -> Vec<RunEvent> {
    let path = ctx.env.run_dir(run).join(run_files::EVENTS);
    match journal::read_all(&path) {
        Ok(back) => back.events,
        Err(err) => {
            tracing::warn!("reading {run}'s record to draw it again: {err}");
            Vec::new()
        }
    }
}

/// The conversation turn that started the run, as its first event names it.
fn started_in(events: &[RunEvent]) -> Option<TurnId> {
    events.iter().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Started(started)) => started.conversation_turn.clone(),
        _ => None,
    })
}

fn ended_turn(event: &ConversationEvent) -> Option<&TurnId> {
    match event {
        ConversationEvent::TurnFinished { turn, .. }
        | ConversationEvent::TurnFailed { turn, .. }
        | ConversationEvent::TurnCancelled { turn }
        | ConversationEvent::TurnStopped { turn, .. } => Some(turn),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::Note;
    use argonauts_core::NoteLevel;
    use argonauts_core::app::ConversationEvent;
    use argonauts_core::base::names::TurnId;
    use argonauts_core::runs::events::run::RunLifecycle;
    use argonauts_core::runs::events::{RunEvent, RunEventKind};
    use time::{Duration, OffsetDateTime};

    use super::{Past, in_order};
    use crate::client::test_support::{base_ts, event, run_started};

    fn turn() -> TurnId {
        TurnId::numbered(1)
    }

    /// A run the first turn started, with a note at each of `minutes` after it began.
    fn run(minutes: &[i64]) -> Vec<RunEvent> {
        let mut started = run_started(0, "Add a --json flag", None);
        if let RunEventKind::Run(RunLifecycle::Started(started)) = &mut started.kind {
            started.conversation_turn = Some(turn());
        }
        let mut events = vec![started];
        for (seq, minute) in (1..).zip(minutes) {
            let mut note = event(
                seq,
                RunEventKind::Note(Note {
                    level: NoteLevel::Info,
                    text: format!("at {minute}"),
                }),
            );
            note.ts = base_ts() + Duration::minutes(*minute);
            events.push(note);
        }
        events
    }

    fn typed(text: &str, at: Option<OffsetDateTime>) -> ConversationEvent {
        ConversationEvent::CommandTyped {
            text: text.to_owned(),
            at,
        }
    }

    fn finished() -> ConversationEvent {
        serde_json::from_value(serde_json::json!({
            "conversation": "turn_finished",
            "turn": "t-001",
            "reply": {"reply": "answer", "markdown": "On it."},
            "spend": {"input_tokens": 0, "output_tokens": 0, "cached_tokens": 0,
                      "cost_usd": 0.0, "cost_partial": false, "cost_estimated": false},
            "elapsed_ms": 1000
        }))
        .expect("a turn's end")
    }

    /// What each line of the past says, in the order it's drawn.
    fn said(past: &[Past]) -> Vec<String> {
        past.iter()
            .map(|past| match past {
                Past::Said(ConversationEvent::CommandTyped { text, .. }) => text.clone(),
                Past::Said(_) => "turn".to_owned(),
                Past::Ran(event) => match &event.kind {
                    RunEventKind::Note(note) => note.text.clone(),
                    _ => "started".to_owned(),
                },
            })
            .collect()
    }

    /// A command you typed while the run went on goes between what it did before and after.
    #[test]
    fn a_command_typed_during_a_run_goes_where_it_was_typed() {
        let at = base_ts() + Duration::minutes(9);
        let past = in_order(
            [finished(), typed("/usage", Some(at))],
            vec![run(&[1, 8, 10])],
        );
        assert_eq!(
            said(&past),
            ["turn", "started", "at 1", "at 8", "/usage", "at 10"]
        );
    }

    /// An older record's command has no time, so it comes after the whole run, as it did.
    #[test]
    fn a_command_with_no_time_comes_after_the_run() {
        let past = in_order([finished(), typed("/usage", None)], vec![run(&[1, 10])]);
        assert_eq!(said(&past), ["turn", "started", "at 1", "at 10", "/usage"]);
    }
}
