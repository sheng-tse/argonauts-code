//! `/prompts`: every role's parts, who writes each, and which you changed. `e` copies one out
//! to edit, `w` picks who writes a role's instructions, and `r` goes back to the built-in.
//! Nothing is locked, so every row takes the same keys.

use argonauts_core::app::{UiCommand, ViewData};
use argonauts_core::input::{PromptsAction, SlashCommand};
use argonauts_core::prompt_list::{PromptList, PromptRow, WriterChoice, heading};
use argonauts_core::prompt_parts::Readers;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::text::{sanitize_line, str_width, wrap_text};
use crate::theme::{Role, Theme};

use super::list::{self, Column, ListState, fit_columns};
use super::pager::position_mark;
use super::panes::{self, Footer, Header, Marks, Side};
use super::{View, ViewCtx, ViewOutcome};

/// Cells between columns.
const GAP: usize = 2;
/// Cells before the first column, where the selection mark goes.
const MARGIN: usize = 2;

const ROLE: &str = "role";
const PART: &str = "part";
const WRITTEN_BY: &str = "written by";
const YOURS: &str = "yours";

#[derive(Debug, Default)]
pub struct PromptsView {
    list: PromptList,
    cursor: ListState,
    /// A word about the last key, in the footer until the next key.
    said: Option<String>,
    /// The row `r` was pressed on once: a template's name, or `roles/<role>` for a role's
    /// own file. A second `r` deletes your file for it.
    resetting: Option<String>,
    page: usize,
}

impl PromptsView {
    pub fn new(list: PromptList) -> Self {
        Self {
            said: list.said.clone(),
            list,
            ..Self::default()
        }
    }

    fn selected(&self) -> Option<&PromptRow> {
        self.list.rows.get(self.cursor.selected)
    }

    fn columns(&self, width: usize) -> Vec<Column> {
        let rows = &self.list.rows;
        let cells = |f: &dyn Fn(&PromptRow) -> String| rows.iter().map(f).collect();
        // A role is named on its first line, and on the top line once that one scrolled away.
        let roles = (0..rows.len())
            .map(|index| {
                if index == self.cursor.offset {
                    clean(heading(&rows[index].role))
                } else {
                    clean(self.list.role_shown(index))
                }
            })
            .collect();
        let columns = vec![
            Column::new(ROLE, roles),
            Column::new(PART, cells(&|row| clean(&row.part))),
            Column::new(WRITTEN_BY, cells(&|row| clean(&row.written_by))).keep(1),
            Column::new(YOURS, cells(&|row| clean(&row.yours))).flex(),
        ];
        fit_columns(columns, width, GAP)
    }

    /// The heading row, then one line per prompt.
    fn lines(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let dim = theme.style(Role::Dim);
        let marks = Marks::for_theme(theme);
        let columns = self.columns(width.saturating_sub(MARGIN));
        let row_of = |cells: &dyn Fn(&Column) -> Span<'static>, opening: Span<'static>| {
            let mut spans = vec![opening];
            for (i, column) in columns.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw(" ".repeat(GAP)));
                }
                spans.push(cells(column));
            }
            Line::from(spans)
        };
        let blank = Span::raw(" ".repeat(MARGIN));
        let mut lines = vec![row_of(
            &|column| Span::styled(column.cell(column.heading), dim),
            blank.clone(),
        )];
        for index in 0..self.list.rows.len() {
            let selected = index == self.cursor.selected;
            let opening = if selected {
                Span::styled(format!("{} ", marks.selected), theme.style(Role::Question))
            } else {
                blank.clone()
            };
            let style = |column: &Column| match column.heading {
                // What is yours stands out on every row.
                YOURS => theme.style(Role::Label),
                _ if selected => theme.style(Role::Question),
                _ => theme.style(Role::Text),
            };
            lines.push(row_of(
                &|column| Span::styled(column.cell(&column.cells[index]), style(column)),
                opening,
            ));
        }
        lines
    }

    /// What you should know about your own files, under the list.
    fn notes(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let warning = theme.style(Role::Warn);
        let first = [Span::styled(format!("{} ", marks.warning), warning)];
        let rest = [Span::raw("  ")];
        self.list
            .notes
            .iter()
            .flat_map(|note| wrap_text(&clean(note), warning, width, &first, &rest))
            .collect()
    }

    /// What the last key did, in as many whole sentences as fit `width`, else the keys.
    fn footer(&self, width: usize, theme: &Theme) -> Footer {
        if let Some(said) = &self.said {
            return Footer::Line(Line::from(Span::styled(
                whole_sentences(&clean(said), width),
                theme.style(Role::Dim),
            )));
        }
        Footer::hints(&[
            ("↑↓ select", 0),
            ("⏎ show", 2),
            ("e copy to edit", 6),
            ("w who writes", 5),
            ("r back to built in", 4),
            ("d diff", 3),
            ("E copy for every project", 1),
            (panes::ESC_BACK, 0),
        ])
    }

    fn header(&self, theme: &Theme) -> Header {
        let changed = self.list.rows.iter().filter(|row| row.has_file).count();
        let mut summary = panes::count(self.list.rows.len(), "part");
        if changed > 0 {
            summary.push_str(&format!(" · {changed} yours"));
        }
        Header {
            title: Line::from(Span::styled("Prompts", theme.style(Role::Label))),
            description: "What each agent reads. A file of yours in .argonauts/prompts wins \
                          over the built-in"
                .to_owned(),
            summary,
        }
    }

    fn send(action: PromptsAction) -> ViewOutcome {
        ViewOutcome::Send(UiCommand::Slash {
            command: SlashCommand::Prompts { action },
        })
    }

    fn say(&mut self, said: String) -> ViewOutcome {
        self.said = Some(said);
        ViewOutcome::Consumed
    }

    /// The text is in the list already, so showing it costs no round trip.
    fn show(&self) -> ViewOutcome {
        let Some(row) = self.selected() else {
            return ViewOutcome::Consumed;
        };
        let from = if row.yours.is_empty() {
            &row.written_by
        } else {
            &row.yours
        };
        let title = format!(
            "{} · {} · {}",
            clean(heading(&row.role)),
            clean(&row.part),
            clean(from)
        );
        let about = match row.name {
            Some(_) => "The text in use, as the next agent that starts reads it",
            None => "Who writes them, and your own words for the role",
        };
        ViewOutcome::Open(Box::new(PromptText::new(title, about, &row.text)))
    }

    /// `e` writes the template out as a file of yours, or a starter for the role's own file,
    /// into the project. `E` writes it for every project.
    fn copy(&self, global: bool) -> ViewOutcome {
        let Some(row) = self.selected() else {
            return ViewOutcome::Consumed;
        };
        Self::send(match &row.name {
            Some(name) => PromptsAction::Copy {
                name: name.clone(),
                global,
            },
            None => PromptsAction::Role {
                role: row.role.clone(),
                global,
            },
        })
    }

    fn pick_writer(&mut self) -> ViewOutcome {
        let Some(row) = self.selected() else {
            return ViewOutcome::Consumed;
        };
        let role = row.role.clone();
        let current = self
            .list
            .rows
            .iter()
            .find(|other| other.role == role && other.name.is_none())
            .map(|instructions| instructions.written_by.clone())
            .unwrap_or_default();
        ViewOutcome::Open(Box::new(WriterPicker::new(
            role,
            self.list.writers.clone(),
            &current,
        )))
    }

    /// Only a file of yours can be deleted, and a second `r` does it, since the file holds
    /// what you wrote.
    fn reset(&mut self) -> ViewOutcome {
        let Some(row) = self.selected().cloned() else {
            return ViewOutcome::Consumed;
        };
        let (key, action, twice) = match row.name {
            Some(name) if !row.has_file => {
                return self.say(format!("{name} is the built-in already"));
            }
            Some(name) => (
                name.clone(),
                PromptsAction::Reset { name: name.clone() },
                format!("r again deletes your file for {name} and goes back to the built-in"),
            ),
            None if !row.has_file => {
                let said = "No file of yours adds to these instructions. w picks who writes them";
                return self.say(said.to_owned());
            }
            None => (
                format!("roles/{}", row.role),
                PromptsAction::ResetRole {
                    role: row.role.clone(),
                },
                format!(
                    "r again deletes your file for {}, with your words in it",
                    the_agents(&row.role)
                ),
            ),
        };
        if self.resetting.as_deref() == Some(&key) {
            self.resetting = None;
            return Self::send(action);
        }
        self.resetting = Some(key);
        self.say(twice)
    }

    fn diff(&mut self) -> ViewOutcome {
        let Some(row) = self.selected().cloned() else {
            return ViewOutcome::Consumed;
        };
        match row.name {
            Some(name) if row.has_file => Self::send(PromptsAction::Diff { name }),
            Some(name) => self.say(format!(
                "{name} is the built-in, so there's nothing to compare. e copies it to edit"
            )),
            None => self.say("Instructions have no built-in to compare with. ⏎ shows them".into()),
        }
    }
}

impl View for PromptsView {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let panes = panes::split(area, 0);
        panes::draw_header(buf, panes.header, &self.header(theme), theme);
        panes::draw_box(buf, &panes, ["", ""], Side::Right, theme);

        let width = usize::from(panes.right.width);
        // The notes keep at most a third of the box, with a blank row above them.
        let notes = self.notes(width, theme);
        let room = usize::from(panes.right.height);
        let notes_rows = notes.len().min(room / 3);
        let below = if notes_rows > 0 { notes_rows + 1 } else { 0 };
        let height = room.saturating_sub(below + 1);
        self.page = height.max(1);
        let rows = self.list.rows.len();
        self.cursor.keep_visible(rows, self.page);

        let lines = self.lines(width, theme);
        let mut shown = vec![lines[0].clone()];
        shown.extend(
            lines
                .iter()
                .skip(1 + self.cursor.offset)
                .take(self.page)
                .cloned(),
        );
        panes::put_lines(buf, panes.right, &shown);
        if notes_rows > 0 {
            let at = Rect {
                y: panes.right.bottom().saturating_sub(notes_rows as u16),
                height: notes_rows as u16,
                ..panes.right
            };
            panes::put_lines(buf, at, &notes[..notes_rows]);
        }

        let marks = Marks::for_theme(theme);
        let mark = position_mark(self.cursor.offset, height, rows, marks.down);
        let beside_mark = match str_width(&mark) {
            0 => 0,
            mark => mark + GAP,
        };
        let footer_room = usize::from(panes.footer.width).saturating_sub(beside_mark);
        let footer = self.footer(footer_room, theme);
        panes::draw_footer(buf, panes.footer, footer, &mark, theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        self.said = None;
        let resetting = self.resetting.take();
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') if plain => {
                return ViewOutcome::Close;
            }
            KeyCode::Enter | KeyCode::Right => return self.show(),
            KeyCode::Char('e') if plain => return self.copy(false),
            KeyCode::Char('E') if plain => return self.copy(true),
            KeyCode::Char('w') if plain => return self.pick_writer(),
            KeyCode::Char('r') if plain => {
                self.resetting = resetting;
                return self.reset();
            }
            KeyCode::Char('d') if plain => return self.diff(),
            _ => {}
        }
        match list::nav_key(&key, self.page) {
            Some(nav) => {
                self.cursor.apply(nav, self.list.rows.len(), self.page);
                ViewOutcome::Consumed
            }
            None => ViewOutcome::Ignored,
        }
    }

    /// A fresh list, such as after a copy, keeps the selection on the same part and says what
    /// was done.
    fn accept(&mut self, data: &ViewData) -> bool {
        let ViewData::Prompts { view } = data else {
            return false;
        };
        let picked = self
            .selected()
            .map(|row| (row.role.clone(), row.part.clone()));
        let index = picked
            .and_then(|(role, part)| {
                let same = |row: &PromptRow| row.role == role && row.part == part;
                view.rows.iter().position(same)
            })
            .unwrap_or(0);
        self.list = (**view).clone();
        self.said = self.list.said.clone();
        self.cursor.move_to(index, self.list.rows.len(), self.page);
        true
    }
}

/// `w`: who writes one role's instructions. Picking goes back to the conversation, where the change
/// is shown and asks where to keep it, as every setting does.
#[derive(Debug)]
struct WriterPicker {
    role: String,
    choices: Vec<WriterChoice>,
    cursor: ListState,
    /// The index of the writer in use now.
    current: Option<usize>,
    page: usize,
}

impl WriterPicker {
    fn new(role: String, choices: Vec<WriterChoice>, current: &str) -> Self {
        let current = choices
            .iter()
            .position(|choice| choice.shown == current || choice.value == current);
        Self {
            role,
            cursor: ListState {
                selected: current.unwrap_or(0),
                offset: 0,
            },
            choices,
            current,
            page: 1,
        }
    }

    /// What each kind of writer does, beside its name.
    fn about(choice: &WriterChoice) -> &'static str {
        match choice.value.as_str() {
            "orchestrator" => "its own words for each agent and each request",
            "built-in" => "the templates, as they are or as you changed them",
            _ => "this model writes them before each agent starts",
        }
    }

    fn lines(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let dim = theme.style(Role::Dim);
        let rows = &self.choices;
        let names = rows.iter().map(|choice| clean(&choice.shown)).collect();
        let about = rows
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                let in_use = if self.current == Some(index) {
                    " · in use"
                } else {
                    ""
                };
                format!("{}{in_use}", Self::about(choice))
            })
            .collect();
        let columns = fit_columns(
            vec![
                Column::new("writer", names),
                Column::new("what it does", about).flex(),
            ],
            width.saturating_sub(MARGIN),
            GAP,
        );
        (0..rows.len())
            .map(|index| {
                let selected = index == self.cursor.selected;
                let mut spans = vec![if selected {
                    Span::styled(format!("{} ", marks.selected), theme.style(Role::Question))
                } else {
                    Span::raw(" ".repeat(MARGIN))
                }];
                for (c, column) in columns.iter().enumerate() {
                    if c > 0 {
                        spans.push(Span::raw(" ".repeat(GAP)));
                    }
                    let style = match (c, selected) {
                        (0, true) => theme.style(Role::Question),
                        (0, false) => theme.style(Role::Text),
                        _ => dim,
                    };
                    spans.push(Span::styled(column.cell(&column.cells[index]), style));
                }
                Line::from(spans)
            })
            .collect()
    }
}

impl View for WriterPicker {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let panes = panes::split(area, 0);
        let marks = Marks::for_theme(theme);
        let header = Header {
            title: Line::from(vec![
                Span::styled("Prompts", theme.style(Role::Dim)),
                Span::styled(format!(" {} ", marks.crumb), theme.style(Role::Dim)),
                Span::styled(
                    format!("who writes for {}", the_agents(&self.role)),
                    theme.style(Role::Label),
                ),
            ]),
            description: "Whoever writes, an agent still reads its frame first, then your file \
                          for the role and what your request said for it"
                .to_owned(),
            summary: String::new(),
        };
        panes::draw_header(buf, panes.header, &header, theme);
        panes::draw_box(buf, &panes, ["", ""], Side::Right, theme);

        let height = usize::from(panes.right.height);
        self.page = height.max(1);
        let rows = self.choices.len();
        self.cursor.keep_visible(rows, self.page);
        let lines = self.lines(usize::from(panes.right.width), theme);
        let shown: Vec<Line<'static>> = lines.into_iter().skip(self.cursor.offset).collect();
        panes::put_lines(buf, panes.right, &shown);

        let footer = Footer::hints(&[("↑↓ select", 0), ("⏎ pick", 0), (panes::ESC_BACK, 0)]);
        let mark = position_mark(self.cursor.offset, height, rows, marks.down);
        panes::draw_footer(buf, panes.footer, footer, &mark, theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') if plain => {
                return ViewOutcome::Close;
            }
            KeyCode::Enter | KeyCode::Right => {
                let Some(choice) = self.choices.get(self.cursor.selected) else {
                    return ViewOutcome::Consumed;
                };
                return ViewOutcome::SendToConversation(UiCommand::Slash {
                    command: SlashCommand::Prompts {
                        action: PromptsAction::Writer {
                            role: Some(self.role.clone()),
                            who: choice.value.clone(),
                        },
                    },
                });
            }
            _ => {}
        }
        match list::nav_key(&key, self.page) {
            Some(nav) => {
                self.cursor.apply(nav, self.choices.len(), self.page);
                ViewOutcome::Consumed
            }
            None => ViewOutcome::Ignored,
        }
    }
}

/// One prompt's text, wrapped at words, since a prompt is prose.
#[derive(Debug)]
struct PromptText {
    title: String,
    about: &'static str,
    paragraphs: Vec<String>,
    /// Rows scrolled from the top.
    offset: usize,
    page: usize,
    rows: usize,
}

impl PromptText {
    fn new(title: String, about: &'static str, text: &str) -> Self {
        Self {
            title,
            about,
            paragraphs: text.trim_end().lines().map(clean_line).collect(),
            offset: 0,
            page: 1,
            rows: 0,
        }
    }

    fn lines(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let text = theme.style(Role::Text);
        self.paragraphs
            .iter()
            .flat_map(|paragraph| {
                if paragraph.is_empty() {
                    vec![Line::default()]
                } else {
                    wrap_text(paragraph, text, width.max(1), &[], &[])
                }
            })
            .collect()
    }
}

impl View for PromptText {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let panes = panes::split(area, 0);
        let header = Header {
            title: Line::from(Span::styled(self.title.clone(), theme.style(Role::Label))),
            description: self.about.to_owned(),
            summary: panes::count(self.paragraphs.len(), "line"),
        };
        panes::draw_header(buf, panes.header, &header, theme);
        panes::draw_box(buf, &panes, ["", ""], Side::Right, theme);

        let lines = self.lines(usize::from(panes.right.width), theme);
        let height = usize::from(panes.right.height);
        self.page = height.max(1);
        self.rows = lines.len();
        self.offset = self.offset.min(lines.len().saturating_sub(height));
        panes::put_lines(buf, panes.right, &lines[self.offset..]);

        let marks = Marks::for_theme(theme);
        let mark = position_mark(self.offset, height, lines.len(), marks.down);
        let hints = Footer::hints(&[
            ("↑↓ scroll", 0),
            ("pgup pgdn page", 1),
            (panes::ESC_BACK, 0),
        ]);
        panes::draw_footer(buf, panes.footer, hints, &mark, theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        if matches!(key.code, KeyCode::Esc | KeyCode::Left | KeyCode::Char('q')) {
            return ViewOutcome::Close;
        }
        match list::nav_key(&key, self.page) {
            Some(nav) => {
                self.offset = list::scroll(self.offset, nav, self.rows, self.page);
                ViewOutcome::Consumed
            }
            None => ViewOutcome::Ignored,
        }
    }
}

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

/// A line of a prompt keeps its indent, so a list inside a list still reads as one.
fn clean_line(text: &str) -> String {
    sanitize_line(text).trim_end().to_owned()
}

/// The role's agents in a sentence: `the reviewers`, `the main agent`, `the critic role`.
fn the_agents(role: &str) -> String {
    match Readers::of_file(role) {
        Some(readers) => readers.the_agents(),
        None => format!("the {}", clean(role)),
    }
}

/// As many whole sentences of `text` as fit `width`, and always the first. The footer has one
/// row, and a sentence cut in its middle says less than the ones before it.
fn whole_sentences(text: &str, width: usize) -> String {
    let mut shown = String::new();
    for sentence in text.split_inclusive(". ") {
        if !shown.is_empty() && str_width(&shown) + str_width(sentence.trim_end()) > width {
            break;
        }
        shown.push_str(sentence);
    }
    shown.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::prompt_list::{CHANGED, INSTRUCTIONS};

    use super::*;
    use crate::state::AppState;

    fn row(role: &str, part: &str, written_by: &str, yours: &str, name: Option<&str>) -> PromptRow {
        PromptRow {
            role: role.to_owned(),
            part: part.to_owned(),
            written_by: written_by.to_owned(),
            yours: yours.to_owned(),
            name: name.map(str::to_owned),
            has_file: !yours.is_empty(),
            text: format!("The {part} of the {role}.\nSecond line.\n"),
        }
    }

    /// A sample table's rows.
    fn pictured() -> PromptList {
        let built_in = "built in";
        let orchestrator = "the orchestrator";
        PromptList {
            rows: vec![
                row("worker", "frame", built_in, "", Some("frames/worker")),
                row(
                    "worker",
                    INSTRUCTIONS,
                    orchestrator,
                    "roles/worker.md",
                    None,
                ),
                row(
                    "reviewer",
                    "frame",
                    built_in,
                    CHANGED,
                    Some("frames/reviewer"),
                ),
                row("reviewer", INSTRUCTIONS, orchestrator, "", None),
                row("judge", "frame", built_in, "", Some("frames/judge")),
                row(
                    "orchestrator",
                    "how it plans",
                    built_in,
                    "",
                    Some("workflow/design"),
                ),
            ],
            writers: vec![
                WriterChoice {
                    value: "orchestrator".to_owned(),
                    shown: orchestrator.to_owned(),
                },
                WriterChoice {
                    value: "built-in".to_owned(),
                    shown: built_in.to_owned(),
                },
                WriterChoice {
                    value: "codex/gpt-5.6".to_owned(),
                    shown: "GPT 5.6 on Codex".to_owned(),
                },
            ],
            ..PromptList::default()
        }
    }

    fn press(view: &mut dyn View, code: KeyCode) -> ViewOutcome {
        let theme = Theme::dark();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        view.handle_key(KeyEvent::from(code), &ctx)
    }

    fn sent(outcome: ViewOutcome) -> PromptsAction {
        match outcome {
            ViewOutcome::Send(UiCommand::Slash {
                command: SlashCommand::Prompts { action },
            }) => action,
            other => panic!("{other:?}"),
        }
    }

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                let text: String = line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                text.trim_end().to_owned()
            })
            .collect()
    }

    fn on_reviewers_frame() -> PromptsView {
        let mut view = PromptsView::new(pictured());
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Down);
        view
    }

    /// The whole table, cell for cell.
    #[test]
    fn the_rows_read_as_the_picture_does() {
        let view = on_reviewers_frame();
        let lines = text(&view.lines(80, &Theme::dark()));
        assert_eq!(
            lines,
            [
                "  role          part          written by        yours",
                "  workers       frame         built in",
                "                instructions  the orchestrator  roles/worker.md",
                "❯ reviewers     frame         built in          ✎ changed",
                "                instructions  the orchestrator",
                "  judge         frame         built in",
                "  orchestrator  how it plans  built in",
            ]
        );
    }

    /// The pointer and the chosen row are lavender, as on every list.
    #[test]
    fn the_chosen_row_is_lavender() {
        let view = on_reviewers_frame();
        let theme = Theme::dark();
        let lines = view.lines(80, &theme);
        let chosen = &lines[3].spans;
        assert_eq!(chosen[0].content, "❯ ");
        assert_eq!(chosen[0].style, theme.style(Role::Question));
        let role = chosen
            .iter()
            .find(|span| span.content.contains("reviewers"));
        assert_eq!(role.unwrap().style, theme.style(Role::Question));
    }

    #[test]
    fn e_copies_a_template_and_starts_a_role_file() {
        let mut view = on_reviewers_frame();
        assert_eq!(
            sent(press(&mut view, KeyCode::Char('e'))),
            PromptsAction::Copy {
                name: "frames/reviewer".to_owned(),
                global: false
            }
        );
        press(&mut view, KeyCode::Down);
        assert_eq!(
            sent(press(&mut view, KeyCode::Char('e'))),
            PromptsAction::Role {
                role: "reviewer".to_owned(),
                global: false
            }
        );
        assert_eq!(
            sent(press(&mut view, KeyCode::Char('E'))),
            PromptsAction::Role {
                role: "reviewer".to_owned(),
                global: true
            }
        );
    }

    #[test]
    fn r_asks_twice_before_it_deletes_your_file() {
        let mut view = on_reviewers_frame();
        assert!(matches!(
            press(&mut view, KeyCode::Char('r')),
            ViewOutcome::Consumed
        ));
        let said = view.said.clone().unwrap();
        assert!(said.starts_with("r again deletes your file for frames/reviewer"));
        assert_eq!(
            sent(press(&mut view, KeyCode::Char('r'))),
            PromptsAction::Reset {
                name: "frames/reviewer".to_owned()
            }
        );
        // Any other key between the two takes the first one back.
        press(&mut view, KeyCode::Char('r'));
        press(&mut view, KeyCode::Up);
        press(&mut view, KeyCode::Down);
        assert!(matches!(
            press(&mut view, KeyCode::Char('r')),
            ViewOutcome::Consumed
        ));
    }

    /// A role's own file holds your words too, so it goes the same way as a template.
    #[test]
    fn r_twice_deletes_your_file_for_a_role() {
        let mut view = PromptsView::new(pictured());
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Char('r'));
        assert_eq!(
            view.said.as_deref(),
            Some("r again deletes your file for the workers, with your words in it")
        );
        assert_eq!(
            sent(press(&mut view, KeyCode::Char('r'))),
            PromptsAction::ResetRole {
                role: "worker".to_owned()
            }
        );
    }

    /// A finer role's rows go by its own name, so every key acts on its own files.
    #[test]
    fn the_keys_act_on_a_finer_roles_own_files() {
        let mut list = pictured();
        list.rows = vec![
            row("critic", "frame", "built in", "", Some("frames/critic")),
            row(
                "critic",
                INSTRUCTIONS,
                "the orchestrator",
                "roles/critic.md",
                None,
            ),
        ];
        let mut view = PromptsView::new(list);
        assert_eq!(
            sent(press(&mut view, KeyCode::Char('e'))),
            PromptsAction::Copy {
                name: "frames/critic".to_owned(),
                global: false
            }
        );
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Char('r'));
        assert_eq!(
            view.said.as_deref(),
            Some("r again deletes your file for the critic role, with your words in it")
        );
        assert_eq!(
            sent(press(&mut view, KeyCode::Char('r'))),
            PromptsAction::ResetRole {
                role: "critic".to_owned()
            }
        );
    }

    #[test]
    fn r_and_d_say_why_there_is_nothing_to_do() {
        let mut view = PromptsView::new(pictured());
        press(&mut view, KeyCode::Char('r'));
        assert_eq!(
            view.said.as_deref(),
            Some("frames/worker is the built-in already")
        );
        press(&mut view, KeyCode::Char('d'));
        assert!(view.said.clone().unwrap().contains("nothing to compare"));
        // The reviewers' instructions have no file of yours after them.
        for _ in 0..3 {
            press(&mut view, KeyCode::Down);
        }
        press(&mut view, KeyCode::Char('r'));
        let said = view.said.clone().unwrap();
        assert!(said.starts_with("No file of yours adds"), "{said}");
        // A file of yours is compared by the controller, which has it as it is now.
        let mut view = on_reviewers_frame();
        assert_eq!(
            sent(press(&mut view, KeyCode::Char('d'))),
            PromptsAction::Diff {
                name: "frames/reviewer".to_owned()
            }
        );
    }

    #[test]
    fn enter_shows_the_text_in_use() {
        let mut view = on_reviewers_frame();
        match press(&mut view, KeyCode::Enter) {
            ViewOutcome::Open(shown) => {
                let text = format!("{shown:?}");
                assert!(text.contains("reviewers · frame · ✎ changed"), "{text}");
                assert!(text.contains("The frame of the reviewer."), "{text}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_prompt_wraps_at_words_and_keeps_its_blank_lines() {
        let shown = PromptText::new(
            "reviewers · frame".to_owned(),
            "The text in use",
            "You are a reviewer. You judge one piece of work.\n\n- Judge the work itself.\n",
        );
        assert_eq!(
            text(&shown.lines(24, &Theme::dark())),
            [
                "You are a reviewer. You",
                "judge one piece of work.",
                "",
                "- Judge the work itself.",
            ]
        );
    }

    #[test]
    fn the_footer_keeps_whole_sentences() {
        let said = "Wrote .argonauts/prompts/frames/main.md. Edit it, and the next agent reads \
                    your text. Delete it to go back.";
        assert_eq!(
            whole_sentences(said, 60),
            "Wrote .argonauts/prompts/frames/main.md."
        );
        assert_eq!(whole_sentences(said, 200), said);
        // The first sentence always shows, and the row cuts it when it must.
        assert_eq!(
            whole_sentences(said, 10),
            "Wrote .argonauts/prompts/frames/main.md."
        );
    }

    #[test]
    fn the_top_row_names_its_role_once_the_first_scrolled_away() {
        let mut view = PromptsView::new(pictured());
        view.cursor.offset = 1;
        view.cursor.selected = 1;
        let lines = text(&view.lines(80, &Theme::dark()));
        assert!(
            lines[2].starts_with("❯ workers       instructions"),
            "{lines:?}"
        );
    }

    #[test]
    fn w_picks_who_writes_for_the_rows_role() {
        let mut view = on_reviewers_frame();
        let ViewOutcome::Open(mut picker) = press(&mut view, KeyCode::Char('w')) else {
            panic!("w opens the writers");
        };
        // It opens on the writer in use, and Enter on another asks for the change in the
        // conversation.
        press(picker.as_mut(), KeyCode::Down);
        press(picker.as_mut(), KeyCode::Down);
        match press(picker.as_mut(), KeyCode::Enter) {
            ViewOutcome::SendToConversation(UiCommand::Slash {
                command: SlashCommand::Prompts { action },
            }) => assert_eq!(
                action,
                PromptsAction::Writer {
                    role: Some("reviewer".to_owned()),
                    who: "codex/gpt-5.6".to_owned()
                }
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_writers_say_what_each_does_and_which_is_in_use() {
        let list = pictured();
        assert_eq!(the_agents("main"), "the main agent");
        let picker = WriterPicker::new("judge".to_owned(), list.writers, "built in");
        assert_eq!(picker.cursor.selected, 1);
        let lines = text(&picker.lines(90, &Theme::dark()));
        assert_eq!(
            lines,
            [
                "  the orchestrator  its own words for each agent and each request",
                "❯ built in          the templates, as they are or as you changed them · in use",
                "  GPT 5.6 on Codex  this model writes them before each agent starts",
            ]
        );
    }

    #[test]
    fn a_fresh_list_keeps_the_selection_and_says_what_was_done() {
        let mut view = on_reviewers_frame();
        let mut fresh = pictured();
        fresh
            .rows
            .insert(0, row("main", "frame", "built in", "", Some("frames/main")));
        fresh.said = Some("Wrote .argonauts/prompts/frames/reviewer.md.".to_owned());
        fresh.refresh = true;
        assert!(view.accept(&ViewData::Prompts {
            view: Box::new(fresh)
        }));
        let row = view.selected().unwrap();
        assert_eq!(
            (row.role.as_str(), row.part.as_str()),
            ("reviewer", "frame")
        );
        assert!(view.said.clone().unwrap().starts_with("Wrote "));
    }

    #[test]
    fn notes_about_your_files_sit_under_the_list() {
        let mut list = pictured();
        list.notes = vec![
            "roles/reviewer.md names Codex, which tells the reviewers who made the work."
                .to_owned(),
        ];
        let view = PromptsView::new(list);
        let notes = text(&view.notes(40, &Theme::dark()));
        assert!(
            notes[0].ends_with("roles/reviewer.md names Codex, which"),
            "{notes:?}"
        );
        assert!(notes[1].starts_with("  tells the reviewers"), "{notes:?}");
    }
}
