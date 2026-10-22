//! `/approvals`: autopilot first, its one switch and who answers questions in your place; then
//! who checks, the judge models and their rules file; then each approval's mode, who
//! decides, its rules, what happens otherwise, and what a hard limit does in autopilot, beside
//! its auto preview record; and the spot checks you mark right or wrong. A setting that takes a
//! model opens the model picker. Every `approvals.` key is a safety key, so an edit goes through
//! the scope question and then the controller's confirm, and no row carries the `!` that marks one
//! in `/config`. Rows go by name, as `/permissions` does.

use argonauts_core::RunId;
use argonauts_core::app::{SettingRow, UiCommand, ViewData, ViewRequest};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::{ApprovalId, ModelRef, RoleName};
use argonauts_core::policy::{ApprovalRow, ApprovalsView as ApprovalsData};
use argonauts_core::settings::approvals::DecideWith;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::flow::{self, Asked};
use super::models;
use super::panel::{draw_editor, edit_key, edit_paste};
use super::table::{Cells, Widths, cells_line, heading_line};
use super::words::{capitalized, freshness, layer_words, row_value_words};
use crate::bottom::Picker;
use crate::questions::{joined, spot_check_evidence, spot_check_line};
use crate::state::{AppState, DecisionRecord};
use crate::text::{sanitize_line, str_width, wrap_text};
use crate::theme::{Role, Theme};
use crate::views::layout::cut;
use crate::views::list;
use crate::views::panes::{self, Footer, Header, Side};
use crate::views::{View, ViewCtx, ViewOutcome};

/// Where rows start, after the pointer.
const INDENT: usize = 2;

const AUTOPILOT: &str = "approvals.autopilot";
const ANSWERED_BY: &str = "approvals.answers";
/// What decides when you don't, right under the switch: who answers, what Esc on a question
/// does, and how often an agent that asks is told to decide.
const WITHOUT_YOU: [&str; 4] = [
    AUTOPILOT,
    ANSWERED_BY,
    "approvals.when_declined",
    "approvals.ask_limit",
];
/// Who settles what your rules can't. It heads the `Who checks` group.
const DECIDES: &str = "approvals.decides";
/// The judge when your crew names none: the policy picks one for each piece of work.
const UNNAMED_JUDGE: &str = "cheapest, other provider";

#[derive(Debug)]
pub struct ApprovalsView {
    pub data: ApprovalsData,
    /// The settings under `approvals.`, which the view edits.
    pub rows: Vec<SettingRow>,
    /// The pointer, as a position among the rows you can act on.
    pub selected: usize,
    /// The first line on screen.
    offset: usize,
    /// Spot checks in the order they showed here. One you mark stays to show your mark.
    spots: Vec<Spot>,
    editor: Option<Picker>,
}

#[derive(Debug)]
struct Spot {
    run: RunId,
    id: ApprovalId,
    /// Names the row when this screen didn't see the call.
    run_title: String,
    /// `None` when the call was made before this screen attached.
    record: Option<DecisionRecord>,
    /// Whether you said the call was right.
    marked: Option<bool>,
}

/// One line of the view.
enum Item {
    Heading {
        name: String,
        note: String,
    },
    Info(String),
    /// What the settings check found, before it's wrapped to the view's width.
    Warning(String),
    /// One wrapped line of a warning.
    Wrapped(Line<'static>),
    Setting {
        index: usize,
        label: String,
    },
    /// An index into `spots`.
    Spot(usize),
    /// A dim line under the row above it: a spot check's call, or the modes a setting covers.
    Detail(String),
    Blank,
}

impl Item {
    fn selectable(&self) -> bool {
        matches!(self, Item::Setting { .. } | Item::Spot(_))
    }
}

impl ApprovalsView {
    pub fn new(data: ApprovalsData) -> Self {
        Self {
            data,
            rows: Vec::new(),
            selected: 0,
            offset: 0,
            spots: Vec::new(),
            editor: None,
        }
    }

    pub fn with_rows(rows: Vec<SettingRow>) -> Self {
        Self {
            rows,
            ..Self::new(ApprovalsData::default())
        }
    }

    /// Asks for the rows under `approvals.`, which the view edits.
    pub fn rows_request() -> UiCommand {
        UiCommand::Request {
            view: ViewRequest::Settings {
                section: Some("approvals".to_owned()),
            },
        }
    }

    /// Lists the spot checks still waiting, keeping the ones you marked here in place. One
    /// answered on its question leaves.
    fn sync_spots(&mut self, state: &AppState) {
        let waits = |spot: &Spot| {
            state
                .runs
                .get(&spot.run)
                .is_some_and(|run| run.spot_checks.contains(&spot.id))
        };
        self.spots
            .retain(|spot| spot.marked.is_some() || waits(spot));
        for run in state.runs.values() {
            for id in &run.spot_checks {
                let listed = self
                    .spots
                    .iter_mut()
                    .find(|spot| spot.run == run.id && spot.id == *id);
                match listed {
                    Some(spot) if spot.record.is_none() => {
                        spot.record = run.decision(id).cloned();
                    }
                    Some(_) => {}
                    None => self.spots.push(Spot {
                        run: run.id.clone(),
                        id: id.clone(),
                        run_title: run.title.clone(),
                        record: run.decision(id).cloned(),
                        marked: None,
                    }),
                }
            }
        }
    }

    fn items(&self, names: &DisplayNames) -> Vec<Item> {
        let mut items = Vec::new();
        // Autopilot comes first: its switch and who answers in your place decide whether a run
        // asks you anything at all.
        let top: Vec<usize> = WITHOUT_YOU
            .iter()
            .filter_map(|key| self.rows.iter().position(|row| row.key == *key))
            .collect();
        if !top.is_empty() {
            items.push(Item::Heading {
                name: "Autopilot".to_owned(),
                note: self.autopilot_note(),
            });
            for warning in &self.data.warnings {
                items.push(Item::Warning(warning.clone()));
            }
            for index in top {
                let row = &self.rows[index];
                let leaf = row.key.trim_start_matches("approvals.");
                items.push(Item::Setting {
                    index,
                    label: setting_name(leaf),
                });
                if row.key == ANSWERED_BY {
                    items.push(Item::Detail(answerer_modes(&row.value).to_owned()));
                }
            }
            items.push(Item::Blank);
        }

        // Who checks comes next, since every approval below that asks a model asks it.
        let judge = self
            .data
            .judge
            .as_ref()
            .map(|worker| names.worker(worker))
            // Unset, the policy picks one for each piece of work it judges.
            .unwrap_or_else(|| "the cheapest model from another provider".to_owned());
        self.push_group(&mut items, "Who checks".to_owned(), judge, "judge");

        let mut approvals: Vec<String> = self.data.points.iter().map(kind_name).collect();
        for row in &self.rows {
            if let Some((group, _)) = split_key(&row.key)
                && group != "judge"
                && !approvals.iter().any(|known| known == group)
            {
                approvals.push(group.to_owned());
            }
        }
        for name in &approvals {
            let point = self
                .data
                .points
                .iter()
                .find(|point| kind_name(point) == *name);
            let mut heading = words_of(name);
            if let Some(point) = point {
                heading = format!(
                    "{heading} · {} · {}",
                    point.mode.label(),
                    decider_words(point.decide_with)
                );
            }
            let note = point.map(record_words).unwrap_or_default();
            self.push_group(&mut items, heading, note, name);
        }

        let other: Vec<Item> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.key.starts_with("approvals.")
                    && !WITHOUT_YOU.contains(&row.key.as_str())
                    && split_key(&row.key).is_none()
            })
            .map(|(index, row)| Item::Setting {
                index,
                label: setting_name(row.key.trim_start_matches("approvals.")),
            })
            .collect();
        if !other.is_empty() {
            items.push(Item::Heading {
                name: "Everything else".to_owned(),
                note: String::new(),
            });
            items.extend(other);
            items.push(Item::Blank);
        }

        let waiting = self.data.spot_checks_waiting;
        items.push(Item::Heading {
            name: "Spot checks".to_owned(),
            note: match waiting {
                0 => String::new(),
                1 => "1 waiting".to_owned(),
                n => format!("{n} waiting"),
            },
        });
        for (at, spot) in self.spots.iter().enumerate() {
            items.push(Item::Spot(at));
            let call = spot
                .record
                .as_ref()
                .and_then(|record| spot_check_line(&spot.run, record, names));
            if let Some(call) = call {
                items.push(Item::Detail(call));
            }
        }
        if self.spots.is_empty() {
            items.push(Item::Info(if waiting == 0 {
                "None waiting".to_owned()
            } else {
                "They show here once their run is on screen".to_owned()
            }));
        }
        items
    }

    fn switch_on(&self) -> bool {
        self.rows
            .iter()
            .find(|row| row.key == AUTOPILOT)
            .is_some_and(|row| row.value.trim() == "true")
    }

    /// Whether autopilot covers every approval, by the switch or by each one's own mode, and
    /// what still waits for you: an approval you keep, or one that waits at a hard limit.
    fn autopilot_note(&self) -> String {
        let run_points: Vec<&ApprovalRow> = self
            .data
            .points
            .iter()
            .filter(|point| kind_name(point) != "workflow_promotion")
            .collect();
        let each = !run_points.is_empty()
            && run_points.iter().all(|point| {
                point.mode == argonauts_core::settings::approvals::ApprovalMode::Autopilot
            });
        if !self.switch_on() && !each {
            return "off: each approval keeps its own mode".to_owned();
        }
        let mut waits: Vec<String> = run_points
            .iter()
            .filter(|point| !point.mode.goes_ahead())
            .map(|point| words_of(&kind_name(point)))
            .collect();
        for row in &self.rows {
            if let Some((group, "at_hard_limit")) = split_key(&row.key)
                && row_value_words(&row.key, &row.value) == "wait"
            {
                waits.push(format!("{}'s hard limit", words_of(group)));
            }
        }
        match waits.as_slice() {
            [] => "on: nothing waits for you".to_owned(),
            [one] => format!("on: only {one} waits for you"),
            many => format!("on: only {} wait for you", joined(many)),
        }
    }

    /// A row in words, saying where the autopilot switch decides in its place.
    fn cells(&self, label: &str, row: &SettingRow, names: &DisplayNames) -> Cells {
        let mut cells = cells(label, row, names);
        if let Some(note) = self.switch_note(row) {
            cells.value = note;
        }
        // Left to the judge, the row names the one your crew set, when it set one.
        if row.key == DECIDES
            && cells.value == UNNAMED_JUDGE
            && let Some(judge) = &self.data.judge
        {
            cells.value = names.worker(judge);
        }
        cells
    }

    /// An approval's mode the switch overrides: auto runs as autopilot while it's on.
    fn switch_note(&self, row: &SettingRow) -> Option<String> {
        let (group, "mode") = split_key(&row.key)? else {
            return None;
        };
        if group == "workflow_promotion" || !self.switch_on() {
            return None;
        }
        // A mode you keep stays yours, and the heading names it.
        let words = row_value_words(&row.key, &row.value);
        (words == "auto").then(|| "autopilot by the switch".to_owned())
    }

    /// A heading, then the settings under `approvals.<group>.`.
    fn push_group(&self, items: &mut Vec<Item>, heading: String, note: String, group: &str) {
        items.push(Item::Heading {
            name: heading,
            note,
        });
        let before = items.len();
        for (index, row) in self.rows.iter().enumerate() {
            if let Some((name, rest)) = split_key(&row.key)
                && name == group
            {
                items.push(Item::Setting {
                    index,
                    label: setting_name(rest),
                });
            }
        }
        if items.len() == before {
            items.push(Item::Info(
                "Its settings show here once they arrive".to_owned(),
            ));
        }
        items.push(Item::Blank);
    }

    /// Sends your answer once. A second one would count twice in the autonomy record.
    fn mark(&mut self, at: usize, right: bool) -> ViewOutcome {
        let Some(spot) = self.spots.get_mut(at) else {
            return ViewOutcome::Consumed;
        };
        if spot.marked.is_some() {
            return ViewOutcome::Notice("You already marked this one.".to_owned());
        }
        spot.marked = Some(right);
        ViewOutcome::Send(UiCommand::SpotCheck {
            run: spot.run.clone(),
            id: spot.id.clone(),
            correct: right,
        })
    }

    fn evidence(&self, at: usize, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let Some(spot) = self.spots.get(at) else {
            return ViewOutcome::Consumed;
        };
        match &spot.record {
            Some(record) => {
                let offset = ctx.state.clock_offset();
                spot_check_evidence(&spot.run, record, offset, &ctx.state.names)
            }
            None => ViewOutcome::Notice(
                "Decided before you attached, so its evidence isn't here.".to_owned(),
            ),
        }
    }

    fn spot_line(&self, at: usize, pointed: bool, width: usize, theme: &Theme) -> Line<'static> {
        let Some(spot) = self.spots.get(at) else {
            return Line::default();
        };
        let pointer = if pointed {
            Span::styled(
                format!("{} ", theme.glyphs.prompt),
                theme.style(Role::Question),
            )
        } else {
            Span::raw("  ")
        };
        let (state, style) = match spot.marked {
            Some(true) => (
                format!("{} marked right", theme.glyphs.ready),
                theme.style(Role::Success),
            ),
            Some(false) => (
                format!("{} marked wrong", theme.glyphs.not_ready),
                theme.style(Role::Danger),
            ),
            None => ("y right · n wrong".to_owned(), theme.style(Role::Dim)),
        };
        let name = match &spot.record {
            Some(record) => sanitize_line(&record.opened.title).trim().to_owned(),
            None => format!(
                "{} · {}",
                sanitize_line(&spot.run_title).trim(),
                spot.id.as_str()
            ),
        };
        let room = width.saturating_sub(INDENT + str_width(&state) + 2);
        let name = cut(&name, room);
        let pad = width.saturating_sub(INDENT + str_width(&name) + str_width(&state));
        Line::from(vec![
            pointer,
            Span::styled(name, theme.style(Role::Text)),
            Span::raw(" ".repeat(pad.max(1))),
            Span::styled(state, style),
        ])
    }
}

impl View for ApprovalsView {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        self.sync_spots(ctx.state);
        let summary = match self.data.spot_checks_waiting {
            0 => String::new(),
            1 => "1 spot check waiting".to_owned(),
            n => format!("{n} spot checks waiting"),
        };
        let header = Header {
            title: Line::from(Span::styled("Approvals", theme.style(Role::Label))),
            description: "Who decides each approval, and how auto matched you".to_owned(),
            summary,
        };
        let panes = panes::split(area, 0);
        panes::draw_header(buf, panes.header, &header, theme);
        panes::draw_box(buf, &panes, ["", ""], Side::Right, theme);

        if let Some(editor) = &self.editor {
            draw_editor(editor, panes.right, buf, ctx);
            let hints = Footer::hints(&[("⏎ choose", 0), ("esc back to the list", 0)]);
            panes::draw_footer(buf, panes.footer, hints, "", theme);
            return;
        }

        let width = usize::from(panes.right.width);
        let height = usize::from(panes.right.height);
        let items = wrap_warnings(self.items(&ctx.state.names), width, theme);
        let settings: Vec<Cells> = items
            .iter()
            .filter_map(|item| match item {
                Item::Setting { index, label } => {
                    Some(self.cells(label, &self.rows[*index], &ctx.state.names))
                }
                _ => None,
            })
            .collect();
        let widths = Widths::fit_cells(&settings, width, false);
        let selectable: Vec<usize> = (0..items.len())
            .filter(|&at| items[at].selectable())
            .collect();
        self.selected = self.selected.min(selectable.len().saturating_sub(1));
        let pointed = selectable.get(self.selected).copied();
        let at = pointed.unwrap_or(0);
        // A spot check's call comes into view with it.
        let last = match items.get(at + 1) {
            Some(Item::Detail(_)) => at + 1,
            _ => at,
        };
        if at < self.offset {
            self.offset = at.saturating_sub(1);
        } else if height > 0 && last >= self.offset + height {
            self.offset = (last + 1 - height).min(at);
        }
        let lines: Vec<Line<'static>> = items
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(height)
            .map(|(at, item)| {
                let here = Some(at) == pointed;
                match item {
                    Item::Heading { name, note } => heading_line(name, note, width, theme),
                    Item::Info(text) => dim_line(text, INDENT, width, theme),
                    Item::Detail(call) => dim_line(&sanitize_line(call), INDENT + 2, width, theme),
                    Item::Warning(text) => dim_line(text, INDENT, width, theme),
                    Item::Wrapped(line) => line.clone(),
                    Item::Setting { index, label } => cells_line(
                        &self.cells(label, &self.rows[*index], &ctx.state.names),
                        &widths,
                        here,
                        theme,
                    ),
                    Item::Spot(spot) => self.spot_line(*spot, here, width, theme),
                    Item::Blank => Line::default(),
                }
            })
            .collect();
        panes::put_lines(buf, panes.right, &lines);

        let hints: &[(&str, u8)] = match pointed.map(|at| &items[at]) {
            Some(Item::Spot(spot)) if self.spots[*spot].marked.is_none() => &[
                ("↑↓  select", 0),
                ("y right", 3),
                ("n wrong", 2),
                ("d evidence", 1),
                (panes::ESC_BACK, 0),
            ],
            Some(Item::Spot(_)) => &[("↑↓  select", 0), ("d evidence", 1), (panes::ESC_BACK, 0)],
            _ => &[("↑↓  select", 0), ("⏎ edit", 1), (panes::ESC_BACK, 0)],
        };
        panes::draw_footer(buf, panes.footer, Footer::hints(hints), "", theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        if let Some(outcome) = edit_key(&mut self.editor, key) {
            return outcome;
        }
        self.sync_spots(ctx.state);
        let items = self.items(&ctx.state.names);
        let selectable: Vec<usize> = (0..items.len())
            .filter(|&at| items[at].selectable())
            .collect();
        let pointed = selectable.get(self.selected).map(|&at| &items[at]);
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        match key.code {
            KeyCode::Esc | KeyCode::Left => ViewOutcome::Close,
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                ViewOutcome::Consumed
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(selectable.len().saturating_sub(1));
                ViewOutcome::Consumed
            }
            KeyCode::Home => {
                self.selected = 0;
                ViewOutcome::Consumed
            }
            KeyCode::End => {
                self.selected = selectable.len().saturating_sub(1);
                ViewOutcome::Consumed
            }
            KeyCode::Enter | KeyCode::Right => {
                if let Some(Item::Setting { index, label }) = pointed {
                    let row = &self.rows[*index];
                    let mut editor = flow::edit_picker(row.clone());
                    editor.title = editor_title(&row.key, label);
                    editor.vim = list::vim(ctx.state);
                    self.editor = Some(editor);
                }
                ViewOutcome::Consumed
            }
            KeyCode::Char(answer @ ('y' | 'n')) => match pointed {
                Some(Item::Spot(spot)) => self.mark(*spot, answer == 'y'),
                _ => ViewOutcome::Consumed,
            },
            KeyCode::Char('d') => match pointed {
                Some(Item::Spot(spot)) => self.evidence(*spot, ctx),
                _ => ViewOutcome::Ignored,
            },
            _ => ViewOutcome::Ignored,
        }
    }

    /// Only an open editor has a line to paste into.
    fn handle_paste(&mut self, text: &str, _ctx: &ViewCtx<'_>) -> ViewOutcome {
        edit_paste(&mut self.editor, text).unwrap_or(ViewOutcome::Ignored)
    }

    fn accept(&mut self, data: &ViewData) -> bool {
        match data {
            // The model picker open in the editor asked for these.
            ViewData::Models {
                choices,
                role,
                refreshed,
                refreshing,
                update,
                sources: _,
                memory: _,
            } => {
                let role = role.filter(|role| *role != RoleName::Main);
                let Some(editor) = self.editor.as_mut() else {
                    return false;
                };
                if *update {
                    if editor.flow.model_for.is_none() {
                        return false;
                    }
                    models::refresh(editor, choices);
                    return true;
                }
                if editor.flow.waiting != Some(Asked::Models(role)) {
                    return false;
                }
                flow::fill_models(editor, role, choices);
                let note = freshness(*refreshed, *refreshing, time::OffsetDateTime::now_utc());
                flow::show_freshness(editor, note.as_deref());
            }
            ViewData::Approvals { view } => self.data = view.clone(),
            ViewData::Settings {
                rows,
                command,
                section,
                ..
            } if command.as_deref() == Some("approvals")
                || section.as_deref() == Some("approvals") =>
            {
                self.rows = rows.clone();
            }
            _ => return false,
        }
        true
    }
}

/// Each warning as the lines it takes at `width`, in the warning color and starting `INDENT`
/// cells in, so none is cut short.
fn wrap_warnings(items: Vec<Item>, width: usize, theme: &Theme) -> Vec<Item> {
    let indent = vec![Span::raw(" ".repeat(INDENT))];
    let mut wrapped = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Item::Warning(text) => {
                let text = sanitize_line(&text).into_owned();
                let lines = wrap_text(
                    &text,
                    theme.style(Role::Warn),
                    width.max(INDENT + 1),
                    &indent,
                    &indent,
                );
                wrapped.extend(lines.into_iter().map(Item::Wrapped));
            }
            other => wrapped.push(other),
        }
    }
    wrapped
}

/// Dim text starting `indent` cells in.
fn dim_line(text: &str, indent: usize, width: usize, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(
            cut(text, width.saturating_sub(indent)),
            theme.style(Role::Dim),
        ),
    ])
}

/// A row in words. Its name says what it decides, so the description shows only where the row
/// has room to spare.
fn cells(label: &str, row: &SettingRow, names: &DisplayNames) -> Cells {
    let label = sanitize_line(label).into_owned();
    let description = sanitize_line(&row.description).trim().to_owned();
    Cells {
        description: if description.eq_ignore_ascii_case(&label) {
            String::new()
        } else {
            description
        },
        label,
        value: sanitize_line(&value_words(row, names)).into_owned(),
        unset: row.value.trim().is_empty(),
        layer: layer_words(&row.layer),
        safety: row.safety,
    }
}

/// `Merge · Who decides in auto`: the editor names the approval, since the name alone is
/// the same for every one.
fn editor_title(key: &str, name: &str) -> String {
    match split_key(key) {
        Some((group, _)) => format!("{} · {name}", words_of(group)),
        None => name.to_owned(),
    }
}

/// What a key under `approvals.` decides, below its approval's heading: `Who decides in
/// auto` for `decide_with`. A key this list doesn't know reads as its own words.
fn setting_name(leaf: &str) -> String {
    let name = match leaf {
        "mode" => "How it's decided",
        "decide_with" => "Who decides in auto",
        "otherwise" => "When auto doesn't approve",
        "max_score_spread" => "Judge's widest score gap",
        "min_winner_margin" => "Winning plan's margin",
        "min_test_coverage_of_changed_lines" => "Changed lines tests cover",
        "max_changed_lines" => "Most lines in the diff",
        "allow_debt" => "Merge with unreviewed debt",
        "max_extra_tries" => "Extra tries auto grants",
        "use_reserve" => "Auto may use the reserve",
        "change_classes" => "Changes auto may promote",
        "min_days_between" => "Days between promotions",
        "decides" => "The judge",
        "model_rules" => "Rules file",
        "different_provider" => "From a provider not yet used",
        "second_model" => "Second judge",
        "spot_check_sample" => "Share of auto calls to check",
        "pause_auto_when_spot_checks_waiting" => "Spot checks that pause auto",
        "goal_changes" => "Changes to an approved goal",
        "answers" => "Who answers questions",
        "when_declined" => "When you press Esc on a question",
        "ask_limit" => "Times an agent that asks is told to decide",
        "autopilot" => "For every approval",
        "at_hard_limit" => "Hard limit on autopilot",
        "presence" => "Check on always-on agents",
        "refuse_ancestor_env" => "Shells refused approval",
        other => return words_of(other),
    };
    name.to_owned()
}

/// A value in words: `both agree` for `rules_and_model`, a worker by its name, `yes` for
/// `true`.
fn value_words(row: &SettingRow, names: &DisplayNames) -> String {
    let words = row_value_words(&row.key, &row.value);
    let leaf = row.key.rsplit('.').next().unwrap_or_default();
    if leaf == "answers" {
        return answerer_words(&words, names);
    }
    // Left to the judge, the row says who that is.
    if leaf == "decides" && words.eq_ignore_ascii_case("judge") {
        return UNNAMED_JUDGE.to_owned();
    }
    if matches!(leaf, "decides" | "second_model")
        && let Ok(worker) = words.parse::<ModelRef>()
    {
        return names.worker(&worker);
    }
    if leaf == "change_classes" && !matches!(words.as_str(), "unset" | "none") {
        return words
            .split(", ")
            .map(change_class_words)
            .collect::<Vec<_>>()
            .join(", ");
    }
    let plain = match (leaf, words.as_str()) {
        ("decide_with", "rules") => "your rules",
        ("decide_with", "model") => "the judge",
        ("decide_with", "rules_and_model") => "both agree",
        ("decide_with", "rules_or_model") => "rules, then judge",
        ("autopilot", "true") => "on",
        ("autopilot", "false") => "off",
        ("at_hard_limit", "stop") => "stop safely",
        ("at_hard_limit", "wait") => "leave it to you",
        ("when_declined", "decide") => "the agent or main decides",
        ("when_declined", "stop") => "that part ends",
        (_, "ask") => "ask you",
        (_, "send_back") => "send it back",
        (_, "open_pull_request") => "open a pull request",
        (_, "allow_clarifications") => "allow clarifications",
        (_, "true") => "yes",
        (_, "false") => "no",
        _ => return words,
    };
    plain.to_owned()
}

/// Who answers: you, the main agent, or a model by its name. Unset reads as what it stands for.
fn answerer_words(words: &str, names: &DisplayNames) -> String {
    match words.trim() {
        word if word.eq_ignore_ascii_case("you") => "you".to_owned(),
        word if word.eq_ignore_ascii_case("main") => "the main agent".to_owned(),
        model => match model.parse::<ModelRef>() {
            Ok(worker) => names.worker(&worker),
            Err(_) => model.to_owned(),
        },
    }
}

/// The modes who answers covers, on the line under its row: a model answers in autopilot only,
/// and autopilot never waits for you.
fn answerer_modes(value: &str) -> &'static str {
    match value.trim().trim_matches('"').trim() {
        "" => "You in supervised, the main agent in auto and autopilot",
        word if word.eq_ignore_ascii_case("you") => {
            "Autopilot never waits for you, so the main agent answers there"
        }
        word if word.eq_ignore_ascii_case("main") => {
            "In every mode, understand.always_ask questions too"
        }
        _ => "On autopilot only; elsewhere you or main, by mode",
    }
}

/// A workflow change class as people say it: `checklist items` for `checklist_item`.
fn change_class_words(class: &str) -> String {
    let words = match class {
        "checklist_item" => "checklist items",
        "critique_rounds" => "critique rounds",
        "critic_policy" => "who critiques",
        "drafting_prompt" => "drafting prompt",
        "build_prompt" => "build prompt",
        "task_split" => "how tasks split",
        "task_limit" => "task limits",
        "crew_composition" => "who is in the crew",
        "step_graph" => "workflow steps",
        "setting" => "other settings",
        other => return other.replace('_', " "),
    };
    words.to_owned()
}

/// The approval a key belongs under and its name there: `merge` and `mode` for
/// `approvals.merge.mode`, and `merge` and `max_changed_lines` for
/// `approvals.rules.merge.max_changed_lines`, since an approval's rules show under it. `None`
/// for a key right under `approvals.`, but for who settles what your rules can't, which
/// belongs with the judge's settings.
fn split_key(key: &str) -> Option<(&str, &str)> {
    if key == DECIDES {
        return Some(("judge", "decides"));
    }
    let rest = key.strip_prefix("approvals.")?;
    rest.strip_prefix("rules.").unwrap_or(rest).split_once('.')
}

fn kind_name(point: &ApprovalRow) -> String {
    serde_json::to_value(point.kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// `Workflow promotion` for `workflow_promotion`.
fn words_of(name: &str) -> String {
    capitalized(&name.replace('_', " "))
}

fn decider_words(decide_with: DecideWith) -> &'static str {
    match decide_with {
        DecideWith::Rules => "your rules decide",
        DecideWith::Model => "the judge decides",
        DecideWith::RulesAndModel => "rules and judge both agree",
        DecideWith::RulesOrModel => "rules, then the judge",
    }
}

/// How auto's calls matched yours, from the auto preview record.
fn record_words(point: &ApprovalRow) -> String {
    let matches = match point.matches_in_a_row {
        1 => "1 match in a row".to_owned(),
        n => format!("{n} matches in a row"),
    };
    let misses = match point.misses {
        1 => "1 miss".to_owned(),
        n => format!("{n} misses"),
    };
    let mut words = format!("{matches} · {misses}");
    if point.suggest_auto {
        words.push_str(" · ready for auto");
    }
    words
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::app::ModelChoice;
    use argonauts_core::limits::CostEstimate;
    use argonauts_core::registry::ModelAccess;
    use argonauts_core::settings::SettingScope;
    use crossterm::event::KeyCode;

    use super::*;
    use crate::state::AppState;

    fn row(key: &str, value: &str) -> SettingRow {
        SettingRow {
            key: key.to_owned(),
            value: value.to_owned(),
            layer: "built-in".to_owned(),
            workflow_from: None,
            default_scope: SettingScope::Project,
            safety: true,
            allowed: Vec::new(),
            description: String::new(),
            picked: None,
        }
    }

    fn words_row(key: &str, value: &str, allowed: &[&str]) -> SettingRow {
        SettingRow {
            allowed: allowed.iter().map(|word| format!("{word:?}")).collect(),
            ..row(key, &format!("{value:?}"))
        }
    }

    fn choice(worker: &str) -> ModelChoice {
        ModelChoice {
            worker: worker.parse().expect("a model"),
            provider: "provider".into(),
            access: ModelAccess::Subscription,
            cost: CostEstimate::Unknown,
            plan_usage: None,
            efforts: Vec::new(),
            strengths: Vec::new(),
            current: false,
            note: None,
            older: false,
            name: String::new(),
            description: None,
            recommended: false,
            new: false,
            default_effort: None,
            effort: None,
            alias: false,
            runs: None,
            listed: None,
        }
    }

    /// Draws `view` and gives it `keys`, returning what the last one did.
    fn press(view: &mut ApprovalsView, keys: &[KeyCode]) -> ViewOutcome {
        let theme = Theme::dark();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let mut outcome = ViewOutcome::Consumed;
        for key in keys {
            outcome = view.handle_key(KeyEvent::from(*key), &ctx);
        }
        outcome
    }

    fn screen(view: &mut ApprovalsView) -> String {
        let theme = Theme::dark();
        let state = AppState::default();
        let area = Rect::new(0, 0, 110, 60);
        let mut buf = Buffer::empty(area);
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        view.render(area, &mut buf, &ctx);
        crate::stability::buffer_text(&buf)
    }

    fn editor_rows(view: &ApprovalsView) -> Vec<String> {
        view.editor
            .as_ref()
            .expect("the editor is open")
            .rows
            .iter()
            .map(|row| row.label.clone())
            .collect()
    }

    /// Every row autopilot takes, the way `/approvals` lists them from the catalog.
    fn autopilot_rows() -> Vec<SettingRow> {
        vec![
            row("approvals.autopilot", "false"),
            row(ANSWERED_BY, ""),
            words_row(
                "approvals.plan.mode",
                "auto",
                &["supervised", "auto", "autopilot"],
            ),
            words_row(
                "approvals.plan.decide_with",
                "rules_and_model",
                &["rules", "model", "rules_and_model", "rules_or_model"],
            ),
            words_row(
                "approvals.plan.otherwise",
                "ask",
                &["ask", "send_back", "stop"],
            ),
            words_row(
                "approvals.plan.at_hard_limit",
                "stop",
                &["stop", "send_back", "wait"],
            ),
            words_row(
                "approvals.merge.at_hard_limit",
                "open_pull_request",
                &["stop", "send_back", "open_pull_request", "wait"],
            ),
            row("approvals.decides", "\"judge\""),
            row("approvals.judge.second_model", ""),
            row("approvals.rules.plan.max_score_spread", "1.0"),
            row("approvals.rules.plan.min_winner_margin", "1.5"),
            row("approvals.spot_check_sample", "0.1"),
        ]
    }

    /// Autopilot comes first, its switch and who answers; then who checks, led by who settles
    /// what your rules can't; then each approval with who decides, what a hard limit does, and
    /// its rules' numbers, though their keys live under `approvals.rules`.
    #[test]
    fn autopilot_comes_first_then_who_checks_then_each_approvals_rules() {
        let names = DisplayNames::default();
        let view = ApprovalsView::with_rows(autopilot_rows());
        let items = view.items(&names);
        let lines: Vec<String> = items
            .iter()
            .filter_map(|item| match item {
                Item::Heading { name, .. } => Some(format!("# {name}")),
                Item::Setting { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            lines[..15],
            [
                "# Autopilot",
                "For every approval",
                "Who answers questions",
                "# Who checks",
                "The judge",
                "Second judge",
                "# Plan",
                "How it's decided",
                "Who decides in auto",
                "When auto doesn't approve",
                "Hard limit on autopilot",
                "Judge's widest score gap",
                "Winning plan's margin",
                "# Merge",
                "Hard limit on autopilot",
            ]
        );
        assert!(matches!(
            &items[0],
            Item::Heading { note, .. } if note == "off: each approval keeps its own mode"
        ));

        let mut view = view;
        let shown = screen(&mut view);
        for (label, value) in [
            ("For every approval", "off"),
            ("Who answers questions", "you or main, by mode"),
            ("The judge", "cheapest, other provider"),
            ("Second judge", "none, one judge"),
            ("Who decides in auto", "both agree"),
            ("Judge's widest score gap", "1.0"),
            ("Winning plan's margin", "1.5"),
        ] {
            let line = shown
                .lines()
                .find(|line| line.contains(label))
                .unwrap_or_else(|| panic!("{label} isn't shown:\n{shown}"));
            assert!(line.contains(value), "{label}: {line}");
        }
        let limits: Vec<&str> = shown
            .lines()
            .filter(|line| line.contains("Hard limit on autopilot"))
            .collect();
        assert_eq!(limits.len(), 2, "{shown}");
        assert!(limits[0].contains("stop safely"), "{limits:?}");
        assert!(limits[1].contains("open a pull request"), "{limits:?}");
    }

    /// A judge your crew names is who the judge row and "Who checks" name.
    #[test]
    fn the_judge_row_names_the_crew_s_judge() {
        let names = DisplayNames::default();
        let judge: ModelRef = "codex/gpt-5.6".parse().unwrap();
        let mut view = ApprovalsView::with_rows(autopilot_rows());
        view.data.judge = Some(judge.clone());
        let shown = screen(&mut view);
        let line = shown
            .lines()
            .find(|line| line.contains("The judge"))
            .unwrap_or_else(|| panic!("no judge row:\n{shown}"));
        assert!(line.contains(&names.worker(&judge)), "{line}");
        assert!(!shown.contains(UNNAMED_JUDGE), "{shown}");
    }

    /// What Esc on a question does, and how often an agent that asks is told to decide, sit
    /// with who answers: together they are what decides when you don't.
    #[test]
    fn what_decides_without_you_sits_under_the_switch() {
        let mut view = ApprovalsView::with_rows(vec![
            row("approvals.spot_check_sample", "0.1"),
            row("approvals.ask_limit", "2"),
            words_row("approvals.when_declined", "stop", &["decide", "stop"]),
            row(ANSWERED_BY, "\"main\""),
            row("approvals.autopilot", "true"),
        ]);
        let names = DisplayNames::default();
        let labels: Vec<String> = view
            .items(&names)
            .iter()
            .filter_map(|item| match item {
                Item::Heading { name, .. } => Some(format!("# {name}")),
                Item::Setting { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            labels[..5],
            [
                "# Autopilot",
                "For every approval",
                "Who answers questions",
                "When you press Esc on a question",
                "Times an agent that asks is told to decide",
            ]
        );
        let shown = screen(&mut view);
        let line = |label: &str| {
            shown
                .lines()
                .find(|line| line.contains(label))
                .unwrap_or_else(|| panic!("{label} isn't shown:\n{shown}"))
                .to_owned()
        };
        assert!(line("When you press Esc").contains("that part ends"));
        assert!(line("Times an agent that asks").contains('2'));
    }

    #[test]
    fn autopilot_values_read_as_words() {
        let names = DisplayNames::default();
        let on = row("approvals.autopilot", "true");
        assert_eq!(value_words(&on, &names), "on");
        let wait = row("approvals.understand.at_hard_limit", "\"wait\"");
        assert_eq!(value_words(&wait, &names), "leave it to you");
        let picked = row(ANSWERED_BY, "\"codex/gpt-5.6\"");
        let worker: ModelRef = "codex/gpt-5.6".parse().expect("a model");
        assert_eq!(value_words(&picked, &names), names.worker(&worker));

        let mut view = ApprovalsView::with_rows(vec![on, row(ANSWERED_BY, "")]);
        assert!(matches!(
            &view.items(&names)[0],
            Item::Heading { note, .. } if note == "on: nothing waits for you"
        ));
        assert!(screen(&mut view).contains("on: nothing waits for you"));
    }

    /// The one switch edits as yes or no, and a hard limit's row offers its approval's words.
    #[test]
    fn the_switch_and_a_hard_limit_edit_from_their_words() {
        let mut rows = autopilot_rows();
        rows[0].allowed = vec!["true".to_owned(), "false".to_owned()];
        let mut view = ApprovalsView::with_rows(rows);
        press(&mut view, &[KeyCode::Enter]);
        assert_eq!(editor_rows(&view), ["true", "false"]);
        let outcome = press(&mut view, &[KeyCode::Up, KeyCode::Enter, KeyCode::Enter]);
        assert!(
            matches!(
                &outcome,
                ViewOutcome::Send(UiCommand::SetSetting { key, value, scope })
                    if key == "approvals.autopilot"
                        && value == "true"
                        && *scope == SettingScope::Project
            ),
            "{outcome:?}"
        );

        // The plan's hard limit is the seventh row you can act on.
        let mut view = ApprovalsView::with_rows(autopilot_rows());
        press(&mut view, &[KeyCode::Down; 7]);
        press(&mut view, &[KeyCode::Enter]);
        assert_eq!(editor_rows(&view), ["stop", "send_back", "wait"]);
        let notes: Vec<String> = view
            .editor
            .as_ref()
            .expect("the editor")
            .rows
            .iter()
            .map(|row| row.description.clone())
            .collect();
        assert_eq!(notes[2], "Leave the approval for you; the run waits");
    }

    /// Who answers offers you, the main agent, or any model from the model picker, and saves the
    /// pick to its own key.
    #[test]
    fn who_answers_picks_you_the_main_agent_or_any_model() {
        let mut view = ApprovalsView::with_rows(autopilot_rows());
        press(&mut view, &[KeyCode::Down, KeyCode::Enter]);
        assert_eq!(
            editor_rows(&view),
            ["You", "The main agent", "Pick a model…"]
        );
        let asked = press(&mut view, &[KeyCode::Down, KeyCode::Down, KeyCode::Enter]);
        assert!(
            matches!(
                asked,
                ViewOutcome::Request(ViewRequest::Models {
                    role: Some(RoleName::TieBreaker)
                })
            ),
            "{asked:?}"
        );
        // A list for another role isn't this picker's.
        let other = ViewData::Models {
            choices: vec![choice("codex/gpt-x")],
            role: Some(RoleName::Reviewer),
            refreshed: None,
            refreshing: false,
            update: false,
            sources: Vec::new(),
            memory: None,
        };
        assert!(!view.accept(&other));
        let listed = ViewData::Models {
            choices: vec![choice("opencode/glm-5"), choice("codex/gpt-x")],
            role: Some(RoleName::TieBreaker),
            refreshed: None,
            refreshing: false,
            update: false,
            sources: Vec::new(),
            memory: None,
        };
        assert!(view.accept(&listed));
        assert_eq!(editor_rows(&view).len(), 2, "one row per CLI");
        // Esc goes back to who answers' own rows, and in again.
        press(&mut view, &[KeyCode::Esc]);
        assert_eq!(
            editor_rows(&view),
            ["You", "The main agent", "Pick a model…"]
        );
        press(&mut view, &[KeyCode::Enter]);
        assert!(view.accept(&listed));

        let saved = press(&mut view, &[KeyCode::Enter, KeyCode::Enter, KeyCode::Enter]);
        assert!(
            matches!(
                &saved,
                ViewOutcome::Send(UiCommand::SetSetting { key, value, .. })
                    if key == ANSWERED_BY && value == "\"opencode/glm-5\""
            ),
            "{saved:?}"
        );
    }

    /// The second judge comes from the same model picker, `/model judge`'s list, and
    /// the model it holds is the one marked.
    #[test]
    fn the_second_judge_comes_from_the_model_picker() {
        let mut rows = autopilot_rows();
        rows[8] = SettingRow {
            layer: "project".to_owned(),
            ..row("approvals.judge.second_model", "\"codex/gpt-x\"")
        };
        let mut view = ApprovalsView::with_rows(rows);
        press(&mut view, &[KeyCode::Down; 3]);
        press(&mut view, &[KeyCode::Enter]);
        assert_eq!(editor_rows(&view), ["Pick a model…", "Reset"]);
        let asked = press(&mut view, &[KeyCode::Enter]);
        assert!(matches!(
            asked,
            ViewOutcome::Request(ViewRequest::Models {
                role: Some(RoleName::Judge)
            })
        ));
        let listed = ViewData::Models {
            choices: vec![choice("opencode/glm-5"), choice("codex/gpt-x")],
            role: Some(RoleName::Judge),
            refreshed: None,
            refreshing: false,
            update: false,
            sources: Vec::new(),
            memory: None,
        };
        assert!(view.accept(&listed));
        let editor = view.editor.as_ref().expect("the editor");
        let marked: Vec<String> = editor
            .flow
            .models
            .iter()
            .filter(|choice| choice.current)
            .map(|choice| choice.worker.to_string())
            .collect();
        assert_eq!(marked, ["codex/gpt-x"]);
        // The pointer starts on the CLI holding it.
        let saved = press(
            &mut view,
            &[KeyCode::Up, KeyCode::Enter, KeyCode::Enter, KeyCode::Enter],
        );
        assert!(
            matches!(
                &saved,
                ViewOutcome::Send(UiCommand::SetSetting { key, value, .. })
                    if key == "approvals.judge.second_model" && value == "\"opencode/glm-5\""
            ),
            "{saved:?}"
        );
    }

    #[test]
    fn change_classes_read_as_words() {
        let names = DisplayNames::default();
        let classes = row(
            "approvals.rules.workflow_promotion.change_classes",
            r#"["checklist_item", "critique_rounds", "task_limit", "new_class"]"#,
        );
        assert_eq!(
            value_words(&classes, &names),
            "checklist items, critique rounds, task limits, new class"
        );
        let none = row("approvals.rules.workflow_promotion.change_classes", "[]");
        assert_eq!(value_words(&none, &names), "none");
    }

    #[test]
    fn who_answers_the_crews_questions_comes_under_autopilot_in_words() {
        let names = DisplayNames::default();
        let unset = row(ANSWERED_BY, "");
        assert_eq!(value_words(&unset, &names), "you or main, by mode");
        assert_eq!(
            value_words(&row(ANSWERED_BY, "\"main\""), &names),
            "the main agent"
        );
        assert_eq!(value_words(&row(ANSWERED_BY, "\"you\""), &names), "you");

        let view = ApprovalsView::with_rows(vec![
            row("approvals.goal_changes", "\"ask\""),
            row("approvals.merge.mode", "\"auto\""),
            unset,
        ]);
        let items = view.items(&names);
        assert!(matches!(&items[0], Item::Heading { name, .. } if name == "Autopilot"));
        assert!(matches!(
            &items[1],
            Item::Setting { index: 2, label } if label == "Who answers questions"
        ));
        // It isn't listed again with the other settings.
        let listed = items
            .iter()
            .filter(|item| matches!(item, Item::Setting { index: 2, .. }))
            .count();
        assert_eq!(listed, 1);
    }

    /// The line under who answers says which modes it covers: a picked model answers in full
    /// auto only, and you never do there.
    #[test]
    fn who_answers_says_which_modes_it_covers() {
        let names = DisplayNames::default();
        let under = |value: &str| {
            let view = ApprovalsView::with_rows(vec![row(ANSWERED_BY, value)]);
            match &view.items(&names)[2] {
                Item::Detail(text) => text.clone(),
                _ => panic!("a line under the row"),
            }
        };
        assert_eq!(
            under("\"codex/gpt-5.6\""),
            "On autopilot only; elsewhere you or main, by mode"
        );
        assert_eq!(
            under("\"you\""),
            "Autopilot never waits for you, so the main agent answers there"
        );
        assert_eq!(
            under("\"main\""),
            "In every mode, understand.always_ask questions too"
        );
        assert_eq!(
            under(""),
            "You in supervised, the main agent in auto and autopilot"
        );
    }

    /// With the switch on, an approval left to auto reads as autopilot, and the heading names
    /// what still waits for you: an approval you keep, and a hard limit that waits. What the
    /// settings check found shows under the heading, whole.
    #[test]
    fn the_switch_marks_what_it_overrides_and_names_what_still_waits() {
        use argonauts_core::events::approval::ApprovalKind;
        use argonauts_core::settings::approvals::ApprovalMode;

        let names = DisplayNames::default();
        let mut rows = autopilot_rows();
        rows[0] = row("approvals.autopilot", "true");
        rows.push(words_row(
            "approvals.understand.at_hard_limit",
            "wait",
            &["stop", "send_back", "wait"],
        ));
        let point = |kind, mode| ApprovalRow {
            kind,
            mode,
            decide_with: DecideWith::Rules,
            matches_in_a_row: 0,
            misses: 0,
            suggest_auto: false,
        };
        let data = ApprovalsData {
            points: vec![
                point(ApprovalKind::Plan, ApprovalMode::Autopilot),
                point(ApprovalKind::Merge, ApprovalMode::Supervised),
            ],
            warnings: vec![
                "approvals.autopilot is on, but approvals.merge.mode = \"supervised\" keeps that \
                 approval for you, so a run still waits there"
                    .to_owned(),
            ],
            ..ApprovalsData::default()
        };
        let mut view = ApprovalsView {
            rows,
            ..ApprovalsView::new(data)
        };
        assert!(matches!(
            &view.items(&names)[0],
            Item::Heading { note, .. } if note == "on: only Merge and Understand's hard limit wait for you"
        ));
        let mode = view
            .rows
            .iter()
            .find(|row| row.key == "approvals.plan.mode")
            .expect("the plan's mode")
            .clone();
        assert_eq!(
            view.cells("How it's decided", &mode, &names).value,
            "autopilot by the switch"
        );
        // Wrapped, never cut.
        let warning = view.data.warnings[0].clone();
        let wrapped: Vec<String> = wrap_warnings(view.items(&names), 60, &Theme::dark())
            .into_iter()
            .filter_map(|item| match item {
                Item::Wrapped(line) => Some(
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>(),
                ),
                _ => None,
            })
            .collect();
        assert!(wrapped.len() > 1, "{wrapped:?}");
        let words: Vec<&str> = wrapped
            .iter()
            .flat_map(|line| line.split_whitespace())
            .collect();
        assert_eq!(words.join(" "), warning);
        assert!(screen(&mut view).contains("approvals.autopilot is on"));
    }
}
