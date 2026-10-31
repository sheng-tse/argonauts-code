//! The note keeper's who-does-what, drawn from the plan the run asks you to approve: each task
//! with a way, who does it and how, why that way, a seat's objection to it, and who checks it
//! where the plan already says. Approve with edits lists the same tasks.

use argonauts_core::base::names::{ModelRef, SeatId};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::look::logos::logo_for;
use argonauts_core::runs::events::approval::{PlanPacket, TaskSeat, WayObjection};
use argonauts_core::runs::events::task::TaskSpec;

use super::{Cx, Talk};
use crate::council::screens::edits::EditTask;
use crate::council::talk::item::Inset;
use crate::council::work::plan::{
    Columns, PROPOSING, PlanTask, RULE, Who, way_word, why_cell, you_asked,
};
use crate::council::{Body, Face, Item, Mark, Piece, SeatSays, Tone};

impl Talk {
    /// The note keeper's who-does-what under its plan message, or in a bubble of its own when
    /// something printed since, with each other seat's ✔ for signing or ✘ for objecting to a
    /// way. The same plan opened again shows once.
    pub(super) fn show_plan(&mut self, plan: &PlanPacket, cx: &mut Cx<'_>) {
        let Some(notes) = self.conversation.notes.clone() else {
            return;
        };
        let names = &cx.state.names;
        let tasks: Vec<PlanTask> = plan
            .graph
            .tasks
            .iter()
            .filter_map(|spec| self.plan_task(spec, plan, names))
            .collect();
        if tasks.is_empty() || self.shown_plan.as_ref() == Some(&tasks) {
            return;
        }
        self.shown_plan = Some(tasks.clone());
        let mut says = SeatSays::new(
            notes.clone(),
            Body::words(vec![vec![Piece::text(PROPOSING)]]),
        );
        let talk = cx
            .state
            .runs
            .get(&self.run)
            .and_then(|run| run.talk.as_ref())
            .filter(|talk| talk.step == self.step);
        let signed = talk.is_some_and(|talk| talk.every_seat_signed());
        says.inset = Some(Inset::Plan {
            tasks,
            columns: Columns::PROPOSED,
            rule: Some(RULE.to_owned()),
            sat_out: talk.map(|talk| talk.sat_out()).unwrap_or_default(),
        });
        if let Some(talk) = talk {
            says.taps = talk
                .seats()
                .filter(|now| now.seat.id != notes)
                .filter_map(|now| {
                    let objects = plan.objections.iter().any(|o| o.seat == now.seat.id);
                    let mark = match (objects, now.signed || signed) {
                        (true, _) => Mark::Object,
                        (false, true) => Mark::Agree,
                        (false, false) => return None,
                    };
                    Some((now.seat.id.clone(), mark))
                })
                .collect();
        }
        if let Some(mut says) = self.under_the_keepers_message(&notes, says) {
            says.note = Some(" · keeps the notes".to_owned());
            self.add(Item::Seat(vec![says]), None, cx);
        }
        if let Some(line) = under_the_plan(signed, &plan.objections) {
            self.add(line, None, cx);
        }
    }

    /// Puts `says` under the note keeper's message while that message is the newest and still
    /// live, as its next bubble. Hands `says` back when it is not.
    fn under_the_keepers_message(&mut self, notes: &SeatId, says: SeatSays) -> Option<SeatSays> {
        let Some((id, _, shared)) = &self.live else {
            return Some(says);
        };
        if self.conversation.seat_of(id).as_ref() != Some(notes) {
            return Some(says);
        }
        match shared.lock() {
            Ok(mut item) => match &mut *item {
                Item::Seat(bubbles) => {
                    bubbles.push(says);
                    None
                }
                _ => Some(says),
            },
            Err(_) => Some(says),
        }
    }

    /// The plan's tasks with a way and a worker, as Approve with edits lists them.
    pub(super) fn edit_tasks(&self, plan: &PlanPacket, names: &DisplayNames) -> Vec<EditTask> {
        plan.graph
            .tasks
            .iter()
            .filter_map(|spec| {
                let task = self.plan_task(spec, plan, names)?;
                let way = spec.way.as_ref()?.how.clone();
                Some(EditTask {
                    task: spec.id.clone(),
                    name: task.name,
                    proposed_who: task.who.clone(),
                    who: task.who,
                    worker: spec.worker.clone(),
                    proposed: way.clone(),
                    way,
                    why: task.why,
                    objection: task.objection,
                })
            })
            .collect()
    }

    /// A task of the plan, when it has a way and a worker, with the seat that builds it and the
    /// first seat's objection to it.
    fn plan_task(
        &self,
        spec: &TaskSpec,
        plan: &PlanPacket,
        names: &DisplayNames,
    ) -> Option<PlanTask> {
        let objections = &plan.objections;
        let way = spec.way.as_ref()?;
        let worker = spec.worker.as_ref()?;
        let who = match self.builds(spec, worker, &plan.seats) {
            Some(seat) => Who::Seat(seat),
            None => self.who(worker, names),
        };
        let why = match (&way.why, way.asked_by_you) {
            (Some(why), _) => why_cell(why),
            (None, true) => vec![you_asked()],
            (None, false) => Vec::new(),
        };
        let checked_by = spec
            .reviewers
            .first()
            .map(|checker| match self.who(checker, names) {
                Who::Seat(seat) => vec![Piece::Badge(seat)],
                Who::Model(face) => vec![Piece::Logo(face.logo)],
            })
            .unwrap_or_default();
        Some(PlanTask {
            name: spec.title.clone(),
            who,
            way: way_word(&way.how),
            why,
            checked_by,
            objection: objections
                .iter()
                .find(|objection| objection.task == spec.id)
                .map(|objection| {
                    let faces = &self.conversation.faces;
                    let reason = objection.words(|seat| faces.face(seat).label());
                    (objection.seat.clone(), reason)
                }),
        })
    }

    /// The seat the plan gives `spec`, where it sits and runs the task's model: the plan names
    /// it even where another seat runs the same model.
    fn builds(&self, spec: &TaskSpec, worker: &ModelRef, seats: &[TaskSeat]) -> Option<SeatId> {
        let given = seats.iter().find(|given| given.task == spec.id)?;
        self.models
            .iter()
            .any(|(seat, model)| *seat == given.seat && model == worker)
            .then(|| given.seat.clone())
    }

    /// The seat that runs `model`. When more than one does, or none, the plan can't say which
    /// seat, so it names the model.
    pub(super) fn who(&self, model: &ModelRef, names: &DisplayNames) -> Who {
        let mut seats = self
            .models
            .iter()
            .filter(|(_, seated)| seated == model)
            .map(|(seat, _)| seat);
        match (seats.next(), seats.next()) {
            (Some(seat), None) => Who::Seat(seat.clone()),
            (Some(seat), Some(_)) => {
                let mut face = self.conversation.faces.face(seat);
                face.number = None;
                Who::Model(face)
            }
            (None, _) => {
                let agent = model.agent.as_str();
                let name = names
                    .model(agent, model.model.as_deref())
                    .unwrap_or_else(|| names.short(model));
                Who::Model(Face::new(
                    name,
                    agent,
                    logo_for(agent, model.model.as_deref()),
                ))
            }
        }
    }
}

/// The centred line under the plan: "Every seat signed both" once every seat backs every point
/// the plan was built from, then the seats that object to ways you asked for, "▐▌ 3.8 Flash
/// objects to a way you asked for; only you change it". None when it would say nothing.
fn under_the_plan(signed: bool, objections: &[WayObjection]) -> Option<Item> {
    let mut line = Vec::new();
    if signed {
        line.push(Piece::dim("Every seat signed both"));
    }
    if let Some(Item::Line(objected)) = objected(objections) {
        if signed {
            line.push(Piece::dim(" · "));
        }
        line.extend(objected);
    }
    (!line.is_empty()).then_some(Item::Line(line))
}

/// The seats that object to ways you asked for: "▐▌ 3.8 Flash objects to a way you asked for;
/// only you change it".
fn objected(objections: &[WayObjection]) -> Option<Item> {
    let mut seats: Vec<&SeatId> = Vec::new();
    for objection in objections {
        if !seats.contains(&&objection.seat) {
            seats.push(&objection.seat);
        }
    }
    let (first, rest) = seats.split_first()?;
    let mut line = Vec::new();
    for (i, seat) in std::iter::once(first).chain(rest).enumerate() {
        if i > 0 {
            line.push(Piece::dim(if i + 1 == seats.len() {
                " and "
            } else {
                ", "
            }));
        }
        line.push(Piece::Called((*seat).clone(), Tone::Dim));
    }
    let ways = match objections.len() {
        1 => "a way you asked for; only you change it".to_owned(),
        n => format!("{n} ways you asked for; only you change them"),
    };
    let objects = if seats.len() == 1 {
        "objects"
    } else {
        "object"
    };
    line.push(Piece::dim(format!(" {objects} to {ways}")));
    Some(Item::Line(line))
}
