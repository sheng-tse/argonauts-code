//! `/bottom-line`'s picker: which CLIs show their plan meters on the line, in what order and
//! with which windows, and whether a CLI that holds up a running task joins them. Its last
//! row is the line itself as the picks would draw it at this width, so each key shows what it
//! does before anything is saved. Enter saves the picks as an ordered list, so the line holds
//! still after you choose.

use std::collections::BTreeMap;
use std::time::Instant;

use argonauts_core::app::{LineFacts, UiCommand};
use argonauts_core::input::{LineChange, SlashCommand};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::bottom_line::{LineCli, PlanUsageLine, PlanUsageWindows};
use argonauts_core::settings::ui::UiSettings;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use time::{OffsetDateTime, UtcOffset};

use super::{Clock, CrewCli, Source, bare, build_from, crew, has_plan, key};
use crate::base::draw::render::RenderCtx;
use crate::base::format;
use crate::base::kit::logo::{ModelLabel, NoPicture};
use crate::base::text::{sanitize_line, spans_width, truncate_line};
use crate::base::theme::{Role, Theme};
use crate::bottom::line::chip_meters;
use crate::bottom::{BottomRow, ModeState, PartBody};
use crate::frame::state::AppState;
use crate::frame::view::list;
use crate::frame::view::{View, ViewCtx, ViewOutcome};

/// The windows `←` and `→` step through, in order.
const WINDOWS: [PlanUsageWindows; 4] = [
    PlanUsageWindows::Both,
    PlanUsageWindows::Fullest,
    PlanUsageWindows::FiveHours,
    PlanUsageWindows::SevenDays,
];

/// The longest of each column's words, so the columns hold still while you change a row.
const WINDOWS_WIDTH: usize = "fuller window".len() + 2;
const WHY_WIDTH: usize = "when it holds up a task".len() + 2;
/// The reasons' column narrowed for a narrow terminal: room for "in use" and "no plan".
const SHORT_WHY_WIDTH: usize = "no plan".len() + 2;

#[derive(Debug, Clone)]
pub struct LinePicker {
    /// The CLI in use first, then the ones the line shows in their order, then the rest.
    rows: Vec<Row>,
    holding_up: bool,
    /// A row, or one past the last for the holding-up row.
    pointed: usize,
    /// Where Enter saves, when the command named a place.
    scope: Option<SettingScope>,
    /// The mode and its hints as the line had them when the picker opened, which the preview
    /// draws beside the chips.
    mode: BottomRow,
    ui: UiSettings,
    slots: BTreeMap<String, PartBody>,
    /// The rows and the holding-up pick as the picker opened, so Enter with nothing changed
    /// writes nothing and the line keeps following your use.
    opened: (Vec<Row>, bool),
}

#[derive(Debug, Clone, PartialEq)]
struct Row {
    id: String,
    shown: bool,
    /// Its own windows, where they aren't what it shows by default.
    windows: Option<PlanUsageWindows>,
}

impl LinePicker {
    /// The picker on the line as the state has it now. `mode` is the mode and its hints it
    /// previews the chips beside, and `scope` where Enter saves when the command named one.
    pub fn new(
        state: &AppState,
        mode: BottomRow,
        ui: &UiSettings,
        slots: &BTreeMap<String, PartBody>,
        scope: Option<SettingScope>,
    ) -> Self {
        let facts = facts(state);
        let set = &facts.settings.plan_usage;
        let clock = clock(state, OffsetDateTime::now_utc());
        let source = source(state, &facts, slots, clock);
        let crew = crew_of(state, set);
        let mut rows: Vec<Row> = source
            .picked(&crew)
            .into_iter()
            .map(|(cli, windows)| Row {
                id: key(cli).to_owned(),
                shown: true,
                windows,
            })
            .collect();
        for cli in &crew {
            if !rows.iter().any(|row| row.id == key(cli)) {
                rows.push(Row {
                    id: key(cli).to_owned(),
                    shown: false,
                    windows: None,
                });
            }
        }
        Self {
            opened: (rows.clone(), set.holding_up),
            rows,
            holding_up: set.holding_up,
            pointed: 0,
            scope,
            mode: BottomRow {
                notice: None,
                ..mode
            },
            ui: ui.clone(),
            slots: slots.clone(),
        }
    }

    /// The picks as `/bottom-line` saves them: the CLIs shown, in order, each with its own
    /// windows.
    fn picked(&self) -> Vec<LineCli> {
        self.rows
            .iter()
            .filter(|row| row.shown)
            .map(|row| LineCli {
                cli: row.id.clone(),
                windows: row.windows,
            })
            .collect()
    }

    fn save(&self, scope: Option<SettingScope>) -> ViewOutcome {
        if (&self.rows, self.holding_up) == (&self.opened.0, self.opened.1) {
            return ViewOutcome::Close;
        }
        let change = LineChange::Set {
            clis: self.picked(),
            holding_up: self.holding_up,
        };
        ViewOutcome::SendToConversation(UiCommand::Slash {
            command: SlashCommand::BottomLine { change, scope },
        })
    }

    /// The CLI the main session is on, which always shows and always comes first.
    fn is_main(&self, at: usize) -> bool {
        at == 0
    }

    /// Moves the pointed CLI one place up or down, never above the one in use.
    fn shift(&mut self, down: bool) -> ViewOutcome {
        let at = self.pointed;
        if at >= self.rows.len() || self.is_main(at) {
            return ViewOutcome::Consumed;
        }
        let to = if down { at + 1 } else { at - 1 };
        if self.is_main(to) || to >= self.rows.len() {
            return ViewOutcome::Consumed;
        }
        self.rows.swap(at, to);
        self.pointed = to;
        ViewOutcome::Consumed
    }

    fn toggle(&mut self) -> ViewOutcome {
        if self.pointed == self.rows.len() {
            self.holding_up = !self.holding_up;
            return ViewOutcome::Consumed;
        }
        if self.is_main(self.pointed) {
            return ViewOutcome::Notice(
                "The one in use always shows, so you always see what your turns cost".to_owned(),
            );
        }
        let row = &mut self.rows[self.pointed];
        row.shown = !row.shown;
        ViewOutcome::Consumed
    }

    /// Steps the pointed CLI's windows to the next or the one before.
    fn step_windows(&mut self, state: &AppState, back: bool) -> ViewOutcome {
        let Some(row) = self.rows.get(self.pointed) else {
            return ViewOutcome::Consumed;
        };
        let facts = facts(state);
        if !plan_of(state, &facts, &row.id) {
            return ViewOutcome::Notice(format!(
                "{} has no plan windows to pick",
                name(state, &row.id)
            ));
        }
        let main = self.is_main(self.pointed);
        let set = &facts.settings.plan_usage;
        let now = shown_windows(row.windows, main, set);
        let at = WINDOWS.iter().position(|w| *w == now).unwrap_or(0);
        let next = if back {
            WINDOWS[(at + WINDOWS.len() - 1) % WINDOWS.len()]
        } else {
            WINDOWS[(at + 1) % WINDOWS.len()]
        };
        let by_default = shown_windows(None, main, set);
        self.rows[self.pointed].windows = (next != by_default).then_some(next);
        ViewOutcome::Consumed
    }

    /// Every row of the picker at `width`, as it draws on a clock that reads `now`.
    fn lines(
        &self,
        state: &AppState,
        theme: &Theme,
        width: u16,
        now: OffsetDateTime,
        at: Instant,
    ) -> Vec<Line<'static>> {
        let facts = facts(state);
        let set = &facts.settings.plan_usage;
        let clock = clock(state, now);
        let source = source(state, &facts, &self.slots, clock);
        let crew = crew_of(state, set);
        let style = |role| theme.style(role);
        let wide = usize::from(width);
        let mut lines = vec![Line::styled("─".repeat(wide), style(Role::Faint))];
        lines.push(Line::raw("  The bottom line"));
        lines.extend(wrapped(
            vec![
                Span::raw(
                    "Which CLIs show their plan meters under the input, and in what order. \
                     The one ",
                ),
                Span::styled("in use", style(Role::Success)),
                Span::raw(" always comes first."),
            ],
            wide,
        ));
        lines.push(Line::default());

        let most_used = facts
            .most_used
            .iter()
            .find(|id| self.rows.first().is_none_or(|main| main.id != **id))
            .filter(|id| self.rows.iter().any(|row| row.id == **id));
        let name_width = self
            .rows
            .iter()
            .map(|row| name(state, &row.id).chars().count())
            .max()
            .unwrap_or(0)
            + 2;
        // A narrow terminal gives up the longer reasons, then the second window of the CLI in
        // use, then every reason, before any row is cut.
        let row_at = |at: usize, row: &Row, layout: Layout| -> Vec<Span<'static>> {
            let main = self.is_main(at);
            let cli = crew.iter().find(|cli| key(cli) == row.id);
            let bare_cli;
            let cli = match cli {
                Some(cli) => cli,
                None => {
                    bare_cli = bare(state, &row.id);
                    &bare_cli
                }
            };
            let planned = plan_of(state, &facts, &row.id);
            let mut spans = self.pointer_and_box(at, row.shown, theme);
            let label =
                ModelLabel::cli_named(&row.id, &name(state, &row.id), theme, NoPicture::Dot);
            spans.extend(label.logo);
            spans.push(Span::raw(pad(label.name, name_width)));
            let windows = if planned {
                windows_words(shown_windows(row.windows, main, set))
            } else {
                ""
            };
            spans.push(Span::raw(pad(windows.to_owned(), WINDOWS_WIDTH)));
            if let Some(width) = layout.why {
                let (why, role) = match () {
                    () if main => ("in use", Some(Role::Success)),
                    () if !planned => ("no plan", Some(Role::Dim)),
                    () if most_used == Some(&row.id) => ("most used, 30 days", None),
                    () if !row.shown && self.holding_up => ("when it holds up a task", None),
                    () => ("", None),
                };
                // A reason too long for a narrowed column is left out, never cut.
                let why = if why.chars().count() + 2 > width {
                    ""
                } else {
                    why
                };
                let why = pad(why.to_owned(), width);
                spans.push(match role {
                    Some(role) => Span::styled(why, style(role)),
                    None => Span::raw(why),
                });
            }
            let windows = match shown_windows(row.windows, main, set) {
                PlanUsageWindows::Both if !layout.second => Some(PlanUsageWindows::Fullest),
                _ => row.windows,
            };
            let mut chip = source.chip(cli, windows, set);
            chip.main = main;
            if !planned {
                chip.note = facts
                    .spent_this_week
                    .get(&row.id)
                    .filter(|spent| **spent > 0.0)
                    .map(|spent| format!("{} spent this week", format::usd(*spent)));
            }
            spans.extend(chip_meters(&chip, theme));
            trimmed(spans)
        };
        let fits = |layout: Layout| {
            self.rows
                .iter()
                .enumerate()
                .all(|(at, row)| spans_width(&row_at(at, row, layout)) <= wide)
        };
        let layout = Layout::NARROWER
            .into_iter()
            .find(|layout| fits(*layout))
            .unwrap_or(Layout::NARROWEST);
        for (at, row) in self.rows.iter().enumerate() {
            lines.push(truncate_line(&Line::from(row_at(at, row, layout)), wide));
        }
        lines.push(Line::default());
        let mut holding = self.pointer_and_box(self.rows.len(), self.holding_up, theme);
        holding.extend([
            Span::raw("Add a CLI that holds up a running task: out, "),
            Span::styled("down", style(Role::Danger)),
            Span::raw(" or 90% used"),
        ]);
        lines.push(truncate_line(&Line::from(holding), wide));
        lines.push(Line::default());

        lines.push(Line::raw("  The line, as it will look:"));
        let mut picked = facts.clone();
        picked.settings.plan_usage.clis = Some(self.picked());
        picked.settings.plan_usage.holding_up = self.holding_up;
        let line = build_from(state, &picked, &self.ui, &self.slots, now);
        let ctx = RenderCtx::new(width, theme, crate::base::draw::render::Clock::still(at));
        let state = ModeState {
            line: Some(&line),
            ..ModeState::default()
        };
        lines.push(self.mode.line(&ctx, state));
        lines.push(Line::default());
        lines.extend(keys_wrapped(&self.hints(&facts), style(Role::Dim), wide));
        lines
    }

    fn pointer_and_box(&self, at: usize, ticked: bool, theme: &Theme) -> Vec<Span<'static>> {
        let pointer = if self.pointed == at {
            Span::styled("❯ ", theme.style(Role::Question))
        } else {
            Span::raw("  ")
        };
        let ticked = if ticked {
            Span::styled("☒ ", theme.style(Role::Question))
        } else {
            Span::styled("☐ ", theme.style(Role::Dim))
        };
        vec![Span::raw("  "), pointer, ticked]
    }

    /// The keys, with where Enter saves: where the command said, else where your changes go.
    fn hints(&self, facts: &LineFacts) -> Vec<&'static str> {
        let place = self.scope.unwrap_or_else(|| facts.saves_to.scope());
        let enter = match place {
            SettingScope::Session => "enter keeps it for this session",
            SettingScope::Project => "enter saves to this project",
            SettingScope::Global | SettingScope::ProjectLocal => "enter saves everywhere",
        };
        let mut keys = vec![
            "space shows or hides",
            "shift+↑↓  moves",
            "←  →  windows",
            enter,
        ];
        if place != SettingScope::Session {
            keys.push("ctrl+s this session");
        }
        keys.push("esc cancels");
        keys
    }
}

impl View for LinePicker {
    fn docked(&self) -> bool {
        true
    }

    fn dock_height(&mut self, screen: Size, ctx: &ViewCtx<'_>) -> Option<u16> {
        let rows = self
            .lines(
                ctx.state,
                ctx.theme,
                screen.width,
                OffsetDateTime::now_utc(),
                ctx.now,
            )
            .len();
        Some(u16::try_from(rows).unwrap_or(u16::MAX).min(screen.height))
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let lines = self.lines(
            ctx.state,
            ctx.theme,
            area.width,
            OffsetDateTime::now_utc(),
            ctx.now,
        );
        // The line and the keys sit at the foot, as the line under the input does.
        let skip = lines.len().saturating_sub(usize::from(area.height));
        for (y, line) in lines.iter().skip(skip).enumerate() {
            let y = area.y + u16::try_from(y).unwrap_or(u16::MAX);
            buf.set_line(area.x, y, line, area.width);
        }
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('s') => self.save(Some(SettingScope::Session)),
                _ => ViewOutcome::Ignored,
            };
        }
        let shifted = key.modifiers.contains(KeyModifiers::SHIFT);
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        match key.code {
            KeyCode::Esc => ViewOutcome::Close,
            KeyCode::Enter => self.save(self.scope),
            KeyCode::Up if shifted => self.shift(false),
            KeyCode::Down if shifted => self.shift(true),
            KeyCode::Up => {
                self.pointed = self.pointed.saturating_sub(1);
                ViewOutcome::Consumed
            }
            KeyCode::Down => {
                self.pointed = (self.pointed + 1).min(self.rows.len());
                ViewOutcome::Consumed
            }
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Left => self.step_windows(ctx.state, true),
            KeyCode::Right => self.step_windows(ctx.state, false),
            _ => ViewOutcome::Ignored,
        }
    }
}

fn facts(state: &AppState) -> LineFacts {
    state
        .status
        .as_ref()
        .map(|status| status.line.as_ref().clone())
        .unwrap_or_default()
}

fn clock(state: &AppState, now: OffsetDateTime) -> Clock {
    Clock {
        now,
        utc_offset: state.terminal.utc_offset.unwrap_or(UtcOffset::UTC),
    }
}

fn source<'a>(
    state: &'a AppState,
    facts: &'a LineFacts,
    slots: &'a BTreeMap<String, PartBody>,
    clock: Clock,
) -> Source<'a> {
    Source {
        state,
        status: state.status.as_ref(),
        facts,
        slots,
        mods: None,
        clock,
    }
}

/// The crew's CLIs, one each, with any the list names that the crew leaves out.
fn crew_of(state: &AppState, set: &PlanUsageLine) -> Vec<CrewCli> {
    let mut clis: Vec<CrewCli> = Vec::new();
    for cli in crew(state, state.status.as_ref()) {
        if !clis.iter().any(|known| key(known) == key(&cli)) {
            clis.push(cli);
        }
    }
    for line_cli in set.clis.iter().flatten() {
        if !clis.iter().any(|cli| key(cli) == line_cli.cli) {
            clis.push(bare(state, &line_cli.cli));
        }
    }
    clis
}

/// Whether the CLI has plan windows to show: one was read, or it's a CLI that has a plan.
fn plan_of(state: &AppState, facts: &LineFacts, id: &str) -> bool {
    let crew = crew_of(state, &facts.settings.plan_usage);
    crew.iter()
        .find(|cli| key(cli) == id)
        .is_some_and(|cli| !cli.readings.is_empty() || has_plan(key(cli)))
}

/// The windows a CLI shows with `own` as its own: the one in use both by default, the others
/// their fuller one.
fn shown_windows(
    own: Option<PlanUsageWindows>,
    main: bool,
    set: &PlanUsageLine,
) -> PlanUsageWindows {
    match own.unwrap_or(set.windows) {
        PlanUsageWindows::Both if main || own.is_some() => PlanUsageWindows::Both,
        PlanUsageWindows::Both => PlanUsageWindows::Fullest,
        windows => windows,
    }
}

fn windows_words(windows: PlanUsageWindows) -> &'static str {
    match windows {
        PlanUsageWindows::Both => "both windows",
        PlanUsageWindows::Fullest => "fuller window",
        PlanUsageWindows::FiveHours => "5h window",
        PlanUsageWindows::SevenDays => "7d window",
    }
}

/// The CLI's own name, such as `Claude Code`.
fn name(state: &AppState, id: &str) -> String {
    sanitize_line(&state.names.cli(id)).trim().to_owned()
}

fn pad(text: String, width: usize) -> String {
    let pad = width.saturating_sub(text.chars().count());
    format!("{text}{}", " ".repeat(pad))
}

/// `spans` without the blanks at their end, which a row with nothing to show would leave.
fn trimmed(mut spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    while let Some(last) = spans.last_mut() {
        let kept = last.content.trim_end().to_owned();
        if !kept.is_empty() {
            last.content = kept.into();
            break;
        }
        spans.pop();
    }
    spans
}

/// What the picker's rows show, from the widest: on a narrow terminal the reasons longer than
/// "no plan" go first, then the second window of the CLI in use, then every reason.
#[derive(Debug, Clone, Copy)]
struct Layout {
    second: bool,
    /// The reasons' column, so wide; none at all when `None`.
    why: Option<usize>,
}

impl Layout {
    const NARROWER: [Layout; 4] = [
        Layout {
            second: true,
            why: Some(WHY_WIDTH),
        },
        Layout {
            second: true,
            why: Some(SHORT_WHY_WIDTH),
        },
        Layout {
            second: false,
            why: Some(SHORT_WHY_WIDTH),
        },
        Layout::NARROWEST,
    ];
    const NARROWEST: Layout = Layout {
        second: false,
        why: None,
    };
}

/// `keys` joined with ` · ` as rows of `width` after a two-column indent, broken only between
/// keys, so no key's words part.
fn keys_wrapped(keys: &[&str], style: Style, width: usize) -> Vec<Line<'static>> {
    let room = width.saturating_sub(2).max(1);
    let mut rows: Vec<String> = Vec::new();
    for key in keys {
        match rows.last_mut() {
            Some(row) if row.chars().count() + 3 + key.chars().count() <= room => {
                row.push_str(" · ");
                row.push_str(key);
            }
            _ => rows.push((*key).to_owned()),
        }
    }
    rows.into_iter()
        .map(|row| Line::from(vec![Span::raw("  "), Span::styled(row, style)]))
        .collect()
}

/// `spans` as rows of `width` after a two-column indent, broken between words.
fn wrapped(spans: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    let room = width.saturating_sub(2).max(1);
    let mut lines: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    for span in spans {
        let style = span.style;
        for word in span.content.split_inclusive(' ') {
            let line = lines.last_mut().expect("a line");
            let used = spans_width(line);
            if used > 0 && used + word.trim_end().chars().count() > room {
                lines.push(Vec::new());
            }
            let line = lines.last_mut().expect("a line");
            if line.is_empty() && word.trim().is_empty() {
                continue;
            }
            line.push(Span::styled(word.to_owned(), style));
        }
    }
    lines
        .into_iter()
        .map(|mut spans| {
            spans.insert(0, Span::raw("  "));
            Line::from(trimmed(spans))
        })
        .collect()
}

#[cfg(test)]
mod tests;
