//! `/approvals`: each checkpoint's mode, who decides, its rules, and what happens otherwise,
//! beside its shadow record; the approver model and its rules file; and the spot checks you
//! mark right or wrong. Every `approvals.` key is a safety key, so an edit goes through the
//! scope question and then the controller's confirm, and no row carries the `!` that marks
//! one in `/config`. Rows go by name, as `/permissions` does.

use argonauts_core::RunId;
use argonauts_core::app::{SettingRow, UiCommand, ViewData, ViewRequest};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::{CheckpointId, WorkerRef};
use argonauts_core::policy::{ApprovalRow, ApprovalsView as ApprovalsData};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::flow;
use super::panel::{draw_editor, edit_key, edit_paste};
use super::table::{Cells, Widths, cells_line, heading_line};
use super::words::{capitalized, layer_words, row_value_words};
use crate::bottom::Picker;
use crate::cards::{spot_check_evidence, spot_check_line};
use crate::state::{AppState, DecisionRecord};
use crate::text::{sanitize_line, str_width};
use crate::theme::{Role, Theme};
use crate::views::layout::cut;
use crate::views::list;
use crate::views::panes::{self, Footer, Header, Side};
use crate::views::{View, ViewCtx, ViewOutcome};

/// Where rows start, after the pointer.
const INDENT: usize = 2;

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
    id: CheckpointId,
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
    Setting {
        index: usize,
        label: String,
    },
    /// An index into `spots`.
    Spot(usize),
    /// A spot check's call, on the line under it.
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
    /// answered on its card leaves.
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
        let mut checkpoints: Vec<String> = self.data.points.iter().map(kind_name).collect();
        for row in &self.rows {
            if let Some((group, _)) = split_key(&row.key)
                && group != "approver"
                && !checkpoints.iter().any(|known| known == group)
            {
                checkpoints.push(group.to_owned());
            }
        }
        for name in &checkpoints {
            let point = self
                .data
                .points
                .iter()
                .find(|point| kind_name(point) == *name);
            let mut heading = words_of(name);
            if let Some(point) = point {
                heading = format!(
                    "{heading} · {} · {}",
                    mode_words(point.mode),
                    decider_words(point.decide_with)
                );
            }
            let note = point.map(record_words).unwrap_or_default();
            self.push_group(&mut items, heading, note, name);
        }

        let approver = self
            .data
            .approver
            .as_ref()
            .map(|worker| names.worker(worker))
            // Unset, the policy picks one for each piece of work it judges.
            .unwrap_or_else(|| "the cheapest model from another company".to_owned());
        self.push_group(&mut items, "Approver".to_owned(), approver, "approver");

        let other: Vec<Item> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.key.starts_with("approvals.") && split_key(&row.key).is_none())
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
            Span::styled(format!("{} ", theme.glyphs.prompt), theme.style(Role::Card))
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
                theme.style(Role::Error),
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
        let items = self.items(&ctx.state.names);
        let summary = match self.data.spot_checks_waiting {
            0 => String::new(),
            1 => "1 spot check waiting".to_owned(),
            n => format!("{n} spot checks waiting"),
        };
        let header = Header {
            title: Line::from(Span::styled("Approvals", theme.style(Role::Label))),
            description: "Who decides each checkpoint, and how auto matched you".to_owned(),
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
        let settings: Vec<Cells> = items
            .iter()
            .filter_map(|item| match item {
                Item::Setting { index, label } => {
                    Some(cells(label, &self.rows[*index], &ctx.state.names))
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
                    Item::Setting { index, label } => cells_line(
                        &cells(label, &self.rows[*index], &ctx.state.names),
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
                ("↑↓ select", 0),
                ("y right", 3),
                ("n wrong", 2),
                ("d evidence", 1),
                (panes::ESC_BACK, 0),
            ],
            Some(Item::Spot(_)) => &[("↑↓ select", 0), ("d evidence", 1), (panes::ESC_BACK, 0)],
            _ => &[("↑↓ select", 0), ("⏎ edit", 1), (panes::ESC_BACK, 0)],
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

/// `Merge · Who decides in auto`: the editor names the checkpoint, since the name alone is
/// the same for every one.
fn editor_title(key: &str, name: &str) -> String {
    match split_key(key) {
        Some((group, _)) if group != "approver" => format!("{} · {name}", words_of(group)),
        Some(_) => format!("Approver · {name}"),
        None => name.to_owned(),
    }
}

/// What a key under `approvals.` decides, below its checkpoint's heading: `Who decides in
/// auto` for `decide_with`. A key this list doesn't know reads as its own words.
fn setting_name(leaf: &str) -> String {
    let name = match leaf {
        "mode" => "How it's decided",
        "decide_with" => "Who decides in auto",
        "otherwise" => "When auto doesn't approve",
        "rules.max_score_spread" => "Judge's widest score gap",
        "rules.min_winner_margin" => "Winning plan's lead",
        "rules.min_changed_line_coverage" => "Changed lines tests cover",
        "rules.max_combined_diff_lines" => "Most lines in the diff",
        "rules.allow_debt" => "Merge with unreviewed debt",
        "rules.max_extra_attempts" => "Extra attempts auto grants",
        "rules.use_reserve" => "Auto may use the reserve",
        "rules.change_classes" => "Changes auto may promote",
        "rules.min_days_between" => "Days between promotions",
        "model" => "Model",
        "model_rules" => "Rules file",
        "different_family" => "From a family not yet used",
        "second_model" => "Second approver",
        "spot_check_sample" => "Share of auto calls to check",
        "pause_auto_when_spot_checks_waiting" => "Spot checks that pause auto",
        "goal_changes" => "Changes to an approved goal",
        "presence" => "Check on always-on agents",
        "refuse_ancestor_env" => "Shells refused approval",
        other => return words_of(other.trim_start_matches("rules.")),
    };
    name.to_owned()
}

/// A value in words: `both agree` for `rules_and_model`, a worker by its name, `yes` for
/// `true`.
fn value_words(row: &SettingRow, names: &DisplayNames) -> String {
    let words = row_value_words(&row.key, &row.value);
    let leaf = row.key.rsplit('.').next().unwrap_or_default();
    if matches!(leaf, "model" | "second_model")
        && let Ok(worker) = words.parse::<WorkerRef>()
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
        ("decide_with", "model") => "the approver",
        ("decide_with", "rules_and_model") => "both agree",
        ("decide_with", "rules_or_model") => "rules, then approver",
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

/// A recipe change class as people say it: `checklist items` for `checklist_item`.
fn change_class_words(class: &str) -> String {
    let words = match class {
        "checklist_item" => "checklist items",
        "critique_rounds" => "critique rounds",
        "critic_policy" => "who critiques",
        "drafting_prompt" => "drafting prompt",
        "build_prompt" => "build prompt",
        "card_split" => "how cards split",
        "card_budget" => "card budgets",
        "crew_composition" => "who is in the crew",
        "step_graph" => "workflow steps",
        "setting" => "other settings",
        other => return other.replace('_', " "),
    };
    words.to_owned()
}

/// `merge` and `mode` for `approvals.merge.mode`; `None` for a key right under `approvals.`.
fn split_key(key: &str) -> Option<(&str, &str)> {
    key.strip_prefix("approvals.")?.split_once('.')
}

fn kind_name(point: &ApprovalRow) -> String {
    serde_json::to_value(point.kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// `Recipe promotion` for `recipe_promotion`.
fn words_of(name: &str) -> String {
    capitalized(&name.replace('_', " "))
}

fn mode_words(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Supervised => "supervised",
        ApprovalMode::Shadow => "shadow",
        ApprovalMode::Auto => "auto",
    }
}

fn decider_words(decide_with: DecideWith) -> &'static str {
    match decide_with {
        DecideWith::Rules => "your rules decide",
        DecideWith::Model => "the approver decides",
        DecideWith::RulesAndModel => "rules and approver both agree",
        DecideWith::RulesOrModel => "rules, then the approver",
    }
}

/// How auto's calls matched yours, from the shadow record.
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
    use argonauts_core::settings::SettingScope;

    use super::*;

    fn row(key: &str, value: &str) -> SettingRow {
        SettingRow {
            key: key.to_owned(),
            value: value.to_owned(),
            layer: "built-in".to_owned(),
            preset_from: None,
            default_scope: SettingScope::Project,
            safety: true,
            allowed: Vec::new(),
            description: String::new(),
        }
    }

    #[test]
    fn change_classes_read_as_words() {
        let names = DisplayNames::default();
        let classes = row(
            "approvals.recipe_promotion.rules.change_classes",
            r#"["checklist_item", "critique_rounds", "card_budget", "new_class"]"#,
        );
        assert_eq!(
            value_words(&classes, &names),
            "checklist items, critique rounds, card budgets, new class"
        );
        let none = row("approvals.recipe_promotion.rules.change_classes", "[]");
        assert_eq!(value_words(&none, &names), "none");
    }
}
