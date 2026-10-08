//! The workflow library: every workflow Argonauts can run, from the built-ins, your files, and
//! the repo's. It shows one, copies one out to edit, diffs your file with the built-in, and
//! opens your file's page.
//! With no run to show, `/workflows` and Ctrl+T open it. The workflow view has no tab for it,
//! as Claude Code's has none.

use argonauts_core::app::{UiCommand, ViewData, ViewRequest};
use argonauts_core::input::{SlashCommand, WorkflowAction};
use argonauts_core::views::{WorkflowFile, WorkflowOrigin};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::text::sanitize_line;
use crate::theme::{Role, Theme};

use super::list::{self, Column, ListState, fit_columns};
use super::pager::{Pager, position_mark};
use super::panes::{self, Footer, Header, Marks, Side};
use super::{View, ViewCtx, ViewOutcome};

/// Cells between columns.
const GAP: usize = 2;

/// The library's rows and selection. `None` until the controller's list arrives.
#[derive(Debug, Default)]
pub struct Library {
    pub workflows: Option<Vec<WorkflowFile>>,
    pub list: ListState,
    /// A word about the last key, such as why `d` had nothing to compare, in the footer until
    /// the next key.
    pub said: Option<String>,
    /// The workflow `d` asked the controller to compare. When it answers with a fresh list
    /// instead of the diff pager, the footer says why.
    comparing: Option<String>,
    page: usize,
}

impl Library {
    pub fn request() -> ViewRequest {
        ViewRequest::WorkflowLibrary
    }

    fn rows(&self) -> &[WorkflowFile] {
        self.workflows.as_deref().unwrap_or_default()
    }

    fn selected(&self) -> Option<&WorkflowFile> {
        self.rows().get(self.list.selected)
    }

    fn columns(&self, width: usize) -> Vec<Column> {
        let rows = self.rows();
        let cells = |f: &dyn Fn(&WorkflowFile) -> String| rows.iter().map(f).collect();
        let columns = vec![
            Column::new("Workflow", cells(&|row| clean(&row.name))),
            Column::new("Notes", cells(&notes)).keep(2),
            Column::new("From", cells(&|row| clean(&row.source))).keep(1),
            Column::new("Description", cells(&|row| clean(about(row)))).flex(),
        ];
        fit_columns(columns, width, GAP)
    }

    fn lines(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let dim = theme.style(Role::Dim);
        let Some(rows) = &self.workflows else {
            return vec![Line::from(Span::styled(
                "  Looking for workflow files…",
                dim,
            ))];
        };
        let marks = Marks::for_theme(theme);
        let lead = 2;
        let columns = self.columns(width.saturating_sub(lead));
        let mut heading = vec![Span::raw(" ".repeat(lead))];
        for (i, column) in columns.iter().enumerate() {
            if i > 0 {
                heading.push(Span::raw(" ".repeat(GAP)));
            }
            heading.push(Span::styled(column.cell(column.heading), dim));
        }
        let mut lines = vec![Line::from(heading)];
        for (i, row) in rows.iter().enumerate() {
            let selected = i == self.list.selected;
            let mut spans = vec![if selected {
                Span::styled(format!("{} ", marks.selected), theme.style(Role::Card))
            } else {
                Span::raw("  ")
            }];
            for (c, column) in columns.iter().enumerate() {
                if c > 0 {
                    spans.push(Span::raw(" ".repeat(GAP)));
                }
                let style = match column.heading {
                    "Workflow" if selected => theme.style(Role::Card),
                    "Workflow" => theme.style(Role::Text),
                    "Description" if row.problem.is_some() => theme.style(Role::Warning),
                    _ => dim,
                };
                spans.push(Span::styled(column.cell(&column.cells[i]), style));
            }
            lines.push(Line::from(spans));
        }
        if rows.is_empty() {
            lines.push(Line::from(Span::styled("  No workflows found", dim)));
        }
        lines
    }

    fn footer(&self, theme: &Theme) -> Footer {
        if let Some(said) = &self.said {
            return Footer::Line(Line::from(Span::styled(
                sanitize_line(said).into_owned(),
                theme.style(Role::Dim),
            )));
        }
        Footer::hints(&[
            ("↑↓ select", 0),
            ("⏎ show", 5),
            ("c copy", 4),
            ("C copy to repo", 2),
            ("d diff", 3),
            ("e edit", 4),
            (panes::ESC_BACK, 0),
        ])
    }

    /// The whole frame: header, tabs, the list, and the footer.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let title = "Workflow library";
        let header = Header {
            title: Line::from(Span::styled(title, theme.style(Role::Label))),
            description: "A repo file wins over yours in ~/.argonauts/workflows, which wins \
                          over the built-in of the same name"
                .to_owned(),
            summary: self
                .workflows
                .as_ref()
                .map_or_else(String::new, |rows| panes::count(rows.len(), "workflow")),
        };
        let panes = panes::split(area, 0);
        panes::draw_header(buf, panes.header, &header, theme);
        panes::draw_box(buf, &panes, ["", ""], Side::Right, theme);

        let rows = self.rows().len();
        let height = usize::from(panes.right.height).saturating_sub(1);
        self.page = height.max(1);
        self.list.keep_visible(rows, self.page);
        let lines = self.lines(usize::from(panes.right.width), theme);
        let mut shown = vec![lines[0].clone()];
        shown.extend(lines.iter().skip(1 + self.list.offset).cloned());
        panes::put_lines(buf, panes.right, &shown);

        let marks = Marks::for_theme(theme);
        let mark = position_mark(self.list.offset, height, rows, marks.down);
        panes::draw_footer(buf, panes.footer, self.footer(theme), &mark, theme);
    }

    pub fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        self.said = None;
        self.comparing = None;
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let vim = list::vim(ctx.state);
        let key = list::standard(key, vim, KeyCode::Esc);
        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') if plain => {
                return ViewOutcome::Close;
            }
            KeyCode::Enter | KeyCode::Right => return self.show(),
            KeyCode::Char('c') if plain => return self.file(WorkflowAction::Copy),
            KeyCode::Char('C') if plain => {
                return self.file(WorkflowAction::CopyToProject);
            }
            KeyCode::Char('d') if plain => return self.diff(),
            KeyCode::Char('e') if plain => return self.edit(),
            _ => {}
        }
        let len = self.rows().len();
        match list::nav_key(&key, self.page) {
            Some(nav) => {
                self.list.apply(nav, len, self.page);
                ViewOutcome::Consumed
            }
            None => ViewOutcome::Ignored,
        }
    }

    /// The text is in the list already, so showing it costs no round trip.
    fn show(&self) -> ViewOutcome {
        let Some(row) = self.selected() else {
            return ViewOutcome::Consumed;
        };
        let title = format!("{} · {}", clean(&row.name), clean(&row.source));
        let lines = row.text.lines().map(str::to_owned).collect();
        ViewOutcome::Open(Box::new(Pager::text(title, lines)))
    }

    fn file(&self, action: WorkflowAction) -> ViewOutcome {
        match self.selected() {
            Some(row) => ViewOutcome::Send(UiCommand::WorkflowFile {
                name: row.name.clone(),
                action,
            }),
            None => ViewOutcome::Consumed,
        }
    }

    /// Your file opens on its page. A built-in one has no file to change until you copy it.
    fn edit(&mut self) -> ViewOutcome {
        let Some(row) = self.selected() else {
            return ViewOutcome::Consumed;
        };
        if row.origin == WorkflowOrigin::BuiltIn {
            let name = clean(&row.name);
            self.said = Some(format!("{name} is built in. c copies it to edit"));
            return ViewOutcome::Consumed;
        }
        ViewOutcome::Send(UiCommand::Slash {
            command: SlashCommand::WorkflowEdit {
                name: row.name.clone(),
            },
        })
    }

    /// Only your file can differ from the built-in, so the others say why there's no diff at
    /// once. Your file goes to the controller every time, since you may have edited it since
    /// the list came.
    fn diff(&mut self) -> ViewOutcome {
        let Some(row) = self.selected() else {
            return ViewOutcome::Consumed;
        };
        let name = clean(&row.name);
        let said = match (row.origin, row.edited) {
            (WorkflowOrigin::BuiltIn, _) => {
                format!(
                    "{name} is the built-in, so there's nothing to compare. c copies it to edit"
                )
            }
            (_, None) => format!("{name} has no built-in of the same name to compare with"),
            (_, Some(_)) => {
                let outcome = self.file(WorkflowAction::Diff);
                self.comparing = Some(row.name.clone());
                return outcome;
            }
        };
        self.said = Some(said);
        ViewOutcome::Consumed
    }

    /// A fresh list, such as after a copy, keeps the selection on the same workflow.
    pub fn accept(&mut self, data: &ViewData) -> bool {
        let ViewData::WorkflowLibrary { workflows, .. } = data else {
            return false;
        };
        let picked = self.selected().map(|row| row.name.clone());
        let index = picked
            .and_then(|name| workflows.iter().position(|row| row.name == name))
            .or_else(|| workflows.iter().position(|row| row.in_use))
            .unwrap_or(0);
        if let Some(compared) = self.comparing.take()
            && let Some(row) = workflows.iter().find(|row| row.name == compared)
            && row.edited == Some(false)
        {
            self.said = Some(format!("{} matches the built-in", clean(&row.name)));
        }
        self.workflows = Some(workflows.clone());
        self.list.move_to(index, workflows.len(), self.page);
        true
    }
}

/// `in use` for the workflow the `workflow` setting names, `edited` for your changed copy.
fn notes(row: &WorkflowFile) -> String {
    let mut notes = Vec::new();
    if row.in_use {
        notes.push("in use");
    }
    if row.edited == Some(true) {
        notes.push("edited");
    }
    notes.join(" · ")
}

/// What the row says of itself, or why `/<name>` won't run it.
fn about(row: &WorkflowFile) -> &str {
    row.problem.as_deref().unwrap_or(&row.description)
}

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

/// The Library on its own, for when there's no run to show.
#[derive(Debug, Default)]
pub struct LibraryView {
    pub library: Library,
}

impl LibraryView {
    pub fn new(workflows: Vec<WorkflowFile>) -> Self {
        let mut library = Library::default();
        library.accept(&ViewData::WorkflowLibrary {
            workflows,
            refresh: false,
        });
        Self { library }
    }
}

impl View for LibraryView {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        self.library.render(area, buf, ctx.theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        self.library.handle_key(key, ctx)
    }

    fn accept(&mut self, data: &ViewData) -> bool {
        self.library.accept(data)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::state::AppState;

    fn file(name: &str, origin: WorkflowOrigin, edited: Option<bool>) -> WorkflowFile {
        WorkflowFile {
            name: name.to_owned(),
            origin,
            source: match origin {
                WorkflowOrigin::BuiltIn => "built in".to_owned(),
                WorkflowOrigin::Yours => format!("~/.argonauts/workflows/{name}.toml"),
                WorkflowOrigin::Repo => format!(".argonauts/workflows/{name}.toml"),
            },
            description: format!("The {name} workflow"),
            text: format!("name = \"{name}\"\n\n[[step]]\nname = \"Do\"\nrole = \"workers\"\n"),
            edited,
            in_use: name == "orchestrator",
            problem: None,
        }
    }

    fn view() -> LibraryView {
        LibraryView::new(vec![
            file("solo", WorkflowOrigin::BuiltIn, None),
            file("orchestrator", WorkflowOrigin::Yours, Some(true)),
            file("mine", WorkflowOrigin::Repo, None),
            file("council", WorkflowOrigin::Yours, Some(false)),
        ])
    }

    fn press(view: &mut LibraryView, code: KeyCode) -> ViewOutcome {
        let theme = Theme::dark();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        view.handle_key(KeyEvent::from(code), &ctx)
    }

    fn sent(outcome: ViewOutcome) -> (String, WorkflowAction) {
        match outcome {
            ViewOutcome::Send(UiCommand::WorkflowFile { name, action }) => (name, action),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn it_opens_on_the_workflow_in_use_and_copies_and_diffs_it() {
        let mut view = view();
        assert_eq!(view.library.list.selected, 1);
        let copied = sent(press(&mut view, KeyCode::Char('c')));
        assert_eq!(copied, ("orchestrator".to_owned(), WorkflowAction::Copy));
        let to_repo = sent(press(&mut view, KeyCode::Char('C')));
        assert_eq!(to_repo.1, WorkflowAction::CopyToProject);
        let diffed = sent(press(&mut view, KeyCode::Char('d')));
        assert_eq!(diffed.1, WorkflowAction::Diff);
    }

    #[test]
    fn enter_or_right_shows_the_text_without_asking_the_controller() {
        for code in [KeyCode::Enter, KeyCode::Right] {
            let mut view = view();
            match press(&mut view, code) {
                ViewOutcome::Open(pager) => {
                    let text = format!("{pager:?}");
                    assert!(text.contains("[[step]]"), "{text}");
                    assert!(text.contains("orchestrator ·"), "{text}");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn left_goes_back_out_of_the_library() {
        let mut view = view();
        assert!(matches!(
            press(&mut view, KeyCode::Left),
            ViewOutcome::Close
        ));
    }

    #[test]
    fn d_says_why_there_is_nothing_to_compare() {
        let mut view = view();
        press(&mut view, KeyCode::Up);
        assert!(matches!(
            press(&mut view, KeyCode::Char('d')),
            ViewOutcome::Consumed
        ));
        let said = view.library.said.clone().unwrap();
        assert!(said.contains("solo is the built-in"), "{said}");
        press(&mut view, KeyCode::Down);
        assert!(view.library.said.is_none());
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Char('d'));
        assert!(view.library.said.clone().unwrap().contains("no built-in"));
        press(&mut view, KeyCode::Down);
        let asked = sent(press(&mut view, KeyCode::Char('d')));
        assert_eq!(asked, ("council".to_owned(), WorkflowAction::Diff));
        assert!(view.library.said.is_none());
        let workflows = view.library.workflows.clone().unwrap();
        assert!(view.accept(&ViewData::WorkflowLibrary {
            workflows,
            refresh: true
        }));
        assert!(
            view.library
                .said
                .clone()
                .unwrap()
                .contains("matches the built-in")
        );
    }

    #[test]
    fn e_opens_your_files_page_and_says_how_to_edit_a_built_in() {
        let mut view = view();
        match press(&mut view, KeyCode::Char('e')) {
            ViewOutcome::Send(UiCommand::Slash {
                command: SlashCommand::WorkflowEdit { name },
            }) => assert_eq!(name, "orchestrator"),
            other => panic!("{other:?}"),
        }
        press(&mut view, KeyCode::Up);
        assert!(matches!(
            press(&mut view, KeyCode::Char('e')),
            ViewOutcome::Consumed
        ));
        let said = view.library.said.clone().unwrap();
        assert_eq!(said, "solo is built in. c copies it to edit");
    }

    #[test]
    fn a_fresh_list_keeps_the_selection() {
        let mut view = view();
        press(&mut view, KeyCode::Down);
        let mut fresh = vec![file("added", WorkflowOrigin::Yours, None)];
        fresh.extend(view.library.workflows.clone().unwrap());
        assert!(view.accept(&ViewData::WorkflowLibrary {
            workflows: fresh,
            refresh: true
        }));
        assert_eq!(view.library.selected().unwrap().name, "mine");
    }

    #[test]
    fn rows_note_the_one_in_use_and_your_edits() {
        let view = view();
        let lines: Vec<String> = view
            .library
            .lines(140, &Theme::dark())
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert!(lines[0].contains("Workflow"), "{lines:?}");
        assert!(lines[2].contains("in use · edited"), "{lines:?}");
        assert!(lines[2].contains("~/.argonauts/workflows/orchestrator.toml"));
        assert!(lines[1].contains("built in"), "{lines:?}");
    }

    #[test]
    fn a_workflow_named_like_a_command_says_so_in_the_warning_colour() {
        let mut clash = file("simplify", WorkflowOrigin::Repo, None);
        clash.problem = Some("/simplify is already a command".to_owned());
        let library = LibraryView::new(vec![file("solo", WorkflowOrigin::BuiltIn, None), clash]);
        let theme = Theme::dark();
        let lines = library.library.lines(140, &theme);
        let said = |line: &Line<'_>| -> String {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };
        assert!(said(&lines[2]).contains("/simplify is already a command"));
        assert!(!said(&lines[2]).contains("The simplify workflow"));
        let warned = lines[2]
            .spans
            .iter()
            .find(|span| span.content.contains("already a command"))
            .unwrap();
        assert_eq!(warned.style, theme.style(Role::Warning));
        assert!(said(&lines[1]).contains("The solo workflow"));
    }
}
