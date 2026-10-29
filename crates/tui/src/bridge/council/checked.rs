//! The lines your approval of the council's plan and each check of its tasks add to the talk:
//! what you approved, and what each fresh session that checked a task without knowing who made
//! it found.

use argonauts_core::TaskId;
use argonauts_core::display_names::{DisplayNames, maker_of};
use argonauts_core::events::approval::{AnswerAction, ApprovalDecided, PlanPacket};
use argonauts_core::events::council::{CheckPick, CheckRung};
use argonauts_core::events::run::Party;
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::steps::judgment::ReviewVerdict;
use time::OffsetDateTime;

use super::session::Checking;
use super::{Cx, Talk, when};
use crate::council::Faces;
use crate::council::checks::{self, Approved, Checked, Checker};
use crate::council::plan::Who;
use crate::council::session_view::SeatSession;

impl Talk {
    /// Your answer to the plan the run asked you to approve: once you approve it, the line that
    /// sums it up at the time you did.
    pub(super) fn decided(
        &mut self,
        decided: &ApprovalDecided,
        ts: OffsetDateTime,
        cx: &mut Cx<'_>,
    ) {
        let Some(plan) = self
            .asked
            .take_if(|(asked, _)| *asked == decided.id)
            .map(|(_, plan)| plan)
        else {
            return;
        };
        let approved = matches!(
            decided.action,
            AnswerAction::Approve | AnswerAction::ApproveWithEdits
        );
        if decided.by != Party::User || !approved {
            return;
        }
        // Edits may have changed the ways a seat objected to, so only a plain approval says
        // which of yours stay.
        let kept = match decided.action {
            AnswerAction::Approve => kept(&plan),
            _ => Vec::new(),
        };
        let line = checks::approved(&Approved {
            at: when(ts).time,
            tasks: plan.graph.tasks.len(),
            checker: self.one_checker(&plan, &cx.state.names),
            kept,
        });
        self.add(line, None, cx);
    }

    /// The checker code picked for a task, kept until its review lands.
    pub(super) fn picked_check(&mut self, pick: &CheckPick) {
        self.checks.retain(|kept| kept.task != pick.task);
        self.checks.push(pick.clone());
    }

    /// A review of `task` by the checker code picked for it: the lines of what it found.
    pub(super) fn reviewed(&mut self, task: &TaskId, verdict: ReviewVerdict, cx: &mut Cx<'_>) {
        let Some(pick) = self.checks.iter().find(|pick| pick.task == *task).cloned() else {
            return;
        };
        // You check it yourself: your own question shows it.
        let Some(model) = &pick.checker else {
            return;
        };
        let names = &cx.state.names;
        let title = self
            .planned
            .iter()
            .find(|planned| planned.task == *task)
            .map(|planned| planned.title.clone())
            .or_else(|| {
                let run = cx.state.runs.get(&self.run)?;
                run.tasks.get(task).map(|row| row.title.clone())
            })
            .unwrap_or_else(|| task.to_string());
        // A check of the change's own maker is a fresh session of the model that made it.
        let maker = self.whole_name(model, names);
        let checker = match (pick.rung, self.who(model, names)) {
            // A fresh session of the model that made the change, which no seat's name fits.
            (CheckRung::SameMaker, Who::Seat(seat)) => {
                let mut face = self.conversation.faces.face(&seat);
                face.number = None;
                Checker::Model(face)
            }
            (_, Who::Model(face)) => Checker::Model(face),
            (_, Who::Seat(seat)) => Checker::Seat(seat),
        };
        let lines = checks::checked(&Checked {
            checker,
            task: title,
            rung: pick.rung,
            verdict,
            maker,
            first: !self.told_what_checks_read,
        });
        if pick.rung != CheckRung::SameMaker {
            self.told_what_checks_read = true;
        }
        for line in lines {
            self.add(line, None, cx);
        }
    }

    /// The view of the session `key` where it checks one of this council's tasks: whose fresh
    /// session it is, the task, and, for you alone, the seat that made the change.
    pub(super) fn check_view(&self, key: &SessionKey, cx: &Cx<'_>) -> Option<(SeatSession, Faces)> {
        let run = cx.state.runs.get(&self.run)?;
        let session = run.sessions.get(key)?;
        let task = session.task.as_ref()?;
        if session.role != RoleName::Reviewer {
            return None;
        }
        let pick = self.checks.iter().find(|pick| pick.task == *task)?;
        let checker = pick.checker.as_ref()?;
        let names = &cx.state.names;
        let face = match self.who(checker, names) {
            Who::Seat(seat) => {
                let mut face = self.conversation.faces.face(&seat);
                face.number = None;
                face
            }
            Who::Model(face) => face,
        };
        let planned = self.planned.iter().find(|planned| planned.task == *task)?;
        let maker = planned
            .seat
            .clone()
            .map(|seat| (seat, pick.rung == CheckRung::SameMaker));
        let at = Checking {
            checker: &face,
            task: &planned.title,
            maker,
            names,
            carried: self.carried.get(key),
        };
        let view = super::session::check_session(session, &at);
        Some((view, self.conversation.faces.clone()))
    }

    /// The model whose fresh sessions check every task of `plan`, by its whole name, and
    /// whether it is the maker of every task's work too. `None` while the checks are different.
    fn one_checker(&self, plan: &PlanPacket, names: &DisplayNames) -> Option<(String, bool)> {
        let mut checkers = plan.graph.tasks.iter().map(|spec| spec.reviewers.first());
        let first = checkers.next()??;
        if !checkers.all(|other| other == Some(first)) {
            return None;
        }
        let maker = |model: &ModelRef| {
            maker_of(model.model.as_deref().unwrap_or(model.agent.as_str())).map(|maker| maker.id)
        };
        let same = maker(first).is_some()
            && plan
                .graph
                .tasks
                .iter()
                .all(|spec| spec.worker.as_ref().map(maker) == Some(maker(first)));
        Some((self.whole_name(first, names), same))
    }

    /// A model's whole name, as a sentence writes it: its seat's, where one seat runs it.
    fn whole_name(&self, model: &ModelRef, names: &DisplayNames) -> String {
        match self.who(model, names) {
            Who::Seat(seat) => self.conversation.faces.face(&seat).name,
            Who::Model(face) => face.name,
        }
    }
}

/// The tasks whose way you asked for, which a seat objected to and which stay as you asked.
fn kept(plan: &PlanPacket) -> Vec<String> {
    plan.graph
        .tasks
        .iter()
        .filter(|spec| spec.way.as_ref().is_some_and(|way| way.asked_by_you))
        .filter(|spec| {
            plan.objections
                .iter()
                .any(|objection| objection.task == spec.id)
        })
        .map(|spec| spec.title.clone())
        .collect()
}
