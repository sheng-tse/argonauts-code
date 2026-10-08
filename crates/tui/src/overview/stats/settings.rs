//! The stats mod's page: `,` on the Stats tab, or `/mods stats`. The card that pins above the
//! input is drawn over its rows as the page stands, so a change shows at once, and the tab
//! behind it takes the period, the grouping and the measure as you pick them. `s` writes the
//! keys you changed into `mods/stats.toml`.

use argonauts_core::app::UiCommand;
use argonauts_core::settings::mods::ModOrigin;
use argonauts_core::settings::mods::view::ModChange;
use argonauts_core::settings::stats::{
    STATS_FILE, StatsColumn, StatsGroup, StatsMeasure, StatsPeriod, StatsSettings, StatsSources,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::text::{spans_width, str_width};
use crate::theme::Role;
use crate::widgets::{Drawn, Kit, spread};

/// One setting, a row of the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setting {
    Period,
    Group,
    Measure,
    Total,
    Columns,
}

/// The rows top to bottom, under their two headings.
const ROWS: [Setting; 5] = [
    Setting::Period,
    Setting::Group,
    Setting::Measure,
    Setting::Total,
    Setting::Columns,
];

/// Cells for a row's name.
const LABEL: usize = 14;

const PERIODS: [(StatsPeriod, &str); 4] = [
    (StatsPeriod::Today, "today"),
    (StatsPeriod::Week, "7 days"),
    (StatsPeriod::Month, "30 days"),
    (StatsPeriod::All, "all"),
];
const GROUPS: [(StatsGroup, &str); 3] = [
    (StatsGroup::Cli, "by CLI"),
    (StatsGroup::Model, "by model"),
    (StatsGroup::Role, "by role"),
];
const MEASURES: [StatsMeasure; 3] = [
    StatsMeasure::Tokens,
    StatsMeasure::Tasks,
    StatsMeasure::Spend,
];

impl Setting {
    fn label(self) -> &'static str {
        match self {
            Self::Period => "Period",
            Self::Group => "Grouped",
            Self::Measure => "Bars measure",
            Self::Total => "All together",
            Self::Columns => "Columns",
        }
    }

    /// Its key in the file.
    fn key(self) -> &'static str {
        match self {
            Self::Period => "period",
            Self::Group => "group",
            Self::Measure => "measure",
            Self::Total => "total",
            Self::Columns => "columns",
        }
    }

    /// The heading a row starts, if it's the first under one.
    fn heading(self) -> Option<&'static str> {
        match self {
            Self::Period => Some("What shows first"),
            Self::Columns => Some("The card above the input"),
            _ => None,
        }
    }
}

/// What a key on the page asks of the tab.
#[derive(Debug, Clone, PartialEq)]
pub enum PageKey {
    /// Drawn again.
    Moved,
    /// The period changed, so the tab wants that period's numbers.
    Period(StatsPeriod),
    /// The grouping or the measure changed, which the tab takes at once.
    Shown,
    Send(UiCommand),
    /// Back to the tab.
    Close,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsPage {
    /// As the page stands.
    pub file: StatsSettings,
    /// As the files hold it.
    saved: StatsSettings,
    sources: StatsSources,
    row: usize,
    /// The column the keys are on, in the Columns row's order.
    column: usize,
    /// A second `r` deletes the file.
    reset_armed: bool,
    /// What the last key had to say. The panel covers the line where notices show, so the
    /// page says it under its rows.
    said: Option<String>,
}

impl SettingsPage {
    pub fn new(saved: StatsSettings, sources: StatsSources) -> Self {
        Self {
            file: saved.clone(),
            saved,
            sources,
            row: 0,
            column: 0,
            reset_armed: false,
            said: None,
        }
    }

    fn say(&mut self, text: impl Into<String>) -> PageKey {
        self.said = Some(text.into());
        PageKey::Moved
    }

    pub fn changed(&self) -> bool {
        self.file != self.saved
    }

    /// The files as newer numbers read them, when nothing here is unsaved: after a save, or
    /// an edit by hand.
    pub fn refresh(&mut self, saved: &StatsSettings, sources: &StatsSources) {
        if self.changed() || (self.saved == *saved && self.sources == *sources) {
            return;
        }
        self.saved = saved.clone();
        self.file = saved.clone();
        self.sources = sources.clone();
    }

    /// A key of yours that the project's file also sets would have no effect, so the page
    /// saves where the file in use is.
    fn to_project(&self) -> bool {
        !self.sources.project.is_empty()
    }

    fn path(&self) -> String {
        if self.to_project() {
            format!(".argonauts/mods/{STATS_FILE}")
        } else {
            format!("~/.argonauts/mods/{STATS_FILE}")
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<PageKey> {
        let armed = std::mem::take(&mut self.reset_armed);
        self.said = None;
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let setting = ROWS[self.row];
        let outcome = match key.code {
            KeyCode::Up | KeyCode::BackTab => {
                self.row = self.row.saturating_sub(1);
                PageKey::Moved
            }
            KeyCode::Down | KeyCode::Tab => {
                self.row = (self.row + 1).min(ROWS.len() - 1);
                PageKey::Moved
            }
            KeyCode::Left if shift && setting == Setting::Columns => self.order(true),
            KeyCode::Right if shift && setting == Setting::Columns => self.order(false),
            KeyCode::Left => self.step(setting, false),
            KeyCode::Right => self.step(setting, true),
            KeyCode::Char(' ') | KeyCode::Enter => self.tick(setting),
            KeyCode::Char('s') => self.save(),
            KeyCode::Char('r') => {
                self.reset_armed = armed;
                self.reset()
            }
            KeyCode::Esc => PageKey::Close,
            _ => return None,
        };
        Some(outcome)
    }

    /// ←→ picks the next choice, or on the Columns row moves between them.
    fn step(&mut self, setting: Setting, ahead: bool) -> PageKey {
        let file = &mut self.file;
        match setting {
            Setting::Period => {
                file.period = next(&PERIODS.map(|(period, _)| period), file.period, ahead);
                PageKey::Period(file.period)
            }
            Setting::Group => {
                file.group = next(&GROUPS.map(|(group, _)| group), file.group, ahead);
                PageKey::Shown
            }
            Setting::Measure => {
                file.measure = next(&MEASURES, file.measure, ahead);
                PageKey::Shown
            }
            Setting::Total => {
                file.total = !file.total;
                PageKey::Shown
            }
            Setting::Columns => {
                let last = self.ticks().len().saturating_sub(1);
                self.column = if ahead {
                    (self.column + 1).min(last)
                } else {
                    self.column.saturating_sub(1)
                };
                PageKey::Moved
            }
        }
    }

    /// Space ticks or unticks the column under the keys. A ticked one goes last of the
    /// ticked, and the keys follow it there.
    fn tick(&mut self, setting: Setting) -> PageKey {
        if setting != Setting::Columns {
            return self.step(setting, true);
        }
        let Some((column, on)) = self.ticks().get(self.column).copied() else {
            return PageKey::Moved;
        };
        if on {
            self.file.columns.retain(|shown| *shown != column);
        } else {
            self.file.columns.push(column);
        }
        self.follow(column);
        PageKey::Moved
    }

    /// Shift+←→ moves a ticked column earlier or later on the card.
    fn order(&mut self, earlier: bool) -> PageKey {
        let tick = self.ticks().get(self.column).copied();
        let Some((column, true)) = tick else {
            return self.say("Tick a column first: only the ticked ones have an order");
        };
        let columns = &mut self.file.columns;
        let Some(at) = columns.iter().position(|shown| *shown == column) else {
            return PageKey::Moved;
        };
        let to = if earlier {
            at.checked_sub(1)
        } else {
            Some(at + 1).filter(|to| *to < columns.len())
        };
        if let Some(to) = to {
            columns.swap(at, to);
            self.follow(column);
        }
        PageKey::Moved
    }

    fn follow(&mut self, column: StatsColumn) {
        if let Some(at) = self.ticks().iter().position(|(tick, _)| *tick == column) {
            self.column = at;
        }
    }

    /// The ticked columns in the card's order, then the rest. Lines are offered only once a
    /// file names them: nothing records them per task yet, so the card leaves them out.
    fn ticks(&self) -> Vec<(StatsColumn, bool)> {
        let shown = &self.file.columns;
        let rest = StatsColumn::ALL
            .into_iter()
            .filter(|column| *column != StatsColumn::Lines && !shown.contains(column));
        shown
            .iter()
            .map(|column| (*column, true))
            .chain(rest.map(|column| (column, false)))
            .collect()
    }

    fn save(&mut self) -> PageKey {
        let pairs = self.file.changes_from(&self.saved);
        if pairs.is_empty() {
            return self.say("Nothing to save: the page matches the file");
        }
        // The page shows it saved at once; the file's own reading follows with the numbers.
        self.saved = self.file.clone();
        PageKey::Send(save_command(pairs, self.to_project()))
    }

    /// `r`: what's unsaved goes first. With nothing unsaved, a second `r` deletes the file,
    /// which brings the built-in settings back.
    fn reset(&mut self) -> PageKey {
        if self.changed() {
            self.file = self.saved.clone();
            return self.say("Back to what the file holds");
        }
        if self.sources.is_empty() {
            return self.say("Already the built-in settings");
        }
        if !self.reset_armed {
            self.reset_armed = true;
            return self.say(format!(
                "r again deletes {} and brings the built-in settings back",
                self.path()
            ));
        }
        self.saved = StatsSettings::default();
        self.file = self.saved.clone();
        PageKey::Send(reset_command(self.to_project()))
    }

    /// The row under the tabs: what the page is, and where it saves.
    pub fn about(&self, kit: &Kit<'_>, width: usize) -> Vec<Span<'static>> {
        let left = vec![kit.dim(" The stats mod: what /stats and its card show first")];
        let right = vec![kit.dim(format!("{} ", self.path()))];
        if spans_width(&left) + spans_width(&right) + 1 > width {
            return spread(Vec::new(), right, width);
        }
        spread(left, right, width)
    }

    /// The keys, in the order the bottom line's page lists them.
    pub fn keys(&self) -> Vec<&'static str> {
        let save = if self.to_project() {
            "s save to the project"
        } else {
            "s save to yours"
        };
        vec![
            "↑↓ setting",
            "←→ change",
            "space tick",
            "shift+←→ order",
            "r reset",
            save,
        ]
    }

    /// The headings and rows, and under them whether it's saved.
    pub fn rows(&self, kit: &Kit<'_>, width: usize) -> Drawn {
        let mut drawn = Drawn::new();
        for (at, setting) in ROWS.into_iter().enumerate() {
            if let Some(heading) = setting.heading() {
                drawn.push(vec![Span::styled(
                    format!(" {heading}"),
                    kit.bold(Role::Text),
                )]);
            }
            for line in self.row(kit, setting, at == self.row, width) {
                drawn.push(line);
            }
        }
        if self.said.is_some() || self.changed() {
            drawn.push(Vec::new());
        }
        if let Some(said) = &self.said {
            drawn.push(vec![kit.dim(format!(" {said}"))]);
        }
        if self.changed() {
            drawn.push(vec![kit.span(" Not saved yet. s saves it", Role::Warning)]);
        }
        drawn
    }

    /// `❯ Grouped       by CLI  by model  by role          built-in`, the choice in use in
    /// bold. A row of ticks too long for one line goes on under its first choice.
    fn row(
        &self,
        kit: &Kit<'_>,
        setting: Setting,
        picked: bool,
        width: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let glyphs = &kit.theme.glyphs;
        let pointer = if picked {
            Span::styled(format!(" {} ", glyphs.prompt), kit.bold(Role::Card))
        } else {
            Span::raw("   ")
        };
        let label = format!("{:<LABEL$}", setting.label());
        let label = if picked {
            Span::styled(label, kit.bold(Role::Card))
        } else {
            Span::raw(label)
        };
        let source = vec![kit.dim(format!("{} ", self.sources.of(setting.key()).word()))];
        let lead = str_width(&format!("   {:<LABEL$}", ""));
        let room = width.saturating_sub(lead + spans_width(&source) + 2);
        let choices = self.choices(kit, setting, picked);
        let mut lines = Vec::new();
        let mut line: Vec<Span<'static>> = Vec::new();
        for choice in choices {
            let wide = spans_width(&line) + 2 + spans_width(&choice);
            if !line.is_empty() && wide > room {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(Span::raw("  "));
            }
            line.extend(choice);
        }
        lines.push(line);
        lines
            .into_iter()
            .enumerate()
            .map(|(at, choices)| {
                let mut left = if at == 0 {
                    vec![pointer.clone(), label.clone()]
                } else {
                    vec![Span::raw(" ".repeat(lead))]
                };
                left.extend(choices);
                if at == 0 {
                    spread(left, source.clone(), width)
                } else {
                    left
                }
            })
            .collect()
    }

    /// Each choice as its own spans: the one in use bold, a ticked column with its place.
    fn choices(&self, kit: &Kit<'_>, setting: Setting, picked: bool) -> Vec<Vec<Span<'static>>> {
        let pick = |words: &str, on: bool| {
            vec![if on {
                Span::styled(words.to_owned(), kit.bold(Role::Card))
            } else {
                kit.dim(words.to_owned())
            }]
        };
        let file = &self.file;
        match setting {
            Setting::Period => PERIODS
                .iter()
                .map(|(period, words)| pick(words, *period == file.period))
                .collect(),
            Setting::Group => GROUPS
                .iter()
                .map(|(group, words)| pick(words, *group == file.group))
                .collect(),
            Setting::Measure => MEASURES
                .iter()
                .map(|measure| pick(measure.word(), *measure == file.measure))
                .collect(),
            Setting::Total => vec![pick("shown", file.total), pick("hidden", !file.total)],
            Setting::Columns => {
                let glyphs = &kit.theme.glyphs;
                self.ticks()
                    .into_iter()
                    .enumerate()
                    .map(|(at, (column, on))| {
                        let mut words = if on {
                            Span::styled(column.word(), kit.bold(Role::Card))
                        } else {
                            kit.dim(column.word())
                        };
                        // The column space and shift+←→ act on.
                        if picked && at == self.column {
                            words.style = words.style.add_modifier(Modifier::UNDERLINED);
                        }
                        let mut spans = if on {
                            vec![
                                Span::styled(
                                    format!("{} ", glyphs.checkbox_on),
                                    kit.bold(Role::Card),
                                ),
                                words,
                            ]
                        } else {
                            vec![kit.dim(format!("{} ", glyphs.checkbox_off)), words]
                        };
                        if on {
                            let place = file.columns.iter().position(|shown| *shown == column);
                            let place = place.map_or(0, |place| place + 1);
                            spans.push(Span::styled(
                                format!(" {}", ordinal(place)),
                                kit.bold(Role::Card),
                            ));
                        }
                        spans
                    })
                    .collect()
            }
        }
    }
}

/// What `s` sends to write `pairs`, each a key and its value as TOML text, into your
/// `mods/stats.toml` or the project's. Writing a mod's file is the mod files' to do, through
/// their `UiCommand::Mod` save, which checks the whole file before it writes.
fn save_command(pairs: Vec<(String, String)>, project: bool) -> UiCommand {
    UiCommand::Mod {
        change: ModChange::Save {
            name: STATS_MOD.to_owned(),
            pairs,
            origin: origin(project),
        },
    }
}

/// What a second `r` sends to delete your `mods/stats.toml` or the project's: the mod files'
/// `UiCommand::Mod` reset, like [`save_command`].
fn reset_command(project: bool) -> UiCommand {
    UiCommand::Mod {
        change: ModChange::Reset {
            name: STATS_MOD.to_owned(),
            origin: origin(project),
        },
    }
}

/// The stats mod's name, which is its file's: `mods/stats.toml`.
const STATS_MOD: &str = "stats";

fn origin(project: bool) -> ModOrigin {
    if project {
        ModOrigin::Project
    } else {
        ModOrigin::Yours
    }
}

/// The choice after `now` in `choices`, or before it, and round again.
fn next<T: Copy + PartialEq>(choices: &[T], now: T, ahead: bool) -> T {
    let at = choices
        .iter()
        .position(|choice| *choice == now)
        .unwrap_or(0);
    let count = choices.len();
    let to = if ahead {
        (at + 1) % count
    } else {
        (at + count - 1) % count
    };
    choices[to]
}

/// `1st`, `2nd`, `3rd`, `4th`, and so on.
fn ordinal(place: usize) -> String {
    let suffix = match (place % 10, place % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{place}{suffix}")
}
