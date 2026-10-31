//! Council's talk into the conversation: each council event of a run becomes the items it adds,
//! printed as cells. The newest message stays live, so the tapbacks and readers that follow it
//! show on it, and prints once the next item lands; the typing row stays live above the
//! working line while a seat writes. Once the seats work, each task's box stays live with its
//! progress until the task ends.

mod checked;
mod plan;
mod session;
mod work;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use argonauts_core::agents::cli_sessions::ResumeCommand;
use argonauts_core::app::{AppEvent, UiCommand, ViewRequest};
use argonauts_core::base::names::{ApprovalId, MessageId, ModelRef, SeatId, SessionKey};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::{ApprovalEvent, ApprovalPacket, PlanPacket};
use argonauts_core::runs::events::council::{
    CheckPick, CouncilEvent, CouncilEventKind, Seating, Speaker,
};
use argonauts_core::runs::events::session::SessionEvent;
use argonauts_core::runs::events::task::ReviewEvent;
use argonauts_core::runs::events::task::{TaskEvent, TaskSpec, TaskState, Way};
use argonauts_core::workflows::workflow::StepRef;
use argonauts_core::{AgentEvent, RunId, TaskId};
use time::OffsetDateTime;

use super::Cx;
use crate::app::UiAction;
use crate::base::format::WallClock;
use crate::cells::cell::Cell;
use crate::cells::cell::history::CellId;
use crate::council::screens::edits::{EditSheet, PlanEdits};
use crate::council::screens::pick::{PickKey, Picker, pick_keys, starts_picking};
use crate::council::screens::session_view::SeatSession;
use crate::council::screens::views::{SeatView, ThreadView};
use crate::council::talk::conversation::ToolUse;
use crate::council::work::plan::PlanTask;
use crate::council::work::task_header::TaskHeader;
use crate::council::{
    Conversation, Face, Faces, Item, Look, SharedItem, Spacing, TalkCell, TypingCell, When,
};
use crate::frame::state::RunState;
use crate::views::waiting::{self, RunEntry};

/// Every council the runs on this screen hold, by its talk step.
#[derive(Debug, Default)]
pub struct CouncilBridge {
    talks: Vec<Talk>,
    /// Each seat's session, so its tool calls count on its first answer.
    seats: HashMap<SessionKey, SeatId>,
    /// The terminal draws pictures under its text, so `ui.bubbles = "auto"` draws bubbles.
    /// Set where the screen learns what the terminal draws.
    pub under_text: bool,
    /// The message or task picked with shift+↑, while one is.
    picking: Option<Picker<Pick>>,
}

/// What shift+↑ can pick.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pick {
    Message(MessageId),
    Task(TaskId),
}

#[derive(Debug)]
struct Talk {
    step: StepRef,
    run: RunId,
    conversation: Conversation,
    look: Look,
    spacing: Spacing,
    /// The newest message, still drawn live.
    live: Option<(MessageId, CellId, SharedItem)>,
    typing: Option<CellId>,
    /// The model each seat runs, to find the seat a task's worker is.
    models: Vec<(SeatId, ModelRef)>,
    /// The tasks the seats work on, in the order they were planned.
    planned: Vec<Planned>,
    /// Every message and task you can pick, in the order they came.
    order: Vec<Pick>,
    /// The working line gave its tip for the talk.
    tipped: bool,
    /// The plan's tasks as the note keeper's who-does-what last showed them.
    shown_plan: Option<Vec<PlanTask>>,
    /// The plan the run asks you to approve, until you answer.
    asked: Option<(ApprovalId, PlanPacket)>,
    /// The checker code picked for each task, until its review lands.
    checks: Vec<CheckPick>,
    /// A check's lines have said what a check reads.
    told_what_checks_read: bool,
    /// How each of the run's sessions carries on in its CLI's own screen, as its adapter said.
    carried: HashMap<SessionKey, ResumeCommand>,
}

/// A planned task with a way, and its box while its seat works on it.
#[derive(Debug)]
struct Planned {
    run: RunId,
    task: TaskId,
    title: String,
    way: Way,
    /// Found from the task's worker, or from the first seat's session on it.
    seat: Option<SeatId>,
    live: Option<(CellId, SharedItem)>,
    /// Its task ended and the box printed as it stood.
    printed: bool,
}

impl CouncilBridge {
    pub fn on_event(&mut self, event: &AppEvent, cx: &mut Cx<'_>) {
        let AppEvent::Run { event } = event else {
            return;
        };
        match &event.kind {
            RunEventKind::Council(council) => {
                self.on_council(council, &event.run_id, event.ts, cx);
                // The picked one may have printed, or the picker's words changed with it.
                if self.picking.is_some() {
                    self.show_picked(cx);
                }
            }
            RunEventKind::Session(SessionEvent::Started(started)) => {
                if let (Some(seat), Some(session)) = (&started.seat, &event.session) {
                    self.seats.insert(session.clone(), seat.clone());
                    if let (Some(task), Some(talk)) = (&event.task_id, self.talks.last_mut()) {
                        talk.seat_found(task, seat);
                    }
                }
            }
            RunEventKind::Session(SessionEvent::CarryOn(command)) => {
                if let (Some(session), Some(talk)) = (
                    &event.session,
                    self.talks
                        .iter_mut()
                        .rev()
                        .find(|talk| talk.run == event.run_id),
                ) {
                    talk.carried.insert(session.clone(), command.clone());
                }
            }
            RunEventKind::Agent(AgentEvent::ToolStarted { name, .. }) => {
                let seat = event.session.as_ref().and_then(|key| self.seats.get(key));
                if let (Some(seat), Some(talk)) = (seat, self.talks.last_mut()) {
                    talk.conversation.tool_used(seat, ToolUse::of(name));
                }
            }
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) => {
                if let ApprovalPacket::Plan(plan) = &opened.packet
                    && let Some(talk) = self
                        .talks
                        .iter_mut()
                        .rev()
                        .find(|talk| talk.run == event.run_id)
                {
                    talk.show_plan(plan, cx);
                    talk.asked = Some((opened.id.clone(), (**plan).clone()));
                    // Its question opens Approve with edits as this sheet.
                    let sheet = self.edit_sheet(&event.run_id, plan, &cx.state.names);
                    cx.model.bottom.council.edits = sheet.map(|sheet| PlanEdits {
                        approval: opened.id.clone(),
                        sheet,
                    });
                }
            }
            RunEventKind::Approval(ApprovalEvent::Decided(decided)) => {
                if let Some(talk) = self
                    .talks
                    .iter_mut()
                    .rev()
                    .find(|talk| talk.run == event.run_id)
                {
                    talk.decided(decided, event.ts, cx);
                }
            }
            RunEventKind::Review(ReviewEvent::Reviewed { review, .. }) => {
                if let (Some(task), Some(talk)) = (
                    &event.task_id,
                    self.talks
                        .iter_mut()
                        .rev()
                        .find(|talk| talk.run == event.run_id),
                ) {
                    talk.reviewed(task, review.verdict, cx);
                }
            }
            RunEventKind::Task(TaskEvent::Created(spec)) => {
                if let (Some(way), Some(talk)) = (&spec.way, self.talks.last_mut()) {
                    talk.plan(&event.run_id, spec, way.how.clone());
                }
            }
            _ => {}
        }
        let ended = match &event.kind {
            RunEventKind::Task(TaskEvent::State { to, .. }) if is_over(*to) => {
                event.task_id.clone()
            }
            _ => None,
        };
        // Streamed words change no box, and come many times a second.
        let streamed = matches!(
            &event.kind,
            RunEventKind::Agent(AgentEvent::TextDelta { .. } | AgentEvent::Reasoning { .. })
        );
        let seats = &self.seats;
        for talk in self.talks.iter_mut().filter(|_| !streamed) {
            talk.show_tasks(&event.run_id, ended.as_ref(), seats, cx);
        }
        if self.picking.is_some() {
            self.show_picked(cx);
        }
        // A finished run's council takes no more messages, though a resumed one talks on.
        cx.model.bottom.council.talking = self.talks.last().is_some_and(|talk| {
            talk.conversation.talking()
                && cx
                    .state
                    .runs
                    .get(&talk.run)
                    .is_none_or(|run| run.finished.is_none())
        });
    }

    /// The council talking in `step`, if this screen has seen it sit down.
    pub fn conversation(&self, step: &StepRef) -> Option<&Conversation> {
        self.talks
            .iter()
            .find(|talk| talk.step == *step)
            .map(|talk| &talk.conversation)
    }

    /// The council that sat down last.
    pub fn newest(&self) -> Option<&Conversation> {
        self.talks.last().map(|talk| &talk.conversation)
    }

    /// Approve with edits for the plan `run`'s council asks you to approve: the plan's tasks
    /// with a way, the ways the engine offers, and the seats a task can go to.
    /// [`crate::council::screens::edits::note`] turns what you set into the approval's edit lines.
    pub fn edit_sheet(
        &self,
        run: &RunId,
        plan: &PlanPacket,
        names: &DisplayNames,
    ) -> Option<EditSheet> {
        let talk = self.talks.iter().rev().find(|talk| talk.run == *run)?;
        let offer = plan.offer.as_ref()?;
        let tasks = talk.edit_tasks(plan, names);
        (!tasks.is_empty()).then(|| EditSheet::new(tasks, offer, talk.models.clone()))
    }

    /// The top of the workflow view for a council's task that a workflow does: the workflow
    /// to open on, whose task it is in which council, and the way's own line.
    pub fn task_header(&self, run: &RunState, task: &TaskId) -> Option<TaskHeader> {
        let planned = self
            .talks
            .iter()
            .filter(|talk| talk.run == run.id)
            .flat_map(|talk| &talk.planned)
            .find(|planned| planned.task == *task)?;
        work::header(
            run,
            (&planned.task, &planned.title),
            &planned.way,
            planned.seat.as_ref()?,
        )
    }

    /// Takes `CouncilKey`: shift+↑ picks the newest message or task of the council that sat
    /// down last, ↑ and ↓ pick another, `r` replies to it, `t` opens its thread, and Esc goes
    /// back to the input.
    pub fn on_action(&mut self, action: &UiAction, cx: &mut Cx<'_>) -> bool {
        if let UiAction::OpenAgent(agent) = action {
            return self.open_seat(agent, cx);
        }
        let UiAction::CouncilKey(key) = action else {
            return false;
        };
        let Some(talk) = self.talks.last() else {
            self.picking = None;
            self.show_picked(cx);
            return true;
        };
        let conversation = &talk.conversation;
        let did = match &mut self.picking {
            Some(picker) => picker.key(*key),
            None if starts_picking(key) => {
                self.picking = Picker::start(talk.newest_first());
                PickKey::Moved
            }
            None => PickKey::Ignored,
        };
        match did {
            PickKey::Moved | PickKey::Ignored => {}
            PickKey::Reply(Pick::Message(id)) => {
                cx.model.bottom.replying = conversation.reply_bar(&id);
                self.picking = None;
            }
            PickKey::Thread(Pick::Message(id)) => {
                if let Some(root) = conversation.thread_root(&id)
                    && let Some(thread) = conversation.thread(&root)
                {
                    let faces = conversation.faces.clone();
                    cx.out
                        .views
                        .push(Box::new(ThreadView::new(thread, faces, root)));
                    self.picking = None;
                }
            }
            PickKey::Open(Pick::Message(id)) => {
                if let Some((view, target)) = talk.seat_view(&id, &self.seats, cx) {
                    let faces = conversation.faces.clone();
                    let key = talk.key_of(&target.0);
                    cx.out
                        .open(SeatView::new(view, faces, Some(target)).asking_key(key));
                    self.picking = None;
                }
            }
            PickKey::Open(Pick::Task(task)) => {
                if let Some(planned) = talk.planned.iter().find(|planned| planned.task == task) {
                    planned.open(cx);
                    self.picking = None;
                }
            }
            PickKey::Reply(Pick::Task(_)) | PickKey::Thread(Pick::Task(_)) => {}
            PickKey::Back => self.picking = None,
        }
        self.show_picked(cx);
        true
    }

    /// The working line while the run of the council that sat down last is the work on
    /// screen: "Parleying" with its token limit and how many seats agree while the seats talk,
    /// and the points still open under it, then "Rowing" with the tasks done once they work.
    /// The talk's first line gets its tip.
    pub fn dress_working_line(&mut self, cx: &mut Cx<'_>) {
        let Some(talk) = self.talks.last_mut() else {
            return;
        };
        let Some(run) = cx.state.runs.get(&talk.run) else {
            return;
        };
        let bottom = &mut cx.model.bottom;
        let ours = bottom.synced.work.as_deref() == Some(format!("run:{}", talk.run).as_str());
        let Some(line) = bottom.working.as_mut().filter(|_| ours) else {
            return;
        };
        if talk.planned.is_empty() {
            line.verb = crate::council::TALKING.to_owned();
            line.details = vec![talk.conversation.agree_note()];
            line.tokens_limit = talk.conversation.token_limit;
            line.note = talk.conversation.still_open_line();
        } else {
            let (done, total) = run.tasks_done();
            line.verb = crate::council::WORKING.to_owned();
            line.details = vec![format!("{done}/{total} tasks")];
            line.tokens_limit = None;
            line.note = None;
        }
        if !std::mem::replace(&mut talk.tipped, true) {
            line.tip = cx.settings.tips.then_some(crate::council::TIP);
        }
    }

    /// Enter on a seat's row opens the seat's own session view, where what you write goes to
    /// it alone. False for any other row.
    fn open_seat(&self, agent: &crate::bottom::AgentRef, cx: &mut Cx<'_>) -> bool {
        let Some((run, session)) = super::keys::find_agent(cx.state, agent) else {
            return false;
        };
        let Some(seat) = self.seats.get(&session.key) else {
            return self.open_check(&run, &session.key, cx);
        };
        let Some(talk) = self.talks.iter().rev().find(|talk| talk.run == run) else {
            return false;
        };
        let Some((view, target)) = talk.seat_view_of(seat, &self.seats, cx) else {
            return false;
        };
        let faces = talk.conversation.faces.clone();
        let key = talk.key_of(seat);
        cx.out
            .open(SeatView::new(view, faces, Some(target)).asking_key(key));
        true
    }

    /// Enter on the row of a session that checks a council's task opens that check's own
    /// session view. False for a session that checks no council's task.
    fn open_check(&self, run: &RunId, key: &SessionKey, cx: &mut Cx<'_>) -> bool {
        let Some(view) = self
            .talks
            .iter()
            .rev()
            .filter(|talk| talk.run == *run)
            .find_map(|talk| talk.check_view(key, cx))
        else {
            return false;
        };
        cx.out.open(SeatView::new(view.0, view.1, None));
        true
    }

    /// Rings the picked message or task where it still draws, and puts the picker's keys on
    /// the bottom line.
    fn show_picked(&mut self, cx: &mut Cx<'_>) {
        if let (Some(picker), Some(talk)) = (&mut self.picking, self.talks.last()) {
            picker.refresh(talk.newest_first());
        }
        let picked = self.picking.as_ref().map(Picker::picked);
        let mut keys = None;
        for talk in &mut self.talks {
            if let Some((id, _, shared)) = &talk.live
                && let Ok(mut item) = shared.lock()
            {
                item.set_picked(picked == Some(&Pick::Message(id.clone())));
            }
            for planned in &talk.planned {
                if let Some((_, shared)) = &planned.live
                    && let Ok(mut item) = shared.lock()
                {
                    let lit = picked == Some(&Pick::Task(planned.task.clone()));
                    item.set_picked(lit);
                    item.set_hint(lit.then(|| "enter to open".to_owned()));
                }
            }
            match picked {
                Some(Pick::Message(id)) if talk.conversation.item(id).is_some() => {
                    let thread = talk
                        .conversation
                        .thread_root(id)
                        .and_then(|root| talk.conversation.thread(&root))
                        .is_some_and(|thread| thread.replies > 0);
                    let opens = talk.seat_view(id, &self.seats, cx).map(|_| "its session");
                    keys = Some(pick_keys(opens, thread));
                }
                Some(Pick::Task(task)) => {
                    if let Some(planned) = talk.planned.iter().find(|planned| planned.task == *task)
                    {
                        keys = Some(task_keys(&planned.way));
                    }
                }
                _ => {}
            }
        }
        cx.model.bottom.council.picking = keys;
        if cx.model.bottom.council.picking.is_none() {
            self.picking = None;
        }
        cx.model.bottom.council.picked = match self.picking.as_ref().map(Picker::picked) {
            Some(Pick::Message(id)) => Some(id.clone()),
            _ => None,
        };
    }

    fn on_council(
        &mut self,
        council: &CouncilEvent,
        run: &RunId,
        ts: OffsetDateTime,
        cx: &mut Cx<'_>,
    ) {
        if let CouncilEventKind::Seated(seating) = &council.kind {
            cx.model.bottom.council.sits = true;
            let look = Look::choose(
                cx.settings.council_view,
                cx.settings.bubbles,
                self.under_text,
            );
            let faces = faces(seating, &cx.state.names);
            cx.model.bottom.council.faces = faces.clone();
            self.talks.retain(|talk| talk.step != council.talk);
            self.talks.push(Talk {
                step: council.talk.clone(),
                run: run.clone(),
                conversation: Conversation::new(seating, faces),
                look,
                spacing: Spacing::default(),
                live: None,
                typing: None,
                models: seating
                    .seats
                    .iter()
                    .map(|seat| (seat.id.clone(), seat.model.clone()))
                    .collect(),
                planned: Vec::new(),
                order: Vec::new(),
                tipped: false,
                shown_plan: None,
                asked: None,
                checks: Vec::new(),
                told_what_checks_read: false,
                carried: HashMap::new(),
            });
        }
        let Some(talk) = self.talks.iter_mut().find(|talk| talk.step == council.talk) else {
            return;
        };
        if let CouncilEventKind::Check(pick) = &council.kind {
            talk.picked_check(pick);
        }
        if let CouncilEventKind::Message(message) = &council.kind
            && message.from == Speaker::You
            && !talk.conversation.is_private(&message.id)
        {
            let words = message.words(|seat| talk.conversation.faces.face(seat).label());
            drop_your_echo(&words, cx);
        }
        let said = talk.conversation.on(&council.kind, Some(&when(ts)));
        if let Some(changed) = &said.changed
            && let Some((live, _, shared)) = &talk.live
            && live == changed
            && let Some(item) = talk.conversation.item(changed)
            && let Ok(mut shown) = shared.lock()
        {
            *shown = item;
        }
        let message = match &council.kind {
            CouncilEventKind::Message(message) => Some(message.id.clone()),
            _ => None,
        };
        for item in said.items {
            talk.add(item, message.clone(), cx);
        }
        talk.show_typing(cx);
    }
}

impl Talk {
    /// What a seat's session view asks for its key: the letters `seat` reads the council's
    /// seats by.
    fn key_of(&self, seat: &SeatId) -> ViewRequest {
        ViewRequest::SeatKey {
            run: self.run.clone(),
            talk: self.step.clone(),
            seats: self.models.iter().map(|(seat, _)| seat.clone()).collect(),
            reader: seat.clone(),
        }
    }

    /// Adds `item` under what came before; a message's stays live until the next lands.
    fn add(&mut self, item: Item, message: Option<MessageId>, cx: &mut Cx<'_>) {
        self.settle(cx);
        let faces = self.conversation.faces.clone();
        match message {
            Some(id) => {
                let shared = Arc::new(Mutex::new(item));
                let cell = TalkCell::shared(shared.clone(), self.look, faces, &mut self.spacing)
                    .for_message(id.clone());
                let cell_id = cx.model.history.push_active(Cell::Custom(Box::new(cell)));
                self.order.push(Pick::Message(id.clone()));
                self.live = Some((id, cell_id, shared));
            }
            None => {
                let cell = TalkCell::new(item, self.look, faces, &mut self.spacing);
                cx.model.history.push(Cell::Custom(Box::new(cell)));
            }
        }
    }

    /// Prints the live message as it stands, and takes the typing row away until it is drawn
    /// again under whatever comes next.
    fn settle(&mut self, cx: &mut Cx<'_>) {
        if let Some(id) = self.typing.take() {
            cx.model.history.remove_active(id);
        }
        if let Some((_, id, _)) = self.live.take() {
            cx.model.history.finish_active(id);
        }
    }

    /// The session view of the seat that wrote message `id`, with where a private message to
    /// it goes, while the seat has a session in the run.
    fn seat_view(
        &self,
        id: &MessageId,
        seats: &HashMap<SessionKey, SeatId>,
        cx: &Cx<'_>,
    ) -> Option<(SeatSession, (SeatId, RunId, SessionKey))> {
        let seat = self.conversation.seat_of(id)?;
        self.seat_view_of(&seat, seats, cx)
    }

    /// The session view of `seat`, while it has a session in the run.
    fn seat_view_of(
        &self,
        seat: &SeatId,
        seats: &HashMap<SessionKey, SeatId>,
        cx: &Cx<'_>,
    ) -> Option<(SeatSession, (SeatId, RunId, SessionKey))> {
        let run = cx.state.runs.get(&self.run)?;
        let order: Vec<SeatId> = self.models.iter().map(|(seat, _)| seat.clone()).collect();
        let at = session::Sitting {
            seat,
            conversation: &self.conversation,
            seats: &order,
            names: &cx.state.names,
            carried: &self.carried,
        };
        session::seat_session(run, |key| seats.get(key) == Some(seat), &at)
    }

    /// Every message and task you can pick, newest first.
    fn newest_first(&self) -> Vec<Pick> {
        self.order.iter().rev().cloned().collect()
    }

    /// A task was planned with `way`: its box shows once its seat is known, from its worker
    /// when one seat runs that model, or else from the first seat's session on it.
    fn plan(&mut self, run: &RunId, spec: &TaskSpec, way: Way) {
        let mut seats = self
            .models
            .iter()
            .filter(|(_, model)| spec.worker.as_ref() == Some(model));
        let seat = match (seats.next(), seats.next()) {
            (Some((seat, _)), None) => Some(seat.clone()),
            _ => None,
        };
        self.planned.retain(|planned| planned.task != spec.id);
        self.planned.push(Planned {
            run: run.clone(),
            task: spec.id.clone(),
            title: spec.title.clone(),
            way,
            seat,
            live: None,
            printed: false,
        });
    }

    fn seat_found(&mut self, task: &TaskId, seat: &SeatId) {
        if let Some(planned) = self
            .planned
            .iter_mut()
            .find(|planned| planned.task == *task && planned.seat.is_none())
        {
            planned.seat = Some(seat.clone());
        }
    }

    /// Each task box of `run` whose seat is known shows, drawn from the run as it stands; the
    /// task that `ended` prints as it ended.
    fn show_tasks(
        &mut self,
        run: &RunId,
        ended: Option<&TaskId>,
        seats: &HashMap<SessionKey, SeatId>,
        cx: &mut Cx<'_>,
    ) {
        let Some(state) = cx.state.runs.get(run) else {
            return;
        };
        let at = work::Seats {
            sessions: seats,
            models: &self.models,
        };
        let mut new = Vec::new();
        for planned in self
            .planned
            .iter_mut()
            .filter(|planned| planned.run == *run)
        {
            let Some(seat) = planned.seat.clone() else {
                continue;
            };
            let drawn = work::drawn(
                state,
                (&planned.task, &planned.title),
                &planned.way,
                &seat,
                &at,
            );
            match &planned.live {
                Some((_, shared)) => {
                    if let Ok(mut item) = shared.lock() {
                        item.set_work(drawn);
                    }
                }
                None if planned.printed => {}
                None => new.push((planned.task.clone(), seat, drawn)),
            }
        }
        for (task, seat, drawn) in new {
            self.settle(cx);
            let name = self.conversation.faces.face(&seat).label();
            if let Some(line) = self
                .planned
                .iter()
                .find(|planned| planned.task == task)
                .and_then(|planned| work::started(&seat, &name, &planned.way))
            {
                let cell = TalkCell::new(
                    line,
                    self.look,
                    self.conversation.faces.clone(),
                    &mut self.spacing,
                );
                cx.model.history.push(Cell::Custom(Box::new(cell)));
            }
            let shared = Arc::new(Mutex::new(Item::Seat(vec![drawn.message(seat, false)])));
            let faces = self.conversation.faces.clone();
            let cell = TalkCell::shared(shared.clone(), self.look, faces, &mut self.spacing);
            let id = cx.model.history.push_active(Cell::Custom(Box::new(cell)));
            self.order.push(Pick::Task(task.clone()));
            if let Some(planned) = self.planned.iter_mut().find(|planned| planned.task == task) {
                planned.live = Some((id, shared));
            }
        }
        if let Some(task) = ended
            && let Some(planned) = self
                .planned
                .iter_mut()
                .find(|planned| planned.task == *task)
            && let Some((id, _)) = planned.live.take()
        {
            planned.printed = true;
            cx.model.history.finish_active(id);
        }
    }

    fn show_typing(&mut self, cx: &mut Cx<'_>) {
        let (writers, reason) = self.conversation.typing();
        if let Some(id) = self.typing.take() {
            cx.model.history.remove_active(id);
        }
        if writers.is_empty() {
            return;
        }
        let cell = TypingCell {
            writers,
            reason,
            look: self.look,
            faces: self.conversation.faces.clone(),
        };
        self.typing = Some(cx.model.history.push_active(Cell::Custom(Box::new(cell))));
    }
}

/// A task in one of these states is done with, so its box prints as it stands.
fn is_over(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Merged | TaskState::Failed | TaskState::Cancelled | TaskState::TakenOver
    )
}

/// The bottom line's keys while a task is picked: Enter opens what does the task.
pub(crate) fn task_keys(way: &Way) -> String {
    let opens = match way {
        Way::Agents { .. } => "its agents",
        Way::Workflow { .. } | Way::CliWorkflow => "its workflow",
        Way::OwnSession => "its session",
    };
    format!("↑ ↓  to pick another · enter to open {opens} · esc to go back to the input")
}

impl Planned {
    /// What does the task, in its own view: the run on the task's workflow or with its agents,
    /// or the seat's session.
    fn open(&self, cx: &mut Cx<'_>) {
        let run = cx.state.runs.get(&self.run);
        let session = run.and_then(|run| {
            run.sessions_in_order()
                .filter(|session| session.task.as_ref() == Some(&self.task))
                .last()
                .map(|session| session.key.clone())
        });
        match (&self.way, session) {
            (Way::OwnSession, Some(session)) => {
                cx.out.open(crate::views::session::SessionView::new(
                    self.run.clone(),
                    session,
                ));
            }
            (Way::Workflow { .. }, _)
                if waiting::workflow_entry(cx.state, &self.run) == RunEntry::Workflow =>
            {
                // The run's view opens on the workflow the task's sessions work in, as it
                // does from that workflow's own row.
                let header = run.zip(self.seat.as_ref()).and_then(|(run, seat)| {
                    work::header(run, (&self.task, &self.title), &self.way, seat)
                });
                if let Some(header) = header {
                    cx.model.bottom.workflows.opening =
                        Some(format!("{}#{}", self.run, header.workflow));
                    cx.model.bottom.council.opening = Some(header);
                }
                cx.out.send(UiCommand::Request {
                    view: ViewRequest::Workflow {
                        run: self.run.clone(),
                        workflow: None,
                    },
                });
            }
            _ => super::keys::open_row(self.run.as_str(), cx),
        }
    }
}

/// Your bubble says what you sent, so the grey `❯` echo of the same words, still waiting for a
/// turn that a council never starts, goes.
fn drop_your_echo(words: &str, cx: &mut Cx<'_>) {
    let same = |text: &str| text.split_whitespace().eq(words.split_whitespace());
    let echo = cx
        .model
        .history
        .active_cells()
        .find_map(|(id, cell)| match cell {
            Cell::User(user)
                if user.pending && same(user.full.as_deref().unwrap_or(&user.text)) =>
            {
                Some(id)
            }
            _ => None,
        });
    if let Some(id) = echo {
        cx.model.history.remove_active(id);
    }
}

/// "Today" or the date, and the time on the wall clock.
fn when(ts: OffsetDateTime) -> When {
    let clock = WallClock::local();
    let at = clock.to_local(ts);
    let day = if at.date() == clock.now().date() {
        "Today".to_owned()
    } else {
        format!("{} {}", at.month(), at.day())
    };
    When {
        day,
        time: clock.clock(ts).to_string(),
    }
}

/// Each seat by its model's name, the logo of whoever serves it, and its number when its model
/// sits more than once.
fn faces(seating: &Seating, names: &DisplayNames) -> Faces {
    Faces::new(
        seating
            .seats
            .iter()
            .map(|seat| (seat.id.clone(), Face::of_seat(seat, names))),
    )
}

#[cfg(test)]
pub(crate) mod tests;
