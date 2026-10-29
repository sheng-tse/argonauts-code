//! Tasks inside the seats' bubbles: the note keeper's plan and who does what, and one task per
//! task while the work runs, its way's progress drawn as its run row draws it.

use argonauts_core::events::task::Way;
use argonauts_core::names::SeatId;

use super::faces::{BADGE, Face, Faces};
use super::item::{Body, SeatSays};
use super::words::{self, Piece, Tone, wrap};
use crate::kit::legend::BETWEEN_WORDS;
use crate::text::str_width;
use crate::theme::Theme;

/// The widest a task's words get, so tasks line up one under another in a wide screen.
pub const TASK_WIDTH: usize = 76;

/// Below this width a task task drops what it can do without: a workflow's last words, the
/// agents' "routes", and a long tool call.
pub const WIDE_FROM: usize = 100;

/// The way as a task's box, its run row and the plan name it: "Own session", the workflow's
/// name, "3 agents".
pub fn way_word(way: &Way) -> String {
    match way {
        Way::OwnSession => "Own session".to_owned(),
        Way::Workflow { name } => argonauts_core::text::shown_name(name.as_str()),
        Way::CliWorkflow => "Its CLI's workflow".to_owned(),
        Way::Agents { count } => agents_word(*count),
    }
}

/// How many agents a task is split between, "3 agents", or "1 agent" for a plan that gives
/// it one.
pub fn agents_word(count: u8) -> String {
    match count {
        1 => "1 agent".to_owned(),
        count => format!("{count} agents"),
    }
}

/// `text` and blanks to `cells`.
pub fn padded(text: &str, cells: usize) -> String {
    let size = str_width(text);
    format!("{text}{}", " ".repeat(cells.saturating_sub(size)))
}

/// Who does a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Who {
    Seat(SeatId),
    /// A model more than one seat runs, when the plan doesn't say which seat: its logo and
    /// its name with no seat's number.
    Model(Face),
}

impl Who {
    /// Its cells: the badge, a blank and its name.
    fn width(&self, faces: &Faces, theme: &Theme) -> usize {
        let name = match self {
            Who::Seat(seat) => faces.face(seat).beside_width(theme),
            Who::Model(face) => face.beside_width(theme),
        };
        BADGE + 1 + name
    }

    /// Its badge, then its name as it reads beside the badge, padded to `cells` after a blank,
    /// in its CLI's colour.
    pub fn pieces(&self, cells: usize, bold: bool, faces: &Faces, theme: &Theme) -> [Piece; 2] {
        match self {
            Who::Seat(seat) => [
                Piece::Badge(seat.clone()),
                Piece::InColour(
                    seat.clone(),
                    format!(" {}", padded(&faces.face(seat).beside(theme), cells)),
                    bold,
                ),
            ],
            Who::Model(face) => [
                Piece::Logo(face.logo.clone()),
                Piece::InColourOf(
                    face.cli.clone(),
                    format!(" {}", padded(&face.beside(theme), cells)),
                    bold,
                ),
            ],
        }
    }
}

/// One task of the plan, as the note keeper's task lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanTask {
    pub name: String,
    pub who: Who,
    /// How it is done: "Own session", a workflow's name, "3 agents".
    pub way: String,
    /// Why that way: "You asked" for a way you named, else the note keeper's reason.
    pub why: Vec<Piece>,
    /// Who checks it, on your copy only.
    pub checked_by: Vec<Piece>,
    /// A seat's objection to its way, with its reason.
    pub objection: Option<(SeatId, String)>,
}

/// Which columns the who-does-what table has. Each takes its widest cell and
/// [`BETWEEN_WORDS`] blanks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Columns {
    pub why: bool,
}

impl Columns {
    /// The plan as the note keeper proposes it: who, how, why and who checks.
    pub const PROPOSED: Columns = Columns { why: true };
    /// Who does what once agreed: who, how and who checks.
    pub const AGREED: Columns = Columns { why: false };
}

/// A column's cells: its widest, the header's counted, and the blanks before the next column.
fn column_cells(header: &str, cells: impl Iterator<Item = usize>) -> usize {
    cells.fold(str_width(header), usize::max) + BETWEEN_WORDS
}

/// "You asked", in your bubble's blue: what came from you, wherever it shows.
pub fn you_asked() -> Piece {
    Piece::tone("You asked", Tone::Yours)
}

/// ✘ and the objecting seat's name, then its reason, dim. A seat that objected with a tapback
/// alone has no reason: "✘ 3.8 Flash objects".
pub fn objection(seat: &SeatId, reason: &str, indent: &str) -> Vec<Piece> {
    let mut pieces = vec![
        Piece::text(indent),
        Piece::tone("✘ ", Tone::Danger),
        Piece::text(" "),
        Piece::Named(seat.clone()),
    ];
    if reason.is_empty() {
        pieces.push(Piece::dim(" objects"));
    } else {
        pieces.push(Piece::InColour(seat.clone(), ": ".to_owned(), false));
        pieces.push(Piece::dim(reason));
    }
    pieces
}

/// The who-does-what task: a row per task with its objection under it, the rule of thumb the
/// note keeper went by (when given), and the line that says only you see who checks.
pub fn who_does_what(
    tasks: &[PlanTask],
    columns: Columns,
    rule: Option<&str>,
    room: usize,
    faces: &Faces,
    theme: &Theme,
) -> Vec<Vec<Piece>> {
    let task_cells = column_cells("Task", tasks.iter().map(|task| str_width(&task.name)));
    let who_cells = column_cells("Who", tasks.iter().map(|task| task.who.width(faces, theme)));
    let how_cells = column_cells("How", tasks.iter().map(|task| str_width(&task.way)));
    let why_cells = columns.why.then(|| {
        column_cells(
            "Why",
            tasks
                .iter()
                .map(|task| words::width(&task.why, faces, theme)),
        )
    });
    let name_cells = who_cells.saturating_sub(BADGE + 1);
    let mut head = vec![
        Piece::dim(padded("Task", task_cells)),
        Piece::dim(padded("Who", who_cells)),
        Piece::dim(padded("How", how_cells)),
    ];
    if let Some(why) = why_cells {
        head.push(Piece::dim(padded("Why", why)));
    }
    head.push(Piece::dim("Checked by"));
    let mut rows = vec![head];
    for task in tasks {
        let mut row = vec![Piece::text(padded(&task.name, task_cells))];
        row.extend(task.who.pieces(name_cells, true, faces, theme));
        row.push(Piece::text(padded(&task.way, how_cells)));
        if let Some(why) = why_cells {
            row.extend(task.why.iter().cloned());
            row.push(Piece::text(" ".repeat(
                why.saturating_sub(words::width(&task.why, faces, theme)),
            )));
        }
        row.extend(task.checked_by.iter().cloned());
        rows.push(row);
        if let Some((seat, reason)) = &task.objection {
            rows.push(objection(seat, reason, &" ".repeat(task_cells)));
        }
    }
    if let Some(rule) = rule {
        rows.extend(wrap(
            &[Piece::dim(rule)],
            room.saturating_sub(2),
            faces,
            theme,
        ));
    }
    rows.push(vec![
        Piece::tone("Only you see Checked by: ", Tone::Label),
        Piece::dim("the seats read \"a fresh session that code picks\"."),
    ]);
    rows
}

/// A task's progress, as its run row draws it, with the count's unit named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub done: u32,
    pub total: u32,
    /// "steps", "to-dos", "agents".
    pub unit: String,
    pub time: String,
    pub tokens: String,
}

impl Progress {
    /// Claude Code's segmented bar in `cells`, then the count, the time and the tokens. With
    /// nothing to count, such as a session with no to-do list, the bar stays empty and no count
    /// shows.
    pub fn pieces(&self, cells: usize) -> Vec<Piece> {
        let total = usize::try_from(self.total.max(1)).unwrap_or(1);
        let done = usize::try_from(self.done).unwrap_or(0).min(total);
        let fill = if self.total == 0 {
            0
        } else {
            done * cells / total
        };
        let count = if self.total == 0 {
            String::new()
        } else {
            format!("{}/{} {} · ", self.done, self.total, self.unit)
        };
        vec![
            Piece::text("▰".repeat(fill)),
            Piece::dim("▱".repeat(cells - fill)),
            Piece::dim(format!("  {count}{} · ", self.time)),
            Piece::dim("↓  "),
            Piece::dim(format!("{} tokens", self.tokens)),
        ]
    }
}

/// The cells of a task's bar at `width`.
pub fn bar_cells(width: usize) -> usize {
    if width < WIDE_FROM { 12 } else { 20 }
}

/// A task's task: its way and task, what it does now, and its progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskWork {
    pub way: String,
    pub task: String,
    pub now: Now,
    pub progress: Progress,
}

/// What a task's task says it does now, by its way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Now {
    /// A workflow's steps, and how it ends.
    Steps {
        steps: Vec<Step>,
        ending: Option<String>,
    },
    /// The seat's model's agents, each on its part.
    Agents {
        seat: SeatId,
        parts: Vec<(bool, String)>,
        /// Words after each part on a wide screen: " routes".
        suffix: String,
    },
    /// The seat's own session's latest tool call.
    Call {
        tool: String,
        argument: String,
    },
    Words(Vec<Piece>),
}

impl Now {
    pub fn pieces(&self, width: usize) -> Vec<Piece> {
        match self {
            Now::Steps { steps: all, ending } => steps(all, ending.as_deref(), width),
            Now::Agents {
                seat,
                parts,
                suffix,
            } => agents(parts, seat, suffix, width),
            Now::Call { tool, argument } => tool_call(tool, argument, width),
            Now::Words(pieces) => pieces.clone(),
        }
    }
}

impl TaskWork {
    pub fn rows(&self, width: usize) -> Vec<Vec<Piece>> {
        vec![
            vec![
                Piece::tone(self.way.clone(), Tone::Bold),
                Piece::dim(format!("  {}", self.task)),
            ],
            self.now.pieces(width),
            self.progress.pieces(bar_cells(width)),
        ]
    }

    /// The task as its seat's message: only the picked one says what Enter does there.
    pub fn message(&self, seat: SeatId, picked: bool) -> SeatSays {
        let mut message = SeatSays::new(seat, Body::Task(self.clone()));
        message.min_width = TASK_WIDTH;
        message.picked = picked;
        message.hint = picked.then(|| "enter to open".to_owned());
        message
    }
}

/// A step of a workflow as its task names it: done or at work, its name, who does it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub done: bool,
    pub name: String,
    pub by: SeatId,
    /// Dim words after who does it: ", new session".
    pub note: String,
}

fn mark(done: bool) -> Piece {
    if done {
        Piece::tone("✔  ", Tone::Ok)
    } else {
        Piece::dim("○  ")
    }
}

/// A workflow's steps joined by faint rules, and at a wide screen how it ends.
pub fn steps(steps: &[Step], ending: Option<&str>, width: usize) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let wide = width >= WIDE_FROM;
    for (i, step) in steps.iter().enumerate() {
        if i > 0 {
            pieces.push(Piece::tone(" ── ", Tone::Faint));
        }
        pieces.push(mark(step.done));
        pieces.push(Piece::text(format!("{} ", step.name)));
        pieces.push(Piece::Named(step.by.clone()));
        if wide && !step.note.is_empty() {
            pieces.push(Piece::dim(step.note.clone()));
        }
    }
    if wide && let Some(ending) = ending {
        pieces.push(Piece::tone(" ── ", Tone::Faint));
        pieces.push(Piece::dim(ending));
    }
    pieces
}

/// A task's agents side by side, each with its mark, its model's badge and its part.
pub fn agents(parts: &[(bool, String)], seat: &SeatId, suffix: &str, width: usize) -> Vec<Piece> {
    let wide = width >= WIDE_FROM;
    let mut pieces = Vec::new();
    for (i, (done, part)) in parts.iter().enumerate() {
        if i > 0 {
            pieces.push(Piece::text(if wide { "    " } else { "   " }));
        }
        pieces.push(mark(*done));
        pieces.push(Piece::Badge(seat.clone()));
        let part = if wide {
            format!(" {part}{suffix}")
        } else {
            format!(" {part}")
        };
        pieces.push(Piece::text(part));
    }
    pieces
}

/// The latest tool call of a task done in its seat's own session, cut where the screen is
/// narrow.
pub fn tool_call(tool: &str, argument: &str, width: usize) -> Vec<Piece> {
    let call = if width >= WIDE_FROM {
        format!("({argument})")
    } else {
        cut_call(argument, NARROW_CALL.saturating_sub(str_width(tool) + 3))
    };
    vec![
        Piece::dim("⏺  "),
        Piece::tone(tool, Tone::Bold),
        Piece::text(call),
    ]
}

/// Cells a tool call's row takes at most on a narrow screen.
const NARROW_CALL: usize = 40;

/// `(argument)` in at most `cells`, cut with an ellipsis that keeps its closing parenthesis.
fn cut_call(argument: &str, cells: usize) -> String {
    if str_width(argument) + 2 <= cells {
        return format!("({argument})");
    }
    let room = cells.saturating_sub(5);
    let mut kept = String::new();
    for ch in argument.chars() {
        if str_width(&kept) + str_width(&ch.to_string()) > room {
            break;
        }
        kept.push(ch);
    }
    format!("({}… )", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plan that gives a task one agent says so in the singular.
    #[test]
    fn one_agent_is_one_agent() {
        assert_eq!(way_word(&Way::Agents { count: 1 }), "1 agent");
        assert_eq!(way_word(&Way::Agents { count: 3 }), "3 agents");
    }

    #[test]
    fn a_long_call_is_cut_and_keeps_its_parenthesis() {
        let call = tool_call("Bash", "npm run migrate:new add_rate_limit_to_plans", 80);
        let text: String = call
            .iter()
            .map(|piece| match piece {
                Piece::Words(words, _) => words.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(text, "⏺  Bash(npm run migrate:new add_rate… )");
    }

    #[test]
    fn a_bar_fills_its_share_and_names_its_unit() {
        let progress = Progress {
            done: 1,
            total: 2,
            unit: "steps".to_owned(),
            time: "3m36s".to_owned(),
            tokens: "402k".to_owned(),
        };
        let text: String = progress
            .pieces(20)
            .iter()
            .map(|piece| match piece {
                Piece::Words(words, _) => words.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(
            text,
            "▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱  1/2 steps · 3m36s · ↓  402k tokens"
        );
    }
}
