//! Questions as numbered lists: what a question is about, its options, and how to answer it, and
//! the answer a pick comes to. Every answer carries the evidence hash of the question that was read
//! out. How typed lines become picks is in `pick`.

use argonauts_core::RunId;
use argonauts_core::TaskId;
use argonauts_core::app::{AnswerTarget, AppChoice, UiCommand, ViewRequest};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::approval::{
    AnswerAction, AnswerOption, ApprovalOpened, ApprovalPacket, FreeText,
};
use argonauts_core::names::ApprovalId;
use time::UtcOffset;

use crate::bottom::{OptionKind, QuestionOption};
use crate::questions::{
    REMEMBER_LABEL, Shown, approval_question_local, approval_view, checklist_questions,
    choice_question, choice_view, spot_check_options, spot_check_question,
};
use crate::state::{AppState, RunState};
use crate::text::sanitize_line;
use crate::views::pager::PagerContent;

use super::about::{about_lines, details_lines, evidence_lines};
use super::decisions::Decision;
use super::pick::Pick;
use super::words::spoken;

/// What typed answers go to.
#[derive(Debug, Clone, PartialEq)]
pub enum ListTarget {
    Question(AnswerTarget),
    /// A spot check of an auto decision, answered with `UiCommand::SpotCheck`.
    SpotCheck {
        run: RunId,
        id: ApprovalId,
    },
}

/// A question waiting on you, read as a numbered list.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionList {
    pub target: ListTarget,
    /// From the question's `ApprovalOpened`; `None` for app choices and spot checks.
    pub evidence: Option<String>,
    pub title: String,
    pub body: Vec<String>,
    pub options: Vec<AnswerOption>,
    pub free_text: FreeText,
    /// Options that ask again before they're sent, as the screen arms them.
    pub irreversible: Vec<usize>,
    /// The merge rows Request changes can name. Naming none sends every task back.
    pub checklist: Vec<TaskId>,
    /// The fallback question's Remember this pick and whether it's on; `None` on other questions.
    pub remember: Option<bool>,
    source: Source,
    /// The terminal's zone, for clock times.
    offset: UtcOffset,
    /// What to call each agent.
    names: DisplayNames,
}

/// What Show the diff, the details, or the evidence reads from.
#[derive(Debug, Clone, PartialEq)]
enum Source {
    Approval(RunId, Box<ApprovalOpened>),
    Choice(Box<AppChoice>),
    SpotCheck(Box<Decision>),
}

/// What Show the diff comes to: a view to ask the app for, or lines to read now.
#[derive(Debug, PartialEq)]
pub enum Spoken {
    Request(ViewRequest),
    Lines(Vec<String>),
    Warning(String),
}

impl QuestionList {
    /// Clock times read at `offset`, the terminal's zone.
    pub fn approval(
        run: &RunId,
        opened: &ApprovalOpened,
        state: Option<&RunState>,
        offset: UtcOffset,
        names: &DisplayNames,
    ) -> Self {
        let prompt = approval_question_local(0, opened, state, offset, names);
        let mut body = about_lines(opened, offset, names);
        body.extend(prompt.detail);
        body.extend(prompt.body);
        body.extend(prompt.question);
        let remember = prompt
            .options
            .iter()
            .any(|option| option.kind == OptionKind::Toggle)
            .then_some(false);
        Self {
            target: ListTarget::Question(AnswerTarget::Approval {
                run: run.clone(),
                id: opened.id.clone(),
            }),
            evidence: Some(opened.evidence_sha256.clone()),
            title: prompt.title.clone(),
            body: clean(body),
            options: opened.options.clone(),
            free_text: opened.free_text,
            irreversible: irreversible(&prompt.options),
            checklist: checklist_questions(opened),
            remember,
            source: Source::Approval(run.clone(), Box::new(opened.clone())),
            offset,
            names: names.clone(),
        }
    }

    pub fn choice(choice: &AppChoice) -> Self {
        Self {
            target: ListTarget::Question(AnswerTarget::Choice {
                id: choice.id.clone(),
            }),
            evidence: None,
            title: choice.title.clone(),
            body: clean(choice.body.clone()),
            options: choice.options.clone(),
            free_text: choice.free_text,
            irreversible: irreversible(&choice_question(0, choice).options),
            checklist: Vec::new(),
            remember: None,
            source: Source::Choice(Box::new(choice.clone())),
            offset: UtcOffset::UTC,
            names: DisplayNames::default(),
        }
    }

    /// Clock times read at `offset`, the terminal's zone.
    pub fn spot_check(decision: &Decision, offset: UtcOffset, names: &DisplayNames) -> Self {
        let prompt = spot_check_question(
            0,
            &decision.opened,
            decision.decided.as_ref(),
            Some(&decision.run),
            names,
        );
        let mut body = prompt.detail;
        body.extend(prompt.question);
        Self {
            target: ListTarget::SpotCheck {
                run: decision.run.clone(),
                id: decision.opened.id.clone(),
            },
            evidence: None,
            title: decision.opened.title.clone(),
            body: clean(body),
            options: spot_check_options(),
            free_text: FreeText::None,
            irreversible: Vec::new(),
            checklist: Vec::new(),
            remember: None,
            source: Source::SpotCheck(Box::new(decision.clone())),
            offset,
            names: names.clone(),
        }
    }

    /// The question `target` names, while it's still open.
    pub fn for_target(state: &AppState, target: &AnswerTarget) -> Option<Self> {
        match target {
            AnswerTarget::Approval { run, id } => {
                let run_state = state.runs.get(run)?;
                let open = run_state.approval(id)?;
                Some(Self::approval(
                    run,
                    &open.opened,
                    Some(run_state),
                    state.clock_offset(),
                    &state.names,
                ))
            }
            AnswerTarget::Choice { id } => {
                let choice = state.choices.iter().find(|choice| choice.id == *id)?;
                Some(Self::choice(choice))
            }
        }
    }

    /// The number of Remember this pick, after the question's options.
    pub fn remember_number(&self) -> Option<usize> {
        self.remember.map(|_| self.options.len() + 1)
    }

    /// The number of Type my own answer: the question's own option for it, or the next number
    /// after the rest.
    pub fn own_answer_number(&self) -> Option<usize> {
        if self.free_text != FreeText::Answer {
            return None;
        }
        let after = self.remember_number().unwrap_or(self.options.len());
        Some(self.own_answer_option().unwrap_or(after) + 1)
    }

    /// A free-text question: its only answer is yours, so it reads as an answer to type, as
    /// the question shows its answer field open, rather than a list of one.
    pub fn is_answer_field(&self) -> bool {
        let own = match self.options.as_slice() {
            [] => self.free_text == FreeText::Answer,
            [only] => only.action == AnswerAction::Answer,
            _ => false,
        };
        own && self.remember.is_none()
    }

    pub(super) fn own_answer_option(&self) -> Option<usize> {
        self.options
            .iter()
            .position(|option| option.action == AnswerAction::Answer)
    }

    pub(super) fn takes_note(&self) -> bool {
        matches!(self.free_text, FreeText::Guidance | FreeText::Edits)
    }

    /// Request changes, when it can name some of the merge's tasks.
    pub(super) fn request_changes(&self) -> Option<usize> {
        if self.checklist.is_empty() {
            return None;
        }
        self.options
            .iter()
            .position(|option| option.action == AnswerAction::RequestChanges)
    }

    /// The title, what the question is about, its numbered options, and how to answer.
    pub fn lines(&self) -> Vec<String> {
        let label = match self.target {
            ListTarget::Question(_) => "needs you",
            ListTarget::SpotCheck { .. } => "spot check",
        };
        let mut lines = vec![format!("{label}: {}", spoken(&self.title))];
        lines.extend(self.body.iter().map(|line| format!("  {line}")));
        if self.is_answer_field() {
            lines.push(self.how_to_answer());
            return lines;
        }
        for (n, option) in self.options.iter().enumerate() {
            let mut text = format!("  {}. {}", n + 1, spoken(&option.label));
            if option.recommended {
                text.push_str(", recommended");
            }
            if let Some(detail) = option.detail.as_deref().map(spoken)
                && !detail.is_empty()
            {
                text.push_str(&format!(", {detail}"));
            }
            lines.push(text);
        }
        if let (Some(n), Some(on)) = (self.remember_number(), self.remember) {
            lines.push(format!("  {n}. {REMEMBER_LABEL}, {}", on_off(on)));
        }
        if let Some(own) = self.own_answer_number()
            && self.own_answer_option().is_none()
        {
            lines.push(format!("  {own}. Type my own answer"));
        }
        lines.push(self.how_to_answer());
        lines
    }

    pub fn how_to_answer(&self) -> String {
        if self.is_answer_field() {
            return "answer: type 1: and then your answer, or 1 alone to type it on the next \
                    line."
                .to_owned();
        }
        let mut text = String::from("answer: type a number.");
        if self.takes_note() {
            text.push_str(" To add a note, type it after a colon, as in 1: the note.");
        }
        if let Some(own) = self.own_answer_number() {
            text.push_str(&format!(
                " For your own answer, type {own}: and then the answer."
            ));
        }
        if let (Some(index), Some(first)) = (self.request_changes(), self.checklist.first()) {
            let n = index + 1;
            text.push_str(&format!(
                " To send back only some tasks, name them after {n}, as in {n} {first}: what to change."
            ));
        }
        text
    }

    /// Whether picking `index` asks again first.
    pub fn asks_again(&self, index: usize) -> bool {
        self.irreversible.contains(&index)
    }

    /// What picking `index` does, as in `merge`.
    pub fn verb(&self, index: usize) -> String {
        let label = self
            .options
            .get(index)
            .map(|option| spoken(&option.label))
            .unwrap_or_default();
        lowercase_first(&label)
    }

    /// Asks you to pick `index` once more, as in `merge: type 1 again to merge.`
    pub fn ask_again(&self, index: usize) -> String {
        let verb = self.verb(index);
        format!("{verb}: type {} again to {verb}.", index + 1)
    }

    /// The answer to send, or `None` for a pick that sends nothing: your own answer without
    /// its text, Remember this pick, or Show the diff.
    pub fn command(&self, pick: &Pick) -> Option<UiCommand> {
        match (&self.target, pick) {
            (ListTarget::SpotCheck { run, id }, Pick::Option { index, .. }) => {
                let action = &self.options.get(*index)?.action;
                (*action != AnswerAction::ShowDiff).then(|| UiCommand::SpotCheck {
                    run: run.clone(),
                    id: id.clone(),
                    correct: *action == AnswerAction::Approve,
                })
            }
            (ListTarget::Question(to), Pick::Option { index, note, tasks }) => {
                let action = self.options.get(*index)?.action.clone();
                if action == AnswerAction::ShowDiff {
                    return None;
                }
                let remember =
                    self.remember == Some(true) && matches!(action, AnswerAction::Candidate { .. });
                Some(UiCommand::Answer {
                    to: to.clone(),
                    action,
                    text: note.clone(),
                    evidence_sha256: self.evidence.clone(),
                    tasks: tasks.clone(),
                    remember,
                })
            }
            (ListTarget::Question(to), Pick::OwnAnswer { text: Some(text) }) => {
                Some(UiCommand::Answer {
                    to: to.clone(),
                    action: AnswerAction::Answer,
                    text: Some(text.clone()),
                    evidence_sha256: self.evidence.clone(),
                    tasks: Vec::new(),
                    remember: false,
                })
            }
            _ => None,
        }
    }

    /// What you picked, as the line that confirms it.
    pub fn picked(&self, pick: &Pick) -> String {
        let label = |index: usize| {
            self.options
                .get(index)
                .map(|option| spoken(&option.label))
                .unwrap_or_default()
        };
        match pick {
            Pick::Option { index, note, tasks } => {
                let mut text = format!("you: {}. {}", index + 1, label(*index));
                let names: Vec<String> = tasks.iter().map(ToString::to_string).collect();
                if !names.is_empty() {
                    text.push_str(&format!(", {}", names.join(", ")));
                } else if self.request_changes() == Some(*index) {
                    text.push_str(", every task");
                }
                if let Some(note) = note {
                    text.push_str(&format!(", {}", spoken(note)));
                }
                text
            }
            Pick::View { index } => format!("you: {}. {}", index + 1, label(*index)),
            Pick::Remember => format!(
                "{}: {}",
                lowercase_first(REMEMBER_LABEL),
                on_off(!self.remember.unwrap_or(false))
            ),
            Pick::OwnAnswer { text } => {
                format!("you: {}", spoken(text.as_deref().unwrap_or_default()))
            }
        }
    }

    /// What Show the diff, the details, the file, or the evidence reads out. The question stays.
    pub fn view(&self) -> Spoken {
        match &self.source {
            Source::Approval(run, opened) => {
                match approval_view(run, opened, self.offset, &self.names) {
                    Shown::Request(view) => Spoken::Request(view),
                    Shown::Notice(text) => Spoken::Warning(text),
                    // A crew merge's combined diff reads as it is. Anything else is the details,
                    // read from their blocks so tables read by header.
                    Shown::Pager(pager) => match (&opened.packet, pager.content) {
                        (ApprovalPacket::Merge(_), PagerContent::Text(lines)) => {
                            Spoken::Lines(titled(&pager.title, as_drawn(&lines)))
                        }
                        _ => Spoken::Lines(titled(
                            &opened.title,
                            clean(details_lines(opened, self.offset, &self.names)),
                        )),
                    },
                }
            }
            Source::Choice(choice) => match choice_view(choice) {
                Shown::Pager(pager) => match pager.content {
                    PagerContent::Text(lines) => {
                        Spoken::Lines(titled(&pager.title, as_drawn(&lines)))
                    }
                    _ => Spoken::Lines(Vec::new()),
                },
                Shown::Request(view) => Spoken::Request(view),
                Shown::Notice(text) => Spoken::Warning(text),
            },
            Source::SpotCheck(decision) => Spoken::Lines(titled(
                &decision.opened.title,
                clean(evidence_lines(decision, self.offset, &self.names)),
            )),
        }
    }
}

/// `details:` and the title, then the lines under it.
fn titled(title: &str, lines: Vec<String>) -> Vec<String> {
    let mut spoken_lines = vec![format!("details: {}", spoken(title))];
    spoken_lines.extend(lines.into_iter().map(|line| format!("  {line}")));
    spoken_lines
}

/// Lines such as a diff's keep their leading spaces.
fn as_drawn(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| sanitize_line(line).trim_end().to_owned())
        .filter(|line| !line.trim().is_empty())
        .collect()
}

fn irreversible(options: &[QuestionOption]) -> Vec<usize> {
    options
        .iter()
        .enumerate()
        .filter(|(_, option)| option.kind == OptionKind::Irreversible)
        .map(|(index, _)| index)
        .collect()
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

fn clean(lines: Vec<String>) -> Vec<String> {
    lines
        .iter()
        .flat_map(|line| line.lines().map(spoken).collect::<Vec<_>>())
        .filter(|line| !line.is_empty())
        .collect()
}
