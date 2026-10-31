//! ◆ questions: a run's questions and approvals and the app's own choices, drawn in the live
//! region in place of the input like Claude Code's permission prompt. Each takes the keys the
//! moment it shows. Esc on one a run waits on hands it on, so whoever asked decides and goes
//! on; Esc on a choice you set off yourself closes it. Every answer is bound to
//! the evidence hash you saw.

mod about;
mod approval;
mod choice;
mod cli_down;
mod confirm;
mod item;
mod options;
mod plan_workflow;
mod records;
mod spot;
mod table;
mod views;
mod words;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use argonauts_core::app::{AnswerTarget, AppChoice, AppEvent, ChoiceKind, UiCommand};
use argonauts_core::base::names::ApprovalId;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::approval::{
    AnswerAction, AnswerOption, ApprovalDecided, ApprovalEvent, ApprovalKind, ApprovalOpened,
    ApprovalPacket, PolicyCheck,
};
use argonauts_core::runs::events::run::Party;
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_core::safety::policy::Verdict;
use argonauts_core::{RunId, TaskId};
use time::{OffsetDateTime, UtcOffset};

use crate::app::UiAction;
use crate::base::text::sanitize_line;
use crate::bottom::{AnsweredPick, QuestionPrompt};
use crate::bridge::Cx;
use crate::cells::cell::Cell;
use crate::council::screens::edits::InQuestion;
use crate::frame::state::DecisionRecord;
use crate::frame::view::ViewOutcome;

pub(crate) use about::details;
pub use about::{about_cells, about_cells_local, about_item, with_run_workers};
pub(crate) use approval::checklist_questions;
pub use approval::{REMEMBER_LABEL, approval_question, approval_question_local};
pub use choice::choice_question;
pub use confirm::confirm_take_over;
pub use item::{AboutCell, Block};
pub use records::record_cell;
pub(crate) use spot::spot_check_options;
pub use spot::spot_check_question;
pub use table::{Column, Table, TableCell};
pub(crate) use views::{Shown, approval_view, choice_view};
pub(crate) use words::joined;

/// Auto decisions kept for a spot check that may follow them.
const RECENT_DECISIONS: usize = 32;

/// What a council's plan question says under its title: what approving with edits and sending
/// it back do.
pub const COUNCIL_PLAN: &str = "Approve with edits: change how any task is done, or who does it. Send back: the seats talk on.";

#[derive(Debug, Default)]
pub struct Questions {
    /// Every question on screen, waiting behind it, or passed on, by the kit's question id.
    pub open: BTreeMap<u64, OpenQuestion>,
    next_id: u64,
    /// What each open question was built from, and its prompt while another question is on screen.
    held: BTreeMap<u64, Held>,
    /// Approvals opened and not yet decided or withdrawn, for their record lines.
    approvals: BTreeMap<(RunId, ApprovalId), Box<ApprovalOpened>>,
    /// Recent decisions, newest last, for the spot checks that follow auto decisions.
    decisions: VecDeque<Decision>,
    /// Approvals carried on without you from this screen, whose line printed then, so the
    /// decision that follows prints no second one.
    passed_on: BTreeSet<(RunId, ApprovalId)>,
    /// The question Enter on its run's row brought up, and where the list's selection was:
    /// answering it, or Esc, goes back to that row.
    from_row: Option<(u64, Option<usize>)>,
}

/// What an answer to a question needs to become a `UiCommand`.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenQuestion {
    pub target: QuestionTarget,
    pub options: Vec<AnswerOption>,
    /// From `ApprovalOpened`, sent back unchanged with the answer.
    pub evidence_sha256: Option<String>,
    /// The fallback question's Remember this pick toggle.
    pub remember: bool,
    /// Where a run's question offers Carry on without me, its last row.
    pub carry_on: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum QuestionTarget {
    Controller(AnswerTarget),
    /// A spot check of an auto decision, answered with `UiCommand::SpotCheck`.
    SpotCheck {
        run: RunId,
        id: ApprovalId,
    },
    /// Built here and answered here, such as the confirm before a take-over or a stop.
    Local(LocalConfirm),
}

#[derive(Debug, Clone, PartialEq)]
pub enum LocalConfirm {
    TakeOver {
        run: argonauts_core::RunId,
        task: argonauts_core::TaskId,
        /// The fallback question whose "Take over myself" this confirms. It waits under the
        /// confirm, and the engine answers it with the take-over.
        answering: Option<u64>,
    },
}

#[derive(Debug)]
struct Held {
    source: Source,
    /// The question's prompt while another question is on screen.
    stashed: Option<QuestionPrompt>,
    /// The merge rows Request changes ticks, in the checklist's order.
    checklist: Vec<TaskId>,
    /// The option index of the fallback question's remember toggle.
    toggle: Option<usize>,
}

#[derive(Debug)]
enum Source {
    Approval(Box<ApprovalOpened>),
    SpotCheck(Box<Decision>),
    Choice(Box<AppChoice>),
    Local,
}

#[derive(Debug, Clone)]
struct Decision {
    run: RunId,
    opened: Box<ApprovalOpened>,
    decided: Option<ApprovalDecided>,
    check: Option<PolicyCheck>,
}

impl Questions {
    /// `Approval::Opened`, `Checked`, `SpotCheckQueued`, `Decided`, and `Withdrawn`; `Choice`
    /// and `ChoiceClosed`. Rings the bell for a question that needs you; the bridge keeps replay
    /// quiet.
    pub fn on_event(&mut self, event: &AppEvent, cx: &mut Cx<'_>) {
        match event {
            AppEvent::Run { event } => {
                if let RunEventKind::Approval(approval) = &event.kind {
                    self.on_approval(event, approval, cx);
                }
            }
            AppEvent::Choice { choice } => self.open_choice(choice, cx),
            AppEvent::ChoiceClosed { id, outcome } => {
                let target = QuestionTarget::Controller(AnswerTarget::Choice { id: id.clone() });
                let title = self
                    .close_target(&target, cx)
                    .and_then(|held| match held.source {
                        // A preview's outcome names what it closes; its plan stays above.
                        Source::Choice(choice) if choice.kind == ChoiceKind::TryWorkflow => None,
                        Source::Choice(choice) => Some(choice.title),
                        _ => None,
                    });
                if !cx.history_only() {
                    cx.model
                        .history
                        .push(records::choice_closed_cell(title.as_deref(), outcome));
                }
                self.show(cx);
            }
            AppEvent::Detached { run } => {
                let bottom = &mut cx.model.bottom;
                if bottom
                    .answered_pick
                    .as_ref()
                    .is_some_and(|pick| pick.run == *run)
                {
                    bottom.answered_pick = None;
                }
                let gone: Vec<u64> = self
                    .open
                    .iter()
                    .filter(|(_, question)| question.target.run() == Some(run))
                    .map(|(id, _)| *id)
                    .collect();
                for id in gone {
                    self.close(id, cx);
                }
                self.approvals.retain(|(worker, _), _| worker != run);
                self.show(cx);
            }
            _ => {}
        }
    }

    /// Takes `Answer`, `OpenView`, `Dismiss`, `AnswerQuestion`, and `ConfirmTakeOver`, and sees a
    /// spot check marked in `/approvals` go out.
    pub fn on_action(&mut self, action: &UiAction, cx: &mut Cx<'_>) -> bool {
        match action {
            UiAction::Answer {
                question,
                option,
                note,
                checked,
            } => self.answer(*question, *option, note.clone(), checked, cx),
            UiAction::OpenView { question, option } => self.open_view(*question, *option, cx),
            UiAction::Dismiss { question } => self.dismiss(*question, cx),
            // The run's view opens all the same, on what the question is about.
            UiAction::OpenWorkflow(key) => {
                self.bring_up_for_row(key, cx);
                false
            }
            UiAction::AnswerQuestion(target) => {
                let wanted = QuestionTarget::Controller(target.clone());
                let Some(id) = self.find(&wanted) else {
                    return false;
                };
                self.display(id, true, cx);
                cx.out.close_views = true;
                true
            }
            UiAction::Command(UiCommand::SpotCheck { run, id, .. }) => {
                // Marked in `/approvals`, so its question has nothing left to ask. The command
                // still goes out.
                let target = QuestionTarget::SpotCheck {
                    run: run.clone(),
                    id: id.clone(),
                };
                if self.close_target(&target, cx).is_some() {
                    self.show(cx);
                }
                false
            }
            UiAction::ConfirmTakeOver { run, task } => {
                let agent = worker_name(cx, run, task);
                let id = self.next_question_id();
                let prompt = confirm_take_over(id, run, task, &agent);
                let target = LocalConfirm::TakeOver {
                    run: run.clone(),
                    task: task.clone(),
                    answering: None,
                };
                self.raise_local(id, prompt, target, confirm::take_over_options(), cx);
                true
            }
            _ => false,
        }
    }

    /// Enter on a run's row while the run waits on you: its oldest question that does comes up
    /// with the keys, under the run's view. A row under it, a task's or a seat's, brings up the
    /// question the row waits on, and only that.
    fn bring_up_for_row(&mut self, key: &str, cx: &mut Cx<'_>) {
        let (run, under) = key.split_once('#').unwrap_or((key, ""));
        let Some(run) = RunId::new(run).ok() else {
            return;
        };
        let answers = cx
            .model
            .bottom
            .workflows
            .rows
            .iter()
            .find(|row| row.key == key)
            .and_then(|row| row.answers.clone());
        if !under.is_empty() && answers.is_none() {
            return;
        }
        let oldest = {
            let shown = cx.model.bottom.question.as_ref();
            let waits = |id: u64| match shown.filter(|question| question.id == id) {
                Some(question) => question.needs_you,
                None => self
                    .held
                    .get(&id)
                    .and_then(|held| held.stashed.as_ref())
                    .is_some_and(|question| question.needs_you),
            };
            let asks = |wanted: Option<&ApprovalId>| {
                self.open
                    .iter()
                    .find(|(id, open)| {
                        matches!(&open.target, QuestionTarget::Controller(AnswerTarget::Approval {
                            run: asked, id: approval,
                        }) if *asked == run && wanted.is_none_or(|wanted| wanted == approval))
                            && waits(**id)
                    })
                    .map(|(id, _)| *id)
            };
            match &answers {
                Some(wanted) if under.is_empty() => asks(Some(wanted)).or_else(|| asks(None)),
                Some(wanted) => asks(Some(wanted)),
                None => asks(None),
            }
        };
        let Some(id) = oldest else {
            return;
        };
        let selected = cx.model.bottom.workflows.selected;
        self.display(id, true, cx);
        self.from_row = Some((id, selected));
    }

    /// Once the question its run's row brought up is answered or set back, the keys go back to
    /// that row, and the view opened over it closes.
    fn back_to_row(&mut self, question: u64, cx: &mut Cx<'_>) {
        let Some((_, selected)) = self.from_row.filter(|(asked, _)| *asked == question) else {
            return;
        };
        self.from_row = None;
        cx.out.close_views = true;
        let list = &mut cx.model.bottom.workflows;
        match selected.filter(|at| *at < list.rows.len()) {
            Some(at) => list.selected = Some(at),
            None => list.focus(),
        }
    }

    pub fn next_question_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn on_approval(&mut self, event: &RunEvent, approval: &ApprovalEvent, cx: &mut Cx<'_>) {
        let run = &event.run_id;
        match approval {
            ApprovalEvent::Opened(opened) => self.open_approval(run, opened, cx),
            ApprovalEvent::Checked(check) => self.checked(run, check, cx),
            ApprovalEvent::Decided(decided) => self.decided(run, decided, event.ts, cx),
            ApprovalEvent::Withdrawn { id, reason } => {
                let opened = self.approvals.remove(&(run.clone(), id.clone()));
                let target = approval_target(run, id);
                let held = self.close_target(&target, cx);
                let title = opened
                    .map(|opened| opened.title)
                    .or_else(|| held.map(|held| held.source.title()));
                if let Some(title) = title
                    && !cx.history_only()
                {
                    cx.model
                        .history
                        .push(records::withdrawn_cell(&title, reason));
                }
                self.show(cx);
            }
            ApprovalEvent::SpotCheckQueued { id } => self.spot_check(run, id, cx),
        }
    }

    fn open_approval(&mut self, run: &RunId, opened: &ApprovalOpened, cx: &mut Cx<'_>) {
        let opened = &with_run_workers(opened, cx.state.runs.get(run), &cx.state.names);
        // An approval opened again replaces its question, such as after the evidence changed.
        let target = approval_target(run, &opened.id);
        self.close_target(&target, cx);
        // A council's plan is the note keeper's table in the talk, and Approve with edits opens
        // it as a sheet, so the crew's printed plan doesn't show.
        let council_plan = cx
            .model
            .bottom
            .council
            .edits
            .as_ref()
            .filter(|edits| edits.approval == opened.id)
            .map(|edits| edits.sheet.clone())
            .zip(
                opened
                    .options
                    .iter()
                    .position(|option| option.action == AnswerAction::ApproveWithEdits),
            );
        if !cx.history_only() && council_plan.is_none() {
            let run_state = cx.state.runs.get(run);
            for cell in about_cells_local(opened, run_state, clock_offset(cx), &cx.state.names) {
                cx.model.history.push(cell);
            }
        }
        let id = self.next_question_id();
        let run_state = cx.state.runs.get(run);
        let mut prompt =
            approval_question_local(id, opened, run_state, clock_offset(cx), &cx.state.names);
        if let Some(agent) = &mut prompt.agent {
            agent.color = cx.model.agent_colors.get(&agent.worker);
        }
        if let ApprovalPacket::AgentDown(down) = &opened.packet {
            let wall = argonauts_core::base::clock::now_utc();
            cli_down::count_down(&mut prompt, down, &opened.options, cx.now, wall);
        }
        // A council's plan sets its edits on a sheet in the question's place, not in a note.
        let in_council = council_plan.is_some();
        if let Some((sheet, option)) = council_plan {
            prompt.sheet = Some(Box::new(InQuestion {
                option,
                open: false,
                sheet,
                faces: cx.model.bottom.council.faces.clone(),
            }));
            council_plan_question(&mut prompt, opened);
        }
        let carry_on = !in_council && approval::carries_on(opened);
        self.open.insert(
            id,
            OpenQuestion {
                target,
                options: opened.options.clone(),
                evidence_sha256: Some(opened.evidence_sha256.clone()),
                remember: false,
                carry_on: carry_on.then(|| prompt.options.len() - 1),
            },
        );
        self.held.insert(
            id,
            Held {
                source: Source::Approval(Box::new(opened.clone())),
                stashed: Some(prompt),
                checklist: approval::checklist_questions(opened),
                toggle: approval::remember_toggle(opened),
            },
        );
        self.approvals
            .insert((run.clone(), opened.id.clone()), Box::new(opened.clone()));
        if opened.needs_you {
            cx.out.ring();
        }
        self.show(cx);
    }

    /// An approval your rules couldn't settle now waits for you. In auto its question says why.
    fn checked(&mut self, run: &RunId, check: &PolicyCheck, cx: &mut Cx<'_>) {
        let key = (run.clone(), check.id.clone());
        let why = auto_left_it(check);
        // A check with no verdict is the policy leaving it to you, with or without a reason.
        let waits = !check.auto_preview
            && (!check.violations.is_empty()
                || why.is_some()
                || matches!(
                    check.verdict,
                    None | Some(Verdict::Reject | Verdict::AskYou)
                ));
        if let Some(why) = why {
            self.add_detail(&approval_target(run, &check.id), why, cx);
        }
        let was_quiet = self
            .approvals
            .get(&key)
            .is_some_and(|opened| !opened.needs_you);
        if waits && was_quiet {
            cx.out.ring();
            if let Some(id) = self.find(&approval_target(run, &check.id)) {
                self.needs_you(id, cx);
                self.show(cx);
            }
        }
        if let Some(decision) = self
            .decisions
            .iter_mut()
            .rev()
            .find(|decision| decision.run == *run && decision.opened.id == check.id)
        {
            decision.check = Some(check.clone());
        } else if let Some(opened) = self.approvals.get(&key) {
            self.remember_decision(Decision {
                run: run.clone(),
                opened: opened.clone(),
                decided: None,
                check: Some(check.clone()),
            });
        }
    }

    fn decided(
        &mut self,
        run: &RunId,
        decided: &ApprovalDecided,
        at: OffsetDateTime,
        cx: &mut Cx<'_>,
    ) {
        let opened = self.approvals.remove(&(run.clone(), decided.id.clone()));
        let target = approval_target(run, &decided.id);
        self.close_target(&target, cx);
        // Esc's own line said it already: the question left to the agent prints once.
        let passed_on = self.passed_on.remove(&(run.clone(), decided.id.clone()))
            && decided.by == Party::Declined;
        if let Some(opened) = opened {
            if !cx.history_only() && !passed_on {
                // The screen shows one run at a time, so the record says "the run", and its
                // tasks go by their labels, as the rest of the transcript names them.
                let shown = cx.state.runs.get(run);
                let label = |task: &TaskId| {
                    shown.map_or_else(|| task.to_string(), |shown| shown.task_label(task))
                };
                let record = records::record(
                    &opened,
                    decided,
                    None,
                    Some(at),
                    clock_offset(cx),
                    &cx.state.names,
                    &label,
                );
                cx.model.history.push(Cell::Notice(record));
            }
            let check = self
                .decisions
                .iter()
                .rev()
                .find(|decision| decision.run == *run && decision.opened.id == decided.id)
                .and_then(|decision| decision.check.clone());
            self.decisions
                .retain(|decision| !(decision.run == *run && decision.opened.id == decided.id));
            self.remember_decision(Decision {
                run: run.clone(),
                opened,
                decided: Some(decided.clone()),
                check,
            });
        }
        self.show(cx);
    }

    fn remember_decision(&mut self, decision: Decision) {
        self.decisions.push_back(decision);
        while self.decisions.len() > RECENT_DECISIONS {
            self.decisions.pop_front();
        }
    }

    /// No bell: a spot check is yours to do when you like.
    fn spot_check(&mut self, run: &RunId, id: &ApprovalId, cx: &mut Cx<'_>) {
        let target = QuestionTarget::SpotCheck {
            run: run.clone(),
            id: id.clone(),
        };
        if self.find(&target).is_some() {
            return;
        }
        let Some(decision) = self
            .decisions
            .iter()
            .rev()
            .find(|decision| decision.run == *run && decision.opened.id == *id)
            .cloned()
        else {
            // Decided before this screen attached: the spot check waits in `/approvals`.
            return;
        };
        let question = self.next_question_id();
        let prompt = spot_check_question(
            question,
            &decision.opened,
            decision.decided.as_ref(),
            Some(run),
            &cx.state.names,
        );
        self.open.insert(
            question,
            OpenQuestion {
                target,
                options: spot::spot_check_options(),
                evidence_sha256: Some(decision.opened.evidence_sha256.clone()),
                remember: false,
                carry_on: None,
            },
        );
        self.held.insert(
            question,
            Held {
                source: Source::SpotCheck(Box::new(decision)),
                stashed: Some(prompt),
                checklist: Vec::new(),
                toggle: None,
            },
        );
        self.show(cx);
    }

    fn open_choice(&mut self, choice: &AppChoice, cx: &mut Cx<'_>) {
        let target = QuestionTarget::Controller(AnswerTarget::Choice {
            id: choice.id.clone(),
        });
        self.close_target(&target, cx);
        let id = self.next_question_id();
        self.open.insert(
            id,
            OpenQuestion {
                target,
                options: choice.options.clone(),
                evidence_sha256: None,
                remember: false,
                carry_on: None,
            },
        );
        self.held.insert(
            id,
            Held {
                source: Source::Choice(Box::new(choice.clone())),
                stashed: Some(choice_question(id, choice)),
                checklist: Vec::new(),
                toggle: None,
            },
        );
        if choice.bell {
            cx.out.ring();
        }
        // A safety change you made in a view, such as /approvals, asks here; the view would
        // hide it, unless it stays up over the question, as Config does for the change it sent.
        if choice.kind == ChoiceKind::ConfirmSafetySetting {
            cx.out.views_make_way = true;
        }
        self.show(cx);
    }

    fn raise_local(
        &mut self,
        id: u64,
        prompt: QuestionPrompt,
        confirm: LocalConfirm,
        options: Vec<AnswerOption>,
        cx: &mut Cx<'_>,
    ) {
        self.open.insert(
            id,
            OpenQuestion {
                target: QuestionTarget::Local(confirm),
                options,
                evidence_sha256: None,
                remember: false,
                carry_on: None,
            },
        );
        self.held.insert(
            id,
            Held {
                source: Source::Local,
                stashed: Some(prompt),
                checklist: Vec::new(),
                toggle: None,
            },
        );
        // You asked for it with a key, so it takes the keys at once, over any open view.
        self.display(id, true, cx);
        cx.out.close_views = true;
    }

    fn answer(
        &mut self,
        question: u64,
        option: usize,
        note: Option<String>,
        checked: &[usize],
        cx: &mut Cx<'_>,
    ) -> bool {
        if let Some((run, taken)) = self.hands_over(question, option) {
            let id = self.next_question_id();
            let agent = worker_name(cx, &run, &taken);
            let prompt = confirm_take_over(id, &run, &taken, &agent);
            let target = LocalConfirm::TakeOver {
                run,
                task: taken,
                answering: Some(question),
            };
            self.raise_local(id, prompt, target, confirm::take_over_options(), cx);
            return true;
        }
        let (Some(open), Some(held)) = (self.open.remove(&question), self.held.remove(&question))
        else {
            return false;
        };
        self.back_to_row(question, cx);
        if cx
            .model
            .bottom
            .question
            .as_ref()
            .is_some_and(|shown| shown.id == question)
        {
            cx.model.bottom.question = None;
        }
        let carry_on = approval::carry_on_answer();
        let picked = match open.carry_on {
            Some(row) if row == option => Some(&carry_on),
            _ => open
                .options
                .get(option)
                .filter(|_| held.toggle != Some(option)),
        };
        // Carrying on says at once who decides now, and the decision's own record then
        // doesn't say it again.
        if let (QuestionTarget::Controller(AnswerTarget::Approval { run, id }), Some(picked)) =
            (&open.target, picked)
            && picked.action == AnswerAction::CarryOn
            && let Source::Approval(opened) = &held.source
            && !cx.replaying()
        {
            let at = argonauts_core::base::clock::now_utc();
            let cell =
                records::passed_on_cell(opened.kind, opened.when_declined, at, clock_offset(cx));
            cx.model.history.push(cell);
            self.passed_on.insert((run.clone(), id.clone()));
        }
        cx.model.bottom.answered_pick = picked
            .filter(|_| !open.remember)
            .and_then(|picked| answered_pick(&open.target, &held.source, picked, &cx.state.names));
        match (open.target, picked) {
            (_, None) => {}
            (_, Some(picked)) if picked.action == AnswerAction::ShowDiff => {}
            (QuestionTarget::Controller(to), Some(picked)) => {
                let tasks = if picked.action == AnswerAction::RequestChanges {
                    checked
                        .iter()
                        .filter_map(|index| held.checklist.get(*index).cloned())
                        .collect()
                } else {
                    Vec::new()
                };
                let remember =
                    open.remember && matches!(picked.action, AnswerAction::Candidate { .. });
                cx.out.send(UiCommand::Answer {
                    to,
                    action: picked.action.clone(),
                    text: note,
                    evidence_sha256: open.evidence_sha256,
                    tasks,
                    remember,
                });
            }
            (QuestionTarget::SpotCheck { run, id }, Some(picked)) => {
                cx.out.send(UiCommand::SpotCheck {
                    run,
                    id,
                    correct: picked.action == AnswerAction::Approve,
                });
            }
            (
                QuestionTarget::Local(LocalConfirm::TakeOver {
                    run,
                    task,
                    answering,
                }),
                Some(_),
            ) => {
                if let Some(answered) = answering {
                    self.close(answered, cx);
                }
                cx.out.send(UiCommand::TakeOver { run, task });
            }
        }
        self.show(cx);
        true
    }

    /// "Take over myself" on a fallback question hands the terminal to the agent's own UI, so it
    /// asks first, as a take-over from a session view does.
    fn hands_over(&self, question: u64, option: usize) -> Option<(RunId, TaskId)> {
        let (open, held) = (self.open.get(&question)?, self.held.get(&question)?);
        let picked = open.options.get(option)?;
        let Source::Approval(opened) = &held.source else {
            return None;
        };
        let QuestionTarget::Controller(AnswerTarget::Approval { run, .. }) = &open.target else {
            return None;
        };
        let taken = opened.task.clone()?;
        (picked.action == AnswerAction::TakeOver
            && opened.kind == ApprovalKind::Fallback
            && held.toggle != Some(option))
        .then(|| (run.clone(), taken))
    }

    fn open_view(&mut self, question: u64, option: usize, cx: &mut Cx<'_>) -> bool {
        let (Some(open), Some(held)) = (self.open.get_mut(&question), self.held.get(&question))
        else {
            return false;
        };
        if held.toggle == Some(option) {
            // The question flipped its own toggle; its state is what the answer carries.
            let shown = cx
                .model
                .bottom
                .question
                .as_ref()
                .filter(|shown| shown.id == question)
                .and_then(|shown| shown.options.get(option))
                .map(|toggle| toggle.on);
            open.remember = shown.unwrap_or(!open.remember);
            return true;
        }
        let shown = match (&open.target, &held.source) {
            (
                QuestionTarget::Controller(AnswerTarget::Approval { run, .. }),
                Source::Approval(opened),
            ) => views::approval_view(run, opened, clock_offset(cx), &cx.state.names),
            (_, Source::SpotCheck(decision)) => views::evidence_view(
                &decision.run,
                &decision.opened,
                decision.decided.as_ref(),
                decision.check.as_ref(),
                clock_offset(cx),
                &cx.state.names,
            ),
            // What a repository's files say is the app's to read: it prints each one above the
            // question, which stays open.
            (
                QuestionTarget::Controller(to @ AnswerTarget::Choice { .. }),
                Source::Choice(choice),
            ) if choice.kind == ChoiceKind::TrustProject => {
                cx.out.send(UiCommand::Answer {
                    to: to.clone(),
                    action: AnswerAction::ShowDiff,
                    text: None,
                    evidence_sha256: None,
                    tasks: Vec::new(),
                    remember: false,
                });
                return true;
            }
            (_, Source::Choice(choice)) => views::choice_view(choice),
            _ => return true,
        };
        match shown {
            views::Shown::Request(view) => cx.out.send(UiCommand::Request { view }),
            views::Shown::Pager(pager) => cx.out.open(pager),
            // Why the key did nothing, on the notice line, never in the transcript.
            views::Shown::Notice(text) => crate::bridge::keys::notice(&text, cx),
        }
        true
    }

    /// Esc took the question off the screen with nothing picked. A choice closes as the controller
    /// says for its kind, and a confirm of this screen's own just closes. A run's question or
    /// approval waits out of sight, unanswered, where its run's row and the list of what needs
    /// you still open it. Nothing waits on a spot check, so it closes and stays in `/approvals`.
    fn dismiss(&mut self, question: u64, cx: &mut Cx<'_>) -> bool {
        let Some(open) = self.open.get(&question) else {
            return false;
        };
        match &open.target {
            QuestionTarget::Controller(to @ AnswerTarget::Choice { .. }) => {
                cx.out.send(UiCommand::SetAside { to: to.clone() });
            }
            // Esc on a run's question or approval goes back to the list unanswered: it waits
            // out of sight, its row still marked, and nothing is sent. Carry on without me is
            // its last option.
            QuestionTarget::Controller(AnswerTarget::Approval { .. }) => {
                self.back_to_row(question, cx)
            }
            QuestionTarget::SpotCheck { .. } | QuestionTarget::Local(_) => {
                self.close(question, cx);
            }
        }
        self.show(cx);
        true
    }

    /// A line under a waiting question's details, once, wherever the question is.
    fn add_detail(&mut self, target: &QuestionTarget, line: String, cx: &mut Cx<'_>) {
        let Some(id) = self.find(target) else {
            return;
        };
        let question = match cx.model.bottom.question.as_mut() {
            Some(question) if question.id == id => Some(question),
            _ => self
                .held
                .get_mut(&id)
                .and_then(|held| held.stashed.as_mut()),
        };
        if let Some(question) = question
            && !question.detail.contains(&line)
        {
            question.detail.push(line);
        }
    }

    fn find(&self, target: &QuestionTarget) -> Option<u64> {
        self.open
            .iter()
            .find(|(_, question)| question.target == *target)
            .map(|(id, _)| *id)
    }

    /// Marks question `id` as one work waits on, on screen or set back.
    fn needs_you(&mut self, id: u64, cx: &mut Cx<'_>) {
        let shown = cx
            .model
            .bottom
            .question
            .as_mut()
            .filter(|question| question.id == id);
        let stashed = self
            .held
            .get_mut(&id)
            .and_then(|held| held.stashed.as_mut());
        if let Some(question) = shown.or(stashed) {
            question.needs_you = true;
        }
    }

    fn close_target(&mut self, target: &QuestionTarget, cx: &mut Cx<'_>) -> Option<Held> {
        let id = self.find(target)?;
        self.close(id, cx)
    }

    fn close(&mut self, id: u64, cx: &mut Cx<'_>) -> Option<Held> {
        self.open.remove(&id);
        if cx
            .model
            .bottom
            .question
            .as_ref()
            .is_some_and(|shown| shown.id == id)
        {
            cx.model.bottom.question = None;
        }
        self.held.remove(&id)
    }

    /// Several questions can wait at once. One you set off yourself comes first, since you're
    /// waiting on it. Then the one on screen stays while it holds the keys, then the oldest
    /// question work waits on, then the spot check on screen or the oldest one. A question that
    /// asks nothing, as one your rules still decide or one you passed on, stays off the screen, so
    /// a question on screen always holds the keys.
    fn show(&mut self, cx: &mut Cx<'_>) {
        let shown = cx.model.bottom.question.as_ref();
        let prompt = |id: &u64| match shown.filter(|question| question.id == *id) {
            Some(question) => Some(question),
            None => self.held.get(id).and_then(|held| held.stashed.as_ref()),
        };
        let yours = |id: &u64| {
            prompt(id).is_some_and(|question| question.takes_focus && !question.dismissed)
        };
        let asks = |id: &u64| prompt(id).is_some_and(QuestionPrompt::claims_keys);
        // A spot check asks too, but nothing waits on it, so it holds its place only while
        // you work it.
        let spot_check = |id: &u64| {
            self.held
                .get(id)
                .is_some_and(|held| matches!(held.source, Source::SpotCheck(_)))
        };
        let current = shown
            .map(|question| question.id)
            .filter(|id| self.open.contains_key(id) && asks(id));
        let holding = current.filter(|id| {
            shown.is_some_and(|question| {
                if spot_check(id) {
                    question.engaged
                } else {
                    question.focused || question.needs_you
                }
            })
        });
        let wanted = current
            .filter(|id| yours(id))
            .or_else(|| self.open.keys().find(|id| yours(id)).copied())
            .or(holding)
            .or_else(|| {
                let waits = |id: &&u64| asks(id) && !spot_check(id);
                self.open.keys().find(waits).copied()
            })
            .or(current)
            .or_else(|| self.open.keys().find(|id| asks(id)).copied());
        match wanted {
            Some(id) => self.display(id, false, cx),
            None => self.take_off_screen(cx),
        }
    }

    /// Clears the question's place on screen, keeping the prompt of one that's still open.
    fn take_off_screen(&mut self, cx: &mut Cx<'_>) {
        if let Some(mut question) = cx.model.bottom.question.take()
            && let Some(held) = self.held.get_mut(&question.id)
        {
            // A run's question set back waits again, so it never comes back holding the keys.
            question.focused &= question.takes_focus;
            held.stashed = Some(question);
        }
    }

    /// Puts question `id` on screen, keeping the one it replaces for later. A question you set off
    /// takes the keys whenever it shows, and one work waits on until you hand it on, from
    /// wherever they were; any other question only when `focus` asks.
    fn display(&mut self, id: u64, focus: bool, cx: &mut Cx<'_>) {
        let on_screen = cx
            .model
            .bottom
            .question
            .as_ref()
            .map(|question| question.id);
        if on_screen != Some(id) {
            self.take_off_screen(cx);
            cx.model.bottom.question = self.held.get_mut(&id).and_then(|held| held.stashed.take());
        }
        if focus {
            if let Some(question) = cx.model.bottom.question.as_mut() {
                question.focus();
            }
        } else if cx.model.bottom.hand_keys_to_question(cx.now) {
            // It took the keys from wherever they were, and a view on top would keep them.
            cx.out.close_views = true;
        }
    }
}

/// Why auto left a question to you, as its line: the engine gives the reason only on the check
/// that sends a question opened in auto to you.
fn auto_left_it(check: &PolicyCheck) -> Option<String> {
    if check.auto_preview || check.verdict.is_some() {
        return None;
    }
    let why = check.reasons.first()?;
    let why = sanitize_line(why);
    let why = why.trim();
    (!why.is_empty()).then(|| format!("Auto couldn't decide: {why}"))
}

/// The resume picker says the same.
impl QuestionTarget {
    fn run(&self) -> Option<&RunId> {
        match self {
            QuestionTarget::Controller(AnswerTarget::Approval { run, .. })
            | QuestionTarget::SpotCheck { run, .. }
            | QuestionTarget::Local(LocalConfirm::TakeOver { run, .. }) => Some(run),
            QuestionTarget::Controller(AnswerTarget::Choice { .. }) => None,
        }
    }
}

impl Source {
    fn title(&self) -> String {
        match self {
            Source::Approval(opened) => opened.title.clone(),
            Source::SpotCheck(decision) => decision.opened.title.clone(),
            Source::Choice(choice) => choice.title.clone(),
            Source::Local => String::new(),
        }
    }
}

/// The call a spot check asks about, as its question shows it, once this screen saw it decided.
pub fn spot_check_line(
    run: &RunId,
    record: &DecisionRecord,
    names: &DisplayNames,
) -> Option<String> {
    let decided = record.decided.as_ref()?;
    Some(format!(
        "{} · decided by {}",
        records::outcome(&record.opened, decided, Some(run), names, &records::by_id),
        words::party(&decided.by, names)
    ))
}

/// What `d` opens for a spot check listed in `/approvals`, the same as on its question, with clock
/// times at `offset`.
pub fn spot_check_evidence(
    run: &RunId,
    record: &DecisionRecord,
    offset: UtcOffset,
    names: &DisplayNames,
) -> ViewOutcome {
    let shown = views::evidence_view(
        run,
        &record.opened,
        record.decided.as_ref(),
        record.check.as_ref(),
        offset,
        names,
    );
    match shown {
        views::Shown::Request(view) => ViewOutcome::Request(view),
        views::Shown::Pager(pager) => ViewOutcome::Open(Box::new(pager)),
        views::Shown::Notice(text) => ViewOutcome::Notice(text),
    }
}

/// A fallback question answered with an agent, offered as a rule until you send something else.
fn answered_pick(
    target: &QuestionTarget,
    source: &Source,
    picked: &AnswerOption,
    names: &DisplayNames,
) -> Option<AnsweredPick> {
    let QuestionTarget::Controller(AnswerTarget::Approval { run, id }) = target else {
        return None;
    };
    let Source::Approval(opened) = source else {
        return None;
    };
    if opened.kind != ApprovalKind::Fallback
        || !matches!(picked.action, AnswerAction::Candidate { .. })
    {
        return None;
    }
    let decided = ApprovalDecided {
        id: id.clone(),
        action: picked.action.clone(),
        text: None,
        by: Party::User,
        evidence_sha256: opened.evidence_sha256.clone(),
        auto_preview: None,
        tasks: Vec::new(),
    };
    let outcome = records::outcome(opened, &decided, Some(run), names, &records::by_id);
    Some(AnsweredPick::new(run.clone(), id.clone(), outcome))
}

/// A council's plan question as the council draws it: under the talk, whose plan says what the
/// tasks are, above the council's input. Esc sets it aside, and the seats' plan never goes ahead
/// without you.
fn council_plan_question(prompt: &mut QuestionPrompt, opened: &ApprovalOpened) {
    prompt.above_input = true;
    if approval::carries_on(opened) {
        prompt.options.pop();
    }
    match prompt.detail.first_mut() {
        Some(summary) => COUNCIL_PLAN.clone_into(summary),
        None => prompt.detail.push(COUNCIL_PLAN.to_owned()),
    }
    prompt.esc_hint = "Esc to set aside".to_owned();
    prompt.offers_shift_tab = false;
}

fn approval_target(run: &RunId, id: &ApprovalId) -> QuestionTarget {
    QuestionTarget::Controller(AnswerTarget::Approval {
        run: run.clone(),
        id: id.clone(),
    })
}

fn worker_name(cx: &Cx<'_>, run: &RunId, task: &TaskId) -> String {
    let state = cx.state.runs.get(run);
    let worker = state
        .and_then(|run| run.tasks.get(task))
        .and_then(|row| row.worker.clone())
        .or_else(|| {
            state?
                .sessions_in_order()
                .filter(|session| session.task.as_ref() == Some(task))
                .last()
                .map(|session| session.worker.clone())
        });
    worker.map_or_else(
        || "its agent".to_owned(),
        |worker| cx.state.names.worker(&worker),
    )
}

/// The zone question times are shown in, read at startup like the bottom line's. UTC when unknown.
fn clock_offset(cx: &Cx<'_>) -> UtcOffset {
    cx.state.clock_offset()
}

#[cfg(test)]
mod tests;
