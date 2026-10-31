//! Approve with edits: the plan's who-how-why as a sheet. ↑ ↓ picks a task, ← → changes its way
//! among the ways the setup allows, tab gives it to the next seat, Enter approves with what you
//! set, which binds as what you asked in your message does. A seat's objection stays beside its
//! task with its reason; only Send back puts a way to the seats again.
//!
//! What you set goes back to the plan's approval as edit lines, one per change:
//! `task-002 way maker-checker`, `task-002 worker codex/gpt-5.6-luna`.

use argonauts_core::TaskId;
use argonauts_core::base::names::{ApprovalId, ModelRef, SeatId};
use argonauts_core::runs::events::approval::WaysOffer;
use argonauts_core::runs::events::task::Way;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::{Line, Span};

use super::super::talk::faces::Faces;
use super::super::talk::row::Row;
use super::super::talk::words::{Piece, Tone};
use super::super::work::plan::{Who, objection, padded, way_word};
use crate::base::theme::Theme;

/// One task as the sheet lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditTask {
    pub task: TaskId,
    pub name: String,
    pub who: Who,
    /// The model that does it, which giving it to another seat changes.
    pub worker: Option<ModelRef>,
    pub way: Way,
    pub why: Vec<Piece>,
    pub objection: Option<(SeatId, String)>,
    /// The way the note keeper had, kept once you change it.
    pub proposed: Way,
    /// Who the note keeper had.
    pub proposed_who: Who,
}

impl EditTask {
    pub fn changed(&self) -> bool {
        self.way != self.proposed || self.who != self.proposed_who
    }
}

/// What a key did on the sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditKey {
    /// Nothing more to do than draw it again.
    Moved,
    /// Approve the plan with these tasks' ways and seats.
    Approve(Vec<EditTask>),
    /// Back to the question, nothing changed.
    Back,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditSheet {
    pub tasks: Vec<EditTask>,
    pub picked: usize,
    /// The ways the setup allows, in its order.
    pub ways: Vec<Way>,
    /// What the setup leaves out and where to change it, as the line under the ways reads.
    pub not_offered: Option<String>,
    /// The seats a task can go to, in seat order, with the model each runs.
    pub seats: Vec<(SeatId, ModelRef)>,
}

/// Approve with edits for the plan a council's approval asks about, which that approval's
/// question opens in its own place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEdits {
    pub approval: ApprovalId,
    pub sheet: EditSheet,
}

/// The sheet as one option of a question opens it, in the question's place, until it answers or
/// goes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InQuestion {
    /// The option it answers with: Approve with edits.
    pub option: usize,
    /// Up in place of the question.
    pub open: bool,
    pub sheet: EditSheet,
    pub faces: Faces,
}

/// What a key did to a question's sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SheetKey {
    /// Draw it again.
    Moved,
    /// Answer with the sheet's option and these edit lines, none when nothing changed.
    Answer(Option<String>),
}

impl InQuestion {
    pub fn key(&mut self, key: KeyEvent) -> SheetKey {
        match self.sheet.key(key) {
            EditKey::Approve(tasks) => {
                self.open = false;
                let lines = note(&tasks);
                SheetKey::Answer((!lines.is_empty()).then_some(lines))
            }
            EditKey::Back => {
                self.open = false;
                SheetKey::Moved
            }
            EditKey::Moved | EditKey::Ignored => SheetKey::Moved,
        }
    }

    pub fn lines(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        self.sheet.lines(width, &self.faces, theme)
    }
}

const TASK: usize = 15;
const WHO: usize = 22;
const HOW: usize = 20;

impl EditSheet {
    /// The sheet for `tasks`, offering the ways the engine's `offer` holds and the `seats` that
    /// sit.
    pub fn new(tasks: Vec<EditTask>, offer: &WaysOffer, seats: Vec<(SeatId, ModelRef)>) -> Self {
        Self {
            tasks,
            picked: 0,
            ways: offer.ways.clone(),
            not_offered: offer.not_offered.clone(),
            seats,
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> EditKey {
        match key.code {
            KeyCode::Up => {
                self.picked = self.picked.saturating_sub(1);
                EditKey::Moved
            }
            KeyCode::Down => {
                self.picked = (self.picked + 1).min(self.tasks.len().saturating_sub(1));
                EditKey::Moved
            }
            KeyCode::Left => self.step_way(-1),
            KeyCode::Right => self.step_way(1),
            KeyCode::Tab => self.next_seat(),
            KeyCode::Enter => EditKey::Approve(self.tasks.clone()),
            KeyCode::Esc => EditKey::Back,
            _ => EditKey::Ignored,
        }
    }

    /// The picked task goes to the next seat, round from the last to the first. A task two
    /// seats of its model could do starts at the first seat.
    fn next_seat(&mut self) -> EditKey {
        let Some(task) = self.tasks.get_mut(self.picked) else {
            return EditKey::Ignored;
        };
        if self.seats.is_empty() {
            return EditKey::Ignored;
        }
        let at = match &task.who {
            Who::Seat(seat) => self.seats.iter().position(|(sits, _)| sits == seat),
            Who::Model(_) => None,
        };
        let next = at.map_or(0, |at| (at + 1) % self.seats.len());
        let (seat, model) = self.seats[next].clone();
        task.who = Who::Seat(seat);
        task.worker = Some(model);
        EditKey::Moved
    }

    fn step_way(&mut self, by: isize) -> EditKey {
        let Some(task) = self.tasks.get_mut(self.picked) else {
            return EditKey::Ignored;
        };
        if self.ways.is_empty() {
            return EditKey::Ignored;
        }
        let at = self
            .ways
            .iter()
            .position(|way| *way == task.way)
            .unwrap_or(0);
        let count = self.ways.len() as isize;
        let next = (at as isize + by).rem_euclid(count) as usize;
        task.way = self.ways[next].clone();
        EditKey::Moved
    }

    pub fn lines(&self, width: usize, faces: &Faces, theme: &Theme) -> Vec<Line<'static>> {
        let dim = Tone::Dim.style(theme);
        let mut rows = vec![
            Row::new()
                .with(0, Span::styled("◆", Tone::Warn.style(theme)))
                .with(
                    2,
                    Span::styled("Approve with edits", Tone::Bold.style(theme)),
                ),
            Row::new().with(
                2,
                Span::styled(
                    "Change how each task is done, or who does it. What you set here binds, as \
                     what you asked in your message does.",
                    dim,
                ),
            ),
            Row::new(),
        ];
        let mut head = Row::new();
        head.pieces(
            4,
            &[
                Piece::dim(padded("Task", TASK)),
                Piece::dim(padded("Who", WHO)),
                Piece::dim(padded("How", HOW)),
                Piece::dim("Why"),
            ],
            faces,
            theme,
        );
        rows.push(head);
        for (i, task) in self.tasks.iter().enumerate() {
            let picked = i == self.picked;
            let mut row = Row::new();
            if picked {
                row.put(2, Span::styled("❯", Tone::Pointer.style(theme)));
            }
            let mut pieces = vec![Piece::tone(
                padded(&task.name, TASK),
                if picked { Tone::Text } else { Tone::Dim },
            )];
            pieces.extend(task.who.pieces(WHO - 3, picked, faces, theme));
            let way = way_word(&task.way);
            if picked {
                let cells = format!("‹ {way} ›").chars().count();
                pieces.push(Piece::tone("‹ ", Tone::Pointer));
                pieces.push(Piece::tone(way, Tone::Bold));
                pieces.push(Piece::tone(" ›", Tone::Pointer));
                pieces.push(Piece::text(" ".repeat(HOW.saturating_sub(cells))));
            } else {
                pieces.push(Piece::text(padded(&way, HOW)));
            }
            if task.way != task.proposed {
                pieces.push(Piece::tone("You changed it", Tone::Yours));
                pieces.push(Piece::dim(format!(
                    " · the note keeper had {}",
                    way_word(&task.proposed)
                )));
            } else if task.who != task.proposed_who {
                let had = match &task.proposed_who {
                    Who::Seat(seat) => faces.face(seat).label(),
                    Who::Model(face) => face.label(),
                };
                pieces.push(Piece::tone("You changed who", Tone::Yours));
                pieces.push(Piece::dim(format!(" · the note keeper had {had}")));
            } else {
                pieces.extend(task.why.iter().cloned());
            }
            row.pieces(4, &pieces, faces, theme);
            rows.push(row);
            if let Some((seat, reason)) = &task.objection {
                let mut row = Row::new();
                let mut pieces = objection(seat, reason, "");
                if task.way == task.proposed {
                    pieces.push(Piece::dim(" · yours stands"));
                }
                row.pieces(4 + TASK, &pieces, faces, theme);
                rows.push(row);
            }
        }
        rows.push(Row::new());
        if let Some(task) = self.tasks.get(self.picked) {
            let mut pieces = vec![
                Piece::text(format!("Ways for {}", task.name)),
                Piece::text("   "),
                Piece::dim("← →   "),
            ];
            for (i, way) in self.ways.iter().enumerate() {
                if i > 0 {
                    pieces.push(Piece::text("   "));
                }
                let tone = if *way == task.way {
                    Tone::PointerBold
                } else {
                    Tone::Dim
                };
                pieces.push(Piece::tone(way_word(way), tone));
            }
            let mut row = Row::new();
            row.pieces(4, &pieces, faces, theme);
            rows.push(row);
        }
        if let Some(not_offered) = &self.not_offered {
            rows.push(Row::new().with(4, Span::styled(not_offered.clone(), dim)));
        }
        rows.push(Row::new());
        rows.push(Row::new().with(
            2,
            Span::styled(
                "enter to approve · ↑ ↓  to pick a task · ← →  to change its way · tab to change \
                 who · esc to go back",
                dim,
            ),
        ));
        rows.into_iter().map(|row| row.line(width)).collect()
    }
}

/// Your edits as the plan's approval reads them: a line for each way you changed and for each
/// task you gave to another seat.
pub fn note(tasks: &[EditTask]) -> String {
    let mut lines = Vec::new();
    for task in tasks {
        if task.way != task.proposed {
            lines.push(format!("{} way {}", task.task, edit_words(&task.way)));
        }
        if task.who != task.proposed_who
            && let Some(worker) = &task.worker
        {
            lines.push(format!("{} worker {worker}", task.task));
        }
    }
    lines.join("\n")
}

/// A way as an edit line writes it: "own session", a workflow's name, "3 agents".
fn edit_words(way: &Way) -> String {
    match way {
        Way::OwnSession => "own session".to_owned(),
        Way::Workflow { name } => name.as_str().to_owned(),
        Way::CliWorkflow => "its cli's own".to_owned(),
        Way::Agents { count } => format!("{count} agents"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn workflow(name: &str) -> Way {
        Way::Workflow {
            name: argonauts_core::workflows::workflow::WorkflowRef::new(name),
        }
    }

    fn seat(n: u32) -> SeatId {
        SeatId::numbered(n)
    }

    fn sheet() -> EditSheet {
        let task = EditTask {
            task: TaskId::numbered(3),
            name: "Memory store".to_owned(),
            who: Who::Seat(seat(4)),
            worker: Some("opencode/qwen3-coder-30b".parse().unwrap()),
            way: Way::OwnSession,
            why: vec![Piece::text("Small, one file")],
            objection: None,
            proposed: Way::OwnSession,
            proposed_who: Who::Seat(seat(4)),
        };
        let offer = WaysOffer {
            ways: vec![Way::OwnSession, workflow("maker-checker")],
            not_offered: Some("Not offered: agents".to_owned()),
        };
        let seats = vec![
            (seat(1), "claude-code/claude-opus-5-5".parse().unwrap()),
            (seat(4), "opencode/qwen3-coder-30b".parse().unwrap()),
        ];
        EditSheet::new(vec![task], &offer, seats)
    }

    #[test]
    fn left_and_right_go_round_the_ways_the_setup_allows() {
        let mut sheet = sheet();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(sheet.key(key(KeyCode::Right)), EditKey::Moved);
        assert_eq!(sheet.tasks[0].way, workflow("maker-checker"));
        assert!(sheet.tasks[0].changed());
        sheet.key(key(KeyCode::Right));
        assert_eq!(sheet.tasks[0].way, Way::OwnSession);
        sheet.key(key(KeyCode::Left));
        assert_eq!(sheet.tasks[0].way, workflow("maker-checker"));
        let EditKey::Approve(tasks) = sheet.key(key(KeyCode::Enter)) else {
            panic!("enter approves");
        };
        assert_eq!(note(&tasks), "task-003 way maker-checker");
        assert_eq!(sheet.key(key(KeyCode::Esc)), EditKey::Back);
    }

    /// Tab gives the picked task to the next seat, round to the first, and the edit line names
    /// the model that seat runs.
    #[test]
    fn tab_gives_the_task_to_the_next_seat() {
        let mut sheet = sheet();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(sheet.key(key(KeyCode::Tab)), EditKey::Moved);
        assert_eq!(sheet.tasks[0].who, Who::Seat(seat(1)));
        assert_eq!(
            note(&sheet.tasks),
            "task-003 worker claude-code/claude-opus-5-5"
        );
        sheet.key(key(KeyCode::Tab));
        assert!(!sheet.tasks[0].changed(), "back to the seat it had");
        assert_eq!(note(&sheet.tasks), "");
    }

    /// Only a task whose way you changed writes a line, in the words the approval reads.
    #[test]
    fn each_changed_way_writes_one_edit_line() {
        let mut tasks = sheet().tasks;
        tasks.push(EditTask {
            task: TaskId::numbered(4),
            way: Way::Agents { count: 2 },
            ..tasks[0].clone()
        });
        assert_eq!(note(&tasks), "task-004 way 2 agents");
        tasks[0].way = Way::CliWorkflow;
        assert_eq!(
            note(&tasks),
            "task-003 way its cli's own\ntask-004 way 2 agents"
        );
    }
}
