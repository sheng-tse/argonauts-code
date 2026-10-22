//! The page of a workflow file of steps, which `/workflow edit <name>` opens: the steps in
//! order as a dry run lists them, the keys of the one you open, and the check under them. A
//! change goes to the controller, which checks it, saves it to the file and sends the page
//! back as it reads now.

use argonauts_core::app::{UiCommand, ViewData};
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::text::shown_name;
use argonauts_core::workflow_page::{PageField, PageGroup, WorkflowChange, WorkflowPage};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::text::{sanitize_line, str_width};
use crate::theme::{Role, Theme};

use super::layout::left;
use super::list::{self, Column, Nav, fit_columns};
use super::pager::position_mark;
use super::panes::{self, Footer, Header, Marks, Side};
use super::{View, ViewCtx, ViewOutcome};

/// Cells between columns.
const GAP: usize = 2;

/// Cells a key's row is set in from its step's row.
const INSET: usize = 4;

/// How a key with no value reads.
const UNSET: &str = "—";

#[derive(Debug)]
pub struct WorkflowPageView {
    page: WorkflowPage,
    /// The group under the pointer: the file's own keys, or a step.
    group: usize,
    /// The key of that group under the pointer, when the pointer is among its keys.
    field: Option<usize>,
    /// The group whose keys show.
    open: Option<usize>,
    /// The line being typed for the key under the pointer.
    typing: Option<Typed>,
    /// `x` asks once more before a step goes.
    removing: bool,
    /// Where the pointer goes when the page comes back changed.
    follow: Option<Follow>,
    /// A word about the last key or the last change, in the footer until the next key.
    said: Option<String>,
    offset: usize,
    /// Rows the list had room for when it was last drawn.
    room: usize,
}

/// One line of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Group(usize),
    Field(usize, usize),
}

#[derive(Debug)]
enum Follow {
    /// The step of this name, wherever it is now.
    Named(String),
    /// The step added at this group, once the page has one more than `steps`.
    Added { at: usize, steps: usize },
}

/// A line being typed, with the cursor counted in characters.
#[derive(Debug, Default)]
struct Typed {
    text: Vec<char>,
    cursor: usize,
}

impl Typed {
    fn of(text: &str) -> Self {
        let text: Vec<char> = text.chars().collect();
        Self {
            cursor: text.len(),
            text,
        }
    }

    fn insert(&mut self, text: &str) {
        for c in text.chars() {
            // A value sits on one line of the file.
            let c = if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            };
            if !c.is_control() {
                self.text.insert(self.cursor, c);
                self.cursor += 1;
            }
        }
    }

    fn text(&self) -> String {
        self.text.iter().collect()
    }

    /// True when the key was one a line takes.
    fn key(&mut self, key: KeyEvent) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('u') if control => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('a') if control => self.cursor = 0,
            KeyCode::Char('e') if control => self.cursor = self.text.len(),
            KeyCode::Char(c) if !control && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert(c.encode_utf8(&mut [0; 4]));
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.text.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                self.text.remove(self.cursor);
            }
            KeyCode::Backspace | KeyCode::Delete => {}
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.text.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            _ => return false,
        }
        true
    }
}

impl WorkflowPageView {
    pub fn new(page: WorkflowPage) -> Self {
        // The first step, when there is one: the steps are what the page is for.
        let group = usize::from(page.groups.len() > 1);
        Self {
            said: page.said.clone(),
            page,
            group,
            field: None,
            open: None,
            typing: None,
            removing: false,
            follow: None,
            offset: 0,
            room: 1,
        }
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (index, group) in self.page.groups.iter().enumerate() {
            rows.push(Row::Group(index));
            if self.open == Some(index) {
                rows.extend((0..group.fields.len()).map(|field| Row::Field(index, field)));
            }
        }
        rows
    }

    fn at(&self) -> Row {
        match self.field {
            Some(field) => Row::Field(self.group, field),
            None => Row::Group(self.group),
        }
    }

    fn point_at(&mut self, row: Row) {
        (self.group, self.field) = match row {
            Row::Group(group) => (group, None),
            Row::Field(group, field) => (group, Some(field)),
        };
    }

    fn pointed_group(&self) -> Option<&PageGroup> {
        self.page.groups.get(self.group)
    }

    fn pointed_field(&self) -> Option<&PageField> {
        self.pointed_group()?.fields.get(self.field?)
    }

    /// The step under the pointer, or the one whose key is. `None` among the file's own keys.
    fn pointed_step(&self) -> Option<u32> {
        self.pointed_group()?.step
    }

    fn steps(&self) -> usize {
        self.page
            .groups
            .iter()
            .filter(|group| group.step.is_some())
            .count()
    }

    fn send(&self, change: WorkflowChange) -> ViewOutcome {
        ViewOutcome::Send(UiCommand::WorkflowPage {
            name: self.page.name.clone(),
            change,
        })
    }

    fn set(&self, value: String) -> ViewOutcome {
        let Some(field) = self.pointed_field() else {
            return ViewOutcome::Consumed;
        };
        self.send(WorkflowChange::Set {
            step: self.pointed_step(),
            key: field.key.clone(),
            value,
        })
    }

    fn navigate(&mut self, nav: Nav) {
        let rows = self.rows();
        let Some(now) = rows.iter().position(|row| *row == self.at()) else {
            return;
        };
        let last = rows.len() - 1;
        let next = match nav {
            Nav::By(delta) => now.saturating_add_signed(delta).min(last),
            Nav::Top => 0,
            Nav::Bottom => last,
        };
        self.point_at(rows[next]);
    }

    /// The next of a key's few values, or the one before. Unset is one of them when the key
    /// may be left out.
    fn cycle(&self, forward: bool) -> ViewOutcome {
        let Some(field) = self.pointed_field() else {
            return ViewOutcome::Consumed;
        };
        let mut values: Vec<&str> = Vec::new();
        if field.optional {
            values.push("");
        }
        values.extend(field.choices.iter().map(String::as_str));
        if values.is_empty() {
            return ViewOutcome::Consumed;
        }
        let count = values.len();
        let next = match values.iter().position(|value| *value == field.value) {
            Some(now) if forward => (now + 1) % count,
            Some(now) => (now + count - 1) % count,
            // A value that isn't one of them, such as a step that was renamed by hand.
            None => 0,
        };
        self.set(values[next].to_owned())
    }

    fn typing_key(&mut self, key: KeyEvent) -> ViewOutcome {
        match key.code {
            KeyCode::Esc => self.typing = None,
            KeyCode::Enter => {
                let typed = self.typing.take().map(|typed| typed.text());
                return self.set(typed.unwrap_or_default());
            }
            _ => {
                if let Some(typed) = self.typing.as_mut() {
                    typed.key(key);
                }
            }
        }
        ViewOutcome::Consumed
    }

    fn enter(&mut self) -> ViewOutcome {
        match self.pointed_field() {
            None => {
                self.open = (self.open != Some(self.group)).then_some(self.group);
                ViewOutcome::Consumed
            }
            Some(field) if field.choices.is_empty() => {
                self.typing = Some(Typed::of(&field.value));
                ViewOutcome::Consumed
            }
            Some(_) => self.cycle(true),
        }
    }

    fn add_step(&mut self) -> ViewOutcome {
        let after = self.pointed_step();
        let steps = self.steps();
        // Group 0 holds the file's own keys, so step n is group n + 1.
        let at = after.map_or(steps, |after| after as usize + 1) + 1;
        self.follow = Some(Follow::Added { at, steps });
        self.send(WorkflowChange::AddStep { after })
    }

    fn remove_step(&mut self, again: bool) -> ViewOutcome {
        let Some(step) = self.pointed_step() else {
            self.said = Some("The workflow's own keys stay. x removes a step".to_owned());
            return ViewOutcome::Consumed;
        };
        if again {
            return self.send(WorkflowChange::RemoveStep { step });
        }
        let name = self
            .pointed_group()
            .map(|group| clean(&group.title))
            .unwrap_or_default();
        self.said = Some(format!(
            "x again removes {name} from the file. Any other key keeps it"
        ));
        self.removing = true;
        ViewOutcome::Consumed
    }

    fn move_step(&mut self, up: bool) -> ViewOutcome {
        let Some(step) = self.pointed_step() else {
            return ViewOutcome::Consumed;
        };
        if let Some(group) = self.pointed_group() {
            self.follow = Some(Follow::Named(group.title.clone()));
        }
        self.send(WorkflowChange::MoveStep { step, up })
    }

    /// Moves the pointer to where the last change put its step, and keeps it on the page.
    fn settle(&mut self) {
        match self.follow.take() {
            Some(Follow::Named(name)) => {
                let found = self
                    .page
                    .groups
                    .iter()
                    .position(|group| group.step.is_some() && group.title == name);
                if let Some(found) = found {
                    if self.open == Some(self.group) {
                        self.open = Some(found);
                    }
                    self.group = found;
                }
            }
            Some(Follow::Added { at, steps }) if self.steps() == steps + 1 => {
                self.group = at;
                self.field = None;
                self.open = Some(at);
            }
            _ => {}
        }
        let last = self.page.groups.len().saturating_sub(1);
        self.group = self.group.min(last);
        if self.open.is_some_and(|open| open > last) {
            self.open = None;
        }
        let fields = self.pointed_group().map_or(0, |group| group.fields.len());
        self.field = self
            .field
            .filter(|_| self.open == Some(self.group) && fields > 0)
            .map(|field| field.min(fields - 1));
    }

    fn group_columns(&self, width: usize) -> Vec<Column> {
        let groups = &self.page.groups;
        let cells = |f: &dyn Fn(&PageGroup) -> String| groups.iter().map(f).collect();
        let named = |group: &PageGroup| match group.step {
            Some(step) => format!("{} {}", step + 1, clean(&group.title)),
            None => clean(&group.title),
        };
        fit_columns(
            vec![
                Column::new("step", cells(&named)),
                Column::new("role", cells(&|group| role_shown(&clean(&group.role)))).keep(2),
                Column::new("who would take it", cells(&|group| clean(&group.who))).keep(1),
                Column::new("how", cells(&|group| clean(&group.how))).flex(),
            ],
            width,
            GAP,
        )
    }

    fn group_line(&self, index: usize, columns: &[Column], theme: &Theme) -> Line<'static> {
        let pointed = self.at() == Row::Group(index);
        let mut spans = vec![pointer(pointed, theme)];
        for (place, column) in columns.iter().enumerate() {
            if place > 0 {
                spans.push(Span::raw(" ".repeat(GAP)));
            }
            let style = match (place, pointed) {
                (0, true) => theme.style(Role::Question),
                (0, false) => theme.style(Role::Text),
                _ => theme.style(Role::Dim),
            };
            spans.push(Span::styled(column.cell(&column.cells[index]), style));
        }
        Line::from(spans)
    }

    fn field_line(
        &self,
        field: &PageField,
        pointed: bool,
        label_width: usize,
        width: usize,
        theme: &Theme,
    ) -> Line<'static> {
        let label_style = if pointed {
            theme.style(Role::Question)
        } else {
            theme.style(Role::Text)
        };
        let mut spans = vec![
            pointer(pointed, theme),
            Span::raw(" ".repeat(INSET)),
            Span::styled(left(&field.label, label_width), label_style),
            Span::raw(" ".repeat(GAP)),
        ];
        let room = width.saturating_sub(2 + INSET + label_width + GAP);
        match (&self.typing, pointed) {
            (Some(typed), true) => spans.extend(typed_spans(typed, room, theme)),
            _ if field.choices.is_empty() => spans.push(written(&field.value, theme)),
            _ => spans.extend(choice_spans(field, theme)),
        }
        Line::from(spans)
    }

    fn list_lines(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let dim = theme.style(Role::Dim);
        let columns = self.group_columns(width.saturating_sub(2));
        let mut heading = vec![Span::raw("  ")];
        for (place, column) in columns.iter().enumerate() {
            if place > 0 {
                heading.push(Span::raw(" ".repeat(GAP)));
            }
            heading.push(Span::styled(column.cell(column.heading), dim));
        }
        let mut lines = vec![Line::from(heading)];
        for row in self.rows() {
            lines.push(match row {
                Row::Group(index) => self.group_line(index, &columns, theme),
                Row::Field(group, field) => {
                    let fields = &self.page.groups[group].fields;
                    let label_width = fields
                        .iter()
                        .map(|field| str_width(&field.label))
                        .max()
                        .unwrap_or(0);
                    let pointed = self.at() == row;
                    self.field_line(&fields[field], pointed, label_width, width, theme)
                }
            });
        }
        lines
    }

    /// What sits under the list: what the key under the pointer does, then the check.
    fn under_lines(&self, theme: &Theme) -> Vec<Line<'static>> {
        let dim = theme.style(Role::Dim);
        let marks = Marks::for_theme(theme);
        let mut lines = Vec::new();
        if let Some(field) = self.pointed_field() {
            lines.push(Line::from(Span::styled(clean(&field.about), dim)));
        }
        let check = &self.page.check;
        let (mark, mark_style, text_style) = if check.valid {
            (marks.done, theme.style(Role::Success), dim)
        } else {
            (
                marks.failed,
                theme.style(Role::Danger),
                theme.style(Role::Text),
            )
        };
        for line in &check.lines {
            lines.push(Line::from(vec![
                Span::styled(format!("{mark} "), mark_style),
                Span::styled(clean(line), text_style),
            ]));
        }
        lines
    }

    fn footer(&self, theme: &Theme) -> Footer {
        if let Some(said) = &self.said {
            let role = if said.starts_with("Not saved") {
                Role::Warn
            } else {
                Role::Dim
            };
            return Footer::Line(Line::from(Span::styled(
                sanitize_line(said).into_owned(),
                theme.style(role),
            )));
        }
        if self.typing.is_some() {
            return Footer::hints(&[("⏎ save", 0), ("esc leave it as it was", 0)]);
        }
        let act = match self.pointed_field() {
            None if self.open == Some(self.group) => "⏎ fold",
            None => "⏎ its keys",
            Some(field) if field.choices.is_empty() => "⏎ type",
            Some(_) => "←  →  change",
        };
        Footer::hints(&[
            ("↑↓  move", 0),
            (act, 0),
            ("a add step", 5),
            ("x remove", 4),
            ("shift+↑↓  reorder", 2),
            ("e editor", 3),
            (panes::ESC_BACK, 0),
        ])
    }
}

impl View for WorkflowPageView {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let marks = Marks::for_theme(theme);
        let header = Header {
            title: Line::from(vec![
                Span::styled("Workflows", theme.style(Role::Text)),
                Span::styled(format!(" {} ", marks.crumb), theme.style(Role::Dim)),
                Span::styled(
                    shown_name(&clean(&self.page.name)),
                    theme.style(Role::Label),
                ),
            ]),
            description: self.page.about.clone(),
            summary: clean(&self.page.path),
        };
        let panes = panes::split(area, 0);
        panes::draw_header(buf, panes.header, &header, theme);
        panes::draw_box(buf, &panes, ["", ""], Side::Right, theme);

        let inner = panes.right;
        let under = self.under_lines(theme);
        // The check keeps its rows, and a blank one parts it from the list.
        let under_rows = (under.len() as u16 + 1).min(inner.height.saturating_sub(2));
        let list = Rect {
            height: inner.height - under_rows,
            ..inner
        };
        let lines = self.list_lines(usize::from(list.width), theme);
        let rows = lines.len() - 1;
        self.room = usize::from(list.height).saturating_sub(1).max(1);
        let pointed = self
            .rows()
            .iter()
            .position(|row| *row == self.at())
            .unwrap_or(0);
        if pointed < self.offset {
            self.offset = pointed;
        } else if pointed >= self.offset + self.room {
            self.offset = pointed + 1 - self.room;
        }
        self.offset = self.offset.min(rows.saturating_sub(self.room));
        let mut shown = vec![lines[0].clone()];
        shown.extend(lines.iter().skip(1 + self.offset).cloned());
        panes::put_lines(buf, list, &shown);
        if under_rows > 1 {
            let under_area = Rect {
                y: list.bottom() + 1,
                height: under_rows - 1,
                ..inner
            };
            panes::put_lines(buf, under_area, &under);
        }

        let mark = position_mark(self.offset, self.room, rows, marks.down);
        panes::draw_footer(buf, panes.footer, self.footer(theme), &mark, theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        self.said = None;
        let removing = std::mem::take(&mut self.removing);
        if self.typing.is_some() {
            return self.typing_key(key);
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return ViewOutcome::Ignored;
        }
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            match key.code {
                KeyCode::Up => return self.move_step(true),
                KeyCode::Down => return self.move_step(false),
                _ => {}
            }
        }
        match key.code {
            KeyCode::Char('K') => return self.move_step(true),
            KeyCode::Char('J') => return self.move_step(false),
            _ => {}
        }
        // `h` and `l` stand for ← and → here, where they change a value.
        let key = match (list::vim(ctx.state), key.code) {
            (true, KeyCode::Char('h')) => KeyEvent::from(KeyCode::Left),
            (true, KeyCode::Char('l')) => KeyEvent::from(KeyCode::Right),
            (vim, _) => list::standard(key, vim, KeyCode::Esc),
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return ViewOutcome::Close,
            KeyCode::Enter | KeyCode::Char(' ') => return self.enter(),
            KeyCode::Right if self.pointed_field().is_some() => return self.cycle(true),
            KeyCode::Left if self.pointed_field().is_some() => return self.cycle(false),
            KeyCode::Right => {
                self.open = Some(self.group);
                return ViewOutcome::Consumed;
            }
            KeyCode::Left => {
                self.open = None;
                return ViewOutcome::Consumed;
            }
            KeyCode::Char('a') => return self.add_step(),
            KeyCode::Char('x') => return self.remove_step(removing),
            KeyCode::Char('e') => return self.send(WorkflowChange::OpenInEditor),
            _ => {}
        }
        match list::nav_key(&key, self.room) {
            Some(nav) => {
                self.navigate(nav);
                ViewOutcome::Consumed
            }
            None => ViewOutcome::Ignored,
        }
    }

    fn handle_paste(&mut self, text: &str, _ctx: &ViewCtx<'_>) -> ViewOutcome {
        match self.typing.as_mut() {
            Some(typed) => {
                typed.insert(text);
                ViewOutcome::Consumed
            }
            None => ViewOutcome::Ignored,
        }
    }

    fn accept(&mut self, data: &ViewData) -> bool {
        let ViewData::WorkflowPage { page } = data else {
            return false;
        };
        if page.name != self.page.name {
            return false;
        }
        self.page = (**page).clone();
        self.said.clone_from(&self.page.said);
        self.typing = None;
        self.settle();
        true
    }
}

fn pointer(pointed: bool, theme: &Theme) -> Span<'static> {
    if pointed {
        let mark = Marks::for_theme(theme).selected;
        Span::styled(format!("{mark} "), theme.style(Role::Question))
    } else {
        Span::raw("  ")
    }
}

/// A typed key's value as written, or the unset mark.
fn written(value: &str, theme: &Theme) -> Span<'static> {
    if value.is_empty() {
        Span::styled(UNSET, theme.style(Role::Dim))
    } else {
        Span::styled(clean(value), theme.style(Role::Text))
    }
}

/// A step's role as the page names it: a crew role by its people, such as `Workers` for
/// `worker`, with a capital.
fn role_shown(role: &str) -> String {
    shown_name(CrewRole::parse(role).map_or(role, |crew| crew.plural()))
}

/// Every value a key takes, side by side, the one written standing out.
fn choice_spans(field: &PageField, theme: &Theme) -> Vec<Span<'static>> {
    let chosen = theme.style(Role::Question).add_modifier(Modifier::BOLD);
    let dim = theme.style(Role::Dim);
    let mut values: Vec<(&str, &str)> = Vec::new();
    if field.optional {
        values.push(("", UNSET));
    }
    // A role is named on screen by its people with a capital, and kept as typed.
    let roles = field.key == "role";
    let shown: Vec<String> = field
        .choices
        .iter()
        .map(|choice| {
            if roles {
                role_shown(choice)
            } else {
                choice.clone()
            }
        })
        .collect();
    values.extend(
        field
            .choices
            .iter()
            .zip(&shown)
            .map(|(choice, shown)| (choice.as_str(), shown.as_str())),
    );
    let mut spans = Vec::new();
    // A value the key doesn't take, as a broken file may hold, is shown for what it is.
    if !values.iter().any(|(value, _)| *value == field.value) {
        spans.push(Span::styled(clean(&field.value), theme.style(Role::Danger)));
        spans.push(Span::raw(" ".repeat(GAP)));
    }
    for (place, (value, shown)) in values.iter().enumerate() {
        if place > 0 {
            spans.push(Span::raw(" ".repeat(GAP)));
        }
        let style = if *value == field.value { chosen } else { dim };
        spans.push(Span::styled(clean(shown), style));
    }
    spans
}

/// The line being typed with its cursor, the end of a long one kept in view.
fn typed_spans(typed: &Typed, room: usize, theme: &Theme) -> Vec<Span<'static>> {
    let before: String = typed.text[..typed.cursor].iter().collect();
    let after: String = typed.text[typed.cursor..].iter().collect();
    let cursor = Marks::for_theme(theme).cursor;
    let before = list::tail(&before, room.saturating_sub(str_width(cursor) + 1));
    vec![
        Span::styled(before, theme.style(Role::Text)),
        Span::styled(cursor, theme.style(Role::Label)),
        Span::styled(after, theme.style(Role::Text)),
    ]
}

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::workflow_page::PageCheck;

    use super::*;
    use crate::state::AppState;

    fn field(key: &str, value: &str, choices: &[&str], optional: bool) -> PageField {
        PageField {
            key: key.to_owned(),
            label: key.replace('_', " "),
            value: value.to_owned(),
            choices: choices.iter().map(|choice| (*choice).to_owned()).collect(),
            optional,
            about: format!("What {key} does."),
        }
    }

    fn step(index: u32, name: &str, role: &str, back: &str) -> PageGroup {
        PageGroup {
            step: Some(index),
            title: name.to_owned(),
            role: role.to_owned(),
            who: "GPT 5.6 on Codex".to_owned(),
            how: "one per task".to_owned(),
            fields: vec![
                field("name", name, &[], false),
                field(
                    "role",
                    role,
                    &["orchestrator", "worker", "reviewer", "judge"],
                    false,
                ),
                field("does", "", &[], true),
                field("send_back_to", back, &["Plan", "Build"], true),
            ],
        }
    }

    fn page() -> WorkflowPage {
        WorkflowPage {
            name: "spec-first".to_owned(),
            path: ".argonauts/workflows/spec-first.toml".to_owned(),
            about: "Write the spec first.".to_owned(),
            check: PageCheck {
                valid: true,
                lines: vec!["the file is valid · about 7 agents".to_owned()],
            },
            groups: vec![
                PageGroup {
                    step: None,
                    title: "Workflow".to_owned(),
                    role: String::new(),
                    who: String::new(),
                    how: "the steps run exactly as written · no goal".to_owned(),
                    fields: vec![field("adapt", "false", &["false", "true"], false)],
                },
                step(0, "Plan", "orchestrator", ""),
                step(1, "Build", "worker", ""),
                step(2, "Review", "reviewer", "Build"),
            ],
            said: None,
            refresh: false,
        }
    }

    fn key(view: &mut WorkflowPageView, key: KeyEvent) -> ViewOutcome {
        let theme = Theme::dark();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        view.handle_key(key, &ctx)
    }

    fn press(view: &mut WorkflowPageView, code: KeyCode) -> ViewOutcome {
        key(view, KeyEvent::from(code))
    }

    fn typed(view: &mut WorkflowPageView, text: &str) {
        for c in text.chars() {
            press(view, KeyCode::Char(c));
        }
    }

    fn change(outcome: ViewOutcome) -> WorkflowChange {
        match outcome {
            ViewOutcome::Send(UiCommand::WorkflowPage { name, change }) => {
                assert_eq!(name, "spec-first");
                change
            }
            other => panic!("{other:?}"),
        }
    }

    fn text(view: &WorkflowPageView) -> Vec<String> {
        view.list_lines(100, &Theme::dark())
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn it_opens_on_the_first_step_with_every_step_folded() {
        let view = WorkflowPageView::new(page());
        let lines = text(&view);
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert!(lines[0].contains("who would take it"), "{lines:?}");
        assert!(lines[1].starts_with("  Workflow"), "{lines:?}");
        assert!(lines[2].starts_with("❯ 1 Plan"), "{lines:?}");
        assert!(lines[4].contains("3 Review"), "{lines:?}");
    }

    /// The pointer and the chosen step are lavender, as on every list.
    #[test]
    fn the_chosen_step_is_lavender() {
        let view = WorkflowPageView::new(page());
        let theme = Theme::dark();
        let lines = view.list_lines(100, &theme);
        let chosen = &lines[2].spans;
        assert!(chosen[0].content.starts_with('❯'), "{chosen:?}");
        assert_eq!(chosen[0].style, theme.style(Role::Question));
        assert_eq!(chosen[1].style, theme.style(Role::Question));
    }

    #[test]
    fn enter_shows_a_steps_keys_and_the_arrows_change_one_with_few_values() {
        let mut view = WorkflowPageView::new(page());
        press(&mut view, KeyCode::Enter);
        let lines = text(&view);
        assert_eq!(lines.len(), 9, "{lines:?}");
        assert!(lines[4].contains("    role  "), "{lines:?}");
        assert!(
            lines[4].ends_with("Orchestrator  Workers  Reviewers  Judge"),
            "{lines:?}"
        );
        assert!(
            lines[5].contains("does") && lines[5].ends_with("  —"),
            "{lines:?}"
        );
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Down);
        let next = change(press(&mut view, KeyCode::Right));
        assert_eq!(
            next,
            WorkflowChange::Set {
                step: Some(0),
                key: "role".to_owned(),
                value: "worker".to_owned(),
            }
        );
        let before = change(press(&mut view, KeyCode::Left));
        assert!(
            matches!(&before, WorkflowChange::Set { value, .. } if value == "judge"),
            "{before:?}"
        );
        // Enter on the step's own row folds it again.
        press(&mut view, KeyCode::Up);
        press(&mut view, KeyCode::Up);
        press(&mut view, KeyCode::Enter);
        assert_eq!(text(&view).len(), 5);
    }

    #[test]
    fn a_key_that_may_be_left_out_goes_round_through_unset() {
        let mut view = WorkflowPageView::new(page());
        press(&mut view, KeyCode::End);
        press(&mut view, KeyCode::Enter);
        press(&mut view, KeyCode::End);
        let lines = text(&view);
        assert!(
            lines
                .last()
                .unwrap()
                .ends_with("send back to  —  Plan  Build"),
            "{lines:?}"
        );
        // Build is the last of its values, so the next one is unset.
        let next = change(press(&mut view, KeyCode::Right));
        assert_eq!(
            next,
            WorkflowChange::Set {
                step: Some(2),
                key: "send_back_to".to_owned(),
                value: String::new(),
            }
        );
    }

    #[test]
    fn enter_on_a_typed_key_takes_a_line_and_esc_leaves_it_as_it_was() {
        let mut view = WorkflowPageView::new(page());
        press(&mut view, KeyCode::Enter);
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Enter);
        // Letters that are keys of the page are text while a line is typed.
        typed(&mut view, "x");
        press(&mut view, KeyCode::Home);
        typed(&mut view, "a");
        press(&mut view, KeyCode::Delete);
        press(&mut view, KeyCode::End);
        press(&mut view, KeyCode::Backspace);
        typed(&mut view, "q");
        let set = change(press(&mut view, KeyCode::Enter));
        assert_eq!(
            set,
            WorkflowChange::Set {
                step: Some(0),
                key: "name".to_owned(),
                value: "alanq".to_owned(),
            }
        );
        press(&mut view, KeyCode::Enter);
        typed(&mut view, "zzz");
        assert!(matches!(
            press(&mut view, KeyCode::Esc),
            ViewOutcome::Consumed
        ));
        let name = &text(&view)[3];
        assert!(name.contains("name") && name.ends_with("  Plan"), "{name}");
        assert!(matches!(press(&mut view, KeyCode::Esc), ViewOutcome::Close));
    }

    #[test]
    fn a_paste_goes_into_the_line_being_typed_on_one_line() {
        let mut view = WorkflowPageView::new(page());
        let theme = Theme::dark();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        assert!(matches!(
            view.handle_paste("ignored", &ctx),
            ViewOutcome::Ignored
        ));
        press(&mut view, KeyCode::Enter);
        for _ in 0..3 {
            press(&mut view, KeyCode::Down);
        }
        press(&mut view, KeyCode::Enter);
        view.handle_paste("Write the spec\nfor {request}", &ctx);
        let set = change(press(&mut view, KeyCode::Enter));
        assert!(
            matches!(&set, WorkflowChange::Set { key, value, .. }
                if key == "does" && value == "Write the spec for {request}"),
            "{set:?}"
        );
    }

    #[test]
    fn a_adds_a_step_after_this_one_and_the_pointer_goes_to_it() {
        let mut view = WorkflowPageView::new(page());
        let added = change(press(&mut view, KeyCode::Char('a')));
        assert_eq!(added, WorkflowChange::AddStep { after: Some(0) });
        let mut fresh = page();
        fresh.groups.insert(2, step(1, "Step 4", "worker", ""));
        fresh.refresh = true;
        assert!(view.accept(&ViewData::WorkflowPage {
            page: Box::new(fresh)
        }));
        let lines = text(&view);
        assert!(lines[3].starts_with("❯ 2 Step 4"), "{lines:?}");
        // It opens, ready for its name.
        assert!(
            lines[4].contains("name") && lines[4].ends_with("  Step 4"),
            "{lines:?}"
        );

        // Among the file's own keys there is no step to add after, so it goes last.
        let mut view = WorkflowPageView::new(page());
        press(&mut view, KeyCode::Up);
        let added = change(press(&mut view, KeyCode::Char('a')));
        assert_eq!(added, WorkflowChange::AddStep { after: None });
    }

    #[test]
    fn x_asks_once_more_before_a_step_goes() {
        let mut view = WorkflowPageView::new(page());
        assert!(matches!(
            press(&mut view, KeyCode::Char('x')),
            ViewOutcome::Consumed
        ));
        let said = view.said.clone().unwrap();
        assert!(said.contains("x again removes Plan"), "{said}");
        // Any other key keeps it.
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Char('x'));
        let removed = change(press(&mut view, KeyCode::Char('x')));
        assert_eq!(removed, WorkflowChange::RemoveStep { step: 1 });

        press(&mut view, KeyCode::Home);
        press(&mut view, KeyCode::Char('x'));
        assert!(view.said.clone().unwrap().contains("own keys stay"));
    }

    #[test]
    fn shift_arrows_move_a_step_and_the_pointer_follows_it() {
        let mut view = WorkflowPageView::new(page());
        press(&mut view, KeyCode::Down);
        let moved = change(key(
            &mut view,
            KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT),
        ));
        assert_eq!(moved, WorkflowChange::MoveStep { step: 1, up: true });
        let mut fresh = page();
        fresh.groups.swap(1, 2);
        fresh.groups[1].step = Some(0);
        fresh.groups[2].step = Some(1);
        assert!(view.accept(&ViewData::WorkflowPage {
            page: Box::new(fresh)
        }));
        assert!(text(&view)[2].starts_with("❯ 1 Build"), "{:?}", text(&view));
        let down = change(press(&mut view, KeyCode::Char('J')));
        assert_eq!(down, WorkflowChange::MoveStep { step: 0, up: false });
    }

    #[test]
    fn what_the_change_did_shows_until_the_next_key_and_another_page_is_left_alone() {
        let mut view = WorkflowPageView::new(page());
        let mut fresh = page();
        fresh.said = Some("Not saved: count takes a whole number".to_owned());
        assert!(view.accept(&ViewData::WorkflowPage {
            page: Box::new(fresh)
        }));
        assert!(view.said.clone().unwrap().starts_with("Not saved"));
        press(&mut view, KeyCode::Down);
        assert!(view.said.is_none());
        let mut other = page();
        other.name = "another".to_owned();
        assert!(!view.accept(&ViewData::WorkflowPage {
            page: Box::new(other)
        }));
        let opened = change(press(&mut view, KeyCode::Char('e')));
        assert_eq!(opened, WorkflowChange::OpenInEditor);
    }
}
