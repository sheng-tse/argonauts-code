//! Which question typed answers go to, and what a pick does: sends the answer, asks again before
//! an option that can't be undone, switches Remember this pick, or reads Show the diff out
//! while the question keeps waiting.

use argonauts_core::app::{AnswerTarget, AppEvent, UiCommand};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::ApprovalEvent;

use crate::base::text::sanitize_line;
use crate::frame::state::NeedsYou;

use super::pick::{Pick, Typed};
use super::questions::{ListTarget, QuestionList, Spoken};
use super::reader::{Reader, Said};

/// An option that asks again, picked once. The same option next sends it.
#[derive(Debug)]
pub struct Armed {
    target: ListTarget,
    pick: Pick,
    /// Such as `merge`.
    verb: String,
}

impl Armed {
    fn cancelled(&self) -> String {
        format!("{}: cancelled, nothing was sent.", self.verb)
    }
}

impl Reader {
    /// A line that isn't a command or a mention: an answer to the question that waits, or a
    /// message.
    pub(super) fn plain(&mut self, text: String, said: &mut Said) {
        let question = self.current_question();
        let typed = question
            .as_ref()
            .map_or(Typed::Message, |question| question.parse(&text));
        let armed = match (self.armed.take(), &question, &typed) {
            (Some(armed), Some(question), Typed::Pick(Pick::Option { index, .. }))
                if armed.target == question.target && same_option(&armed.pick, *index) =>
            {
                Some(armed)
            }
            (Some(armed), ..) => {
                said.lines.push(armed.cancelled());
                None
            }
            (None, ..) => None,
        };
        match (question, typed) {
            (Some(question), Typed::Pick(pick)) => match armed {
                Some(armed) => self.send(&question, &with_note(armed.pick, pick), said),
                None => self.pick(question, pick, said),
            },
            (_, Typed::Warning(warning)) => said.lines.push(format!("warning: {warning}")),
            // The plain screen has no key to ignore the word, so the word alone decides.
            _ => said.commands.push(UiCommand::Submit {
                workflow_requested: argonauts_core::look::keywords::asks_for_workflow(&text),
                text,
                pictures: Vec::new(),
            }),
        }
    }

    fn pick(&mut self, question: QuestionList, pick: Pick, said: &mut Said) {
        match pick {
            Pick::OwnAnswer { text: None } => {
                said.lines
                    .push("answer: type your answer and press Enter.".into());
                self.own_answer = Some(question);
            }
            Pick::View { .. } => self.show(&question, &pick, said),
            Pick::Remember => {
                said.lines.push(question.picked(&pick));
                if let ListTarget::Question(target) = &question.target {
                    if question.remember == Some(true) {
                        self.remember.retain(|on| on != target);
                    } else {
                        self.remember.push(target.clone());
                    }
                }
            }
            Pick::Option { index, .. } if question.asks_again(index) => {
                said.lines.push(question.ask_again(index));
                self.armed = Some(Armed {
                    verb: question.verb(index),
                    target: question.target,
                    pick,
                });
            }
            pick => self.send(&question, &pick, said),
        }
    }

    /// Your own answer, typed on the line after its number.
    pub(super) fn own_answer(&mut self, question: QuestionList, line: &str, said: &mut Said) {
        let pick = Pick::OwnAnswer {
            text: Some(line.trim().to_owned()).filter(|text| !text.is_empty()),
        };
        match question.command(&pick) {
            Some(_) => self.send(&question, &pick, said),
            None => self.own_answer = Some(question),
        }
    }

    fn send(&mut self, question: &QuestionList, pick: &Pick, said: &mut Said) {
        let Some(command) = question.command(pick) else {
            return;
        };
        said.lines
            .push(sanitize_line(&question.picked(pick)).into_owned());
        said.commands.push(command);
        match &question.target {
            ListTarget::Question(target) => {
                self.answered.push(target.clone());
                self.remember.retain(|on| on != target);
            }
            ListTarget::SpotCheck { run, id } => self.decisions.checked(run, id),
        }
        if let Some(next) = self.current_question() {
            said.lines.push("still waiting:".to_owned());
            said.lines.extend(next.lines());
            self.question = Some(next.target);
        }
    }

    /// Show the diff, the details, the file, or the evidence. Nothing is answered.
    fn show(&mut self, question: &QuestionList, pick: &Pick, said: &mut Said) {
        said.lines
            .push(sanitize_line(&question.picked(pick)).into_owned());
        match question.view() {
            Spoken::Request(view) => said.commands.push(UiCommand::Request { view }),
            Spoken::Lines(lines) => said.lines.extend(lines),
            Spoken::Warning(text) => said
                .lines
                .push(format!("warning: {}", sanitize_line(&text))),
        }
        said.lines.push(question.how_to_answer());
        self.question = Some(question.target.clone());
    }

    /// Keeps the question that numbers answer: the newest one read out, and when it closes, the
    /// oldest one still waiting, read out again. A spot check waits behind the questions.
    pub(super) fn follow_questions(&mut self, event: &AppEvent, said: &mut Said) {
        let waiting = self.waiting();
        let needs_you = self.state.needs_you();
        self.answered.retain(|target| {
            needs_you
                .iter()
                .any(|question| target_of(question) == *target)
        });
        let opened = opened_question(event).map(ListTarget::Question);
        // Once the question closes or another opens, typing the same key again could answer
        // something you haven't heard ask.
        if let Some(armed) = self
            .armed
            .take_if(|armed| opened.is_some() || !waiting.contains(&armed.target))
        {
            said.lines.push(armed.cancelled());
        }
        if let Some(opened) = opened
            && !self.state.replaying()
            && waiting.contains(&opened)
        {
            self.question = Some(opened);
            return;
        }
        let still = self
            .question
            .as_ref()
            .is_some_and(|question| waiting.contains(question));
        let caught_up = matches!(event, AppEvent::ReplayDone { .. });
        if (still && !caught_up) || self.state.replaying() {
            return;
        }
        self.question = None;
        if let Some(next) = self.current_question() {
            if !queued_spot_check(event, &next.target) {
                said.lines.push("still waiting:".to_owned());
            }
            said.lines.extend(next.lines());
            self.question = Some(next.target);
        }
    }

    /// Questions waiting on you that weren't answered here, oldest first, then spot checks.
    fn waiting(&self) -> Vec<ListTarget> {
        let needs_you = self.state.needs_you();
        let questions = needs_you
            .iter()
            .map(target_of)
            .filter(|target| !self.answered.contains(target))
            .map(ListTarget::Question);
        let spot_checks =
            self.decisions
                .spot_checks()
                .iter()
                .map(|decision| ListTarget::SpotCheck {
                    run: decision.run.clone(),
                    id: decision.opened.id.clone(),
                });
        questions.chain(spot_checks).collect()
    }

    pub(super) fn current_question(&self) -> Option<QuestionList> {
        let waiting = self.waiting();
        let target = self
            .question
            .as_ref()
            .filter(|question| waiting.contains(question))
            .or_else(|| waiting.first())?;
        match target {
            ListTarget::Question(target) => {
                let mut question = QuestionList::for_target(&self.state, target)?;
                if question.remember.is_some() {
                    question.remember = Some(self.remember.contains(target));
                }
                Some(question)
            }
            ListTarget::SpotCheck { run, id } => {
                let decision = self.decisions.spot_check(run, id)?;
                let offset = self.state.clock_offset();
                Some(QuestionList::spot_check(
                    decision,
                    offset,
                    &self.state.names,
                ))
            }
        }
    }
}

fn same_option(armed: &Pick, index: usize) -> bool {
    matches!(armed, Pick::Option { index: at, .. } if *at == index)
}

/// The armed pick, with the note typed when confirming it if there is one.
fn with_note(armed: Pick, confirm: Pick) -> Pick {
    match (armed, confirm) {
        (
            Pick::Option { index, note, tasks },
            Pick::Option {
                note: confirm_note, ..
            },
        ) => Pick::Option {
            index,
            note: confirm_note.or(note),
            tasks,
        },
        (armed, _) => armed,
    }
}

fn target_of(question: &NeedsYou) -> AnswerTarget {
    match question {
        NeedsYou::Approval { run, id, .. } => AnswerTarget::Approval {
            run: run.clone(),
            id: id.clone(),
        },
        NeedsYou::Choice { id, .. } => AnswerTarget::Choice { id: id.clone() },
    }
}

/// The question `event` read out, if it opened one that needs you.
fn opened_question(event: &AppEvent) -> Option<AnswerTarget> {
    match event {
        AppEvent::Choice { choice } => Some(AnswerTarget::Choice {
            id: choice.id.clone(),
        }),
        AppEvent::Run { event } => match &event.kind {
            RunEventKind::Approval(ApprovalEvent::Opened(opened)) if opened.needs_you => {
                Some(AnswerTarget::Approval {
                    run: event.run_id.clone(),
                    id: opened.id.clone(),
                })
            }
            _ => None,
        },
        _ => None,
    }
}

/// Whether `event` queued the spot check `target`, so it's news rather than still waiting.
fn queued_spot_check(event: &AppEvent, target: &ListTarget) -> bool {
    let AppEvent::Run { event } = event else {
        return false;
    };
    matches!(
        (&event.kind, target),
        (
            RunEventKind::Approval(ApprovalEvent::SpotCheckQueued { id }),
            ListTarget::SpotCheck { run, id: queued },
        ) if *run == event.run_id && id == queued
    )
}
