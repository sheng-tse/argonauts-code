//! One mod's page: `/mods checks`. A live preview drawn by the same drawer the screen uses,
//! and under it the mod's settings: its body and form, where it shows, where its numbers come
//! from, and where its meters turn amber and red. Every change shows in the preview at once,
//! and `s` writes it into the mod's file.
//!
//! A built-in card (context, plan-usage, spend, tokens, runs, waiting) draws from settings of
//! its own, so its page offers only where it shows, which `s` writes into your place lists.

use std::time::Duration;

use argonauts_core::app::UiCommand;
use argonauts_core::settings::mods::view::{ModChange, ModRow, ModRowKind};
use argonauts_core::settings::mods::{
    BuiltinSource, ModBody, ModFile, ModForm, ModOrigin, ModPlace, ModReading, ModSource, builtin,
    span_words,
};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::draw;
use super::rows::{self, Cells, Group, Row, Tick, Top};
use crate::kit::{Drawn, Kit};
use crate::theme::{Role, Theme};
use crate::views::ViewOutcome;
use crate::views::panes::Footer;

const BODY_ROW: usize = 0;
const FORM_ROW: usize = 1;
const SHOWN_ROW: usize = 2;
const PLACES_ROW: usize = 3;
const SOURCE_ROW: usize = 4;
const EVERY_ROW: usize = 5;
const WARN_ROW: usize = 6;
const DANGER_ROW: usize = 7;
const ROWS: usize = 8;

/// How often a command may be set to run from the page. The file takes any time.
const EVERY_STEPS: [u64; 8] = [5, 10, 30, 60, 300, 900, 1800, 3600];
/// The percents a level may be set to from the page.
const LEVEL_STEPS: [f64; 9] = [50.0, 60.0, 70.0, 75.0, 80.0, 85.0, 90.0, 95.0, 100.0];

#[derive(Debug, Clone, PartialEq)]
pub struct ModPage {
    /// What the file in use holds.
    saved: ModFile,
    built_in: Option<ModFile>,
    /// As edited here.
    pub file: ModFile,
    reading: Option<ModReading>,
    note: Option<String>,
    needs_trust: bool,
    path: String,
    /// Where the file in use is, and so where `s` writes: the project's stays the project's,
    /// anything else is saved as yours.
    origin: ModOrigin,
    selected: usize,
    place_cursor: usize,
    reset_armed: bool,
    /// One of the built-in cards, which a file of this name doesn't change: only its places
    /// are offered.
    closed: bool,
}

/// A place as the page says it.
fn place_label(place: ModPlace) -> &'static str {
    match place {
        ModPlace::BottomLine => "bottom line",
        other => other.word(),
    }
}

/// `value` among `steps`, added in its place when it's none of them.
fn with_current<T: PartialOrd + Copy>(steps: &[T], value: T) -> (Vec<T>, usize) {
    let mut all = steps.to_vec();
    let at = match all.iter().position(|step| *step == value) {
        Some(at) => at,
        None => {
            let at = all
                .iter()
                .position(|step| *step > value)
                .unwrap_or(all.len());
            all.insert(at, value);
            at
        }
    };
    (all, at)
}

impl ModPage {
    /// `None` for a mod that draws itself, which has a page of its own.
    pub fn for_row(row: &ModRow) -> Option<Self> {
        let ModRowKind::Card {
            file,
            built_in,
            reading,
            note,
            needs_trust,
            ..
        } = &row.kind
        else {
            return None;
        };
        let closed = builtin::closed_card(&row.name).is_some();
        Some(Self {
            saved: (**file).clone(),
            built_in: built_in.as_deref().cloned(),
            file: (**file).clone(),
            reading: reading.clone(),
            note: note.clone(),
            needs_trust: *needs_trust,
            path: row.path.clone(),
            origin: row.origin,
            selected: if closed { PLACES_ROW } else { BODY_ROW },
            place_cursor: 0,
            reset_armed: false,
            closed,
        })
    }

    pub fn name(&self) -> &str {
        &self.saved.name
    }

    /// Newer numbers, and the file again when nothing here is unsaved.
    pub fn refresh(&mut self, row: &ModRow) {
        let Some(fresh) = Self::for_row(row) else {
            return;
        };
        self.reading = fresh.reading;
        self.note = fresh.note;
        self.needs_trust = fresh.needs_trust;
        self.path = fresh.path;
        self.origin = fresh.origin;
        self.built_in = fresh.built_in;
        if !self.changed() {
            self.saved = fresh.saved.clone();
            self.file = fresh.saved;
        }
    }

    pub fn changed(&self) -> bool {
        self.file != self.saved
    }

    fn save_origin(&self) -> ModOrigin {
        match self.origin {
            ModOrigin::Project => ModOrigin::Project,
            _ => ModOrigin::Yours,
        }
    }

    /// Where a row's value comes from: the built-in card while it says the same, else the
    /// file.
    fn origin_of(&self, same_as_built_in: impl Fn(&ModFile) -> bool) -> &'static str {
        match &self.built_in {
            Some(built_in) if same_as_built_in(built_in) => ModOrigin::BuiltIn.word(),
            _ => self.save_origin().word(),
        }
    }

    fn every(&self) -> Option<Duration> {
        match &self.file.data {
            ModSource::Command { every, .. } => Some(*every),
            ModSource::Builtin { .. } => None,
        }
    }

    fn groups(&self, kit: &Kit<'_>) -> Vec<Group> {
        let all = self.all_groups(kit);
        if !self.closed {
            return all;
        }
        all.into_iter()
            .filter_map(|group| {
                let rows: Vec<Row> = group
                    .rows
                    .into_iter()
                    .filter(|row| row.label == "Places")
                    .collect();
                (!rows.is_empty()).then_some(Group {
                    title: group.title,
                    rows,
                })
            })
            .collect()
    }

    fn all_groups(&self, kit: &Kit<'_>) -> Vec<Group> {
        let file = &self.file;
        let words = |all: Vec<&str>| all.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let level_row = |label, value: f64, same: &dyn Fn(&ModFile) -> bool| {
            let (steps, current) = with_current(&LEVEL_STEPS, value);
            Row {
                label,
                cells: Cells::Choice {
                    options: steps.iter().map(|step| format!("{step:.0}%")).collect(),
                    current,
                },
                origin: self.origin_of(same),
            }
        };
        let source_row = match &file.data {
            ModSource::Builtin { source } => Row {
                label: "Source",
                cells: Cells::Choice {
                    options: words(BuiltinSource::ALL.iter().map(|s| s.word()).collect()),
                    current: BuiltinSource::ALL
                        .iter()
                        .position(|have| have == source)
                        .unwrap_or(0),
                },
                origin: self.origin_of(|built| built.data == file.data),
            },
            ModSource::Command {
                command, timeout, ..
            } => Row {
                label: "Command",
                cells: Cells::Words(vec![
                    kit.strong(command.join(" ")),
                    kit.dim(format!(
                        " · stopped after {} · the file changes it",
                        span_words(*timeout)
                    )),
                ]),
                origin: self.origin_of(|built| built.data == file.data),
            },
        };
        let every_row = match self.every() {
            Some(every) => {
                let (steps, current) = with_current(&EVERY_STEPS, every.as_secs());
                Row {
                    label: "Every",
                    cells: Cells::Choice {
                        options: steps
                            .iter()
                            .map(|secs| span_words(Duration::from_secs(*secs)))
                            .collect(),
                        current,
                    },
                    origin: self.origin_of(|built| built.data == file.data),
                }
            }
            None => Row {
                label: "Every",
                cells: Cells::Words(vec![kit.dim("as the numbers change")]),
                origin: ModOrigin::BuiltIn.word(),
            },
        };
        vec![
            Group {
                title: "Look",
                rows: vec![
                    Row {
                        label: "Body",
                        cells: Cells::Choice {
                            options: words(ModBody::ALL.iter().map(|body| body.word()).collect()),
                            current: ModBody::ALL
                                .iter()
                                .position(|body| *body == file.body)
                                .unwrap_or(0),
                        },
                        origin: self.origin_of(|built| built.body == file.body),
                    },
                    Row {
                        label: "Form",
                        cells: Cells::Choice {
                            options: words(ModForm::ALL.iter().map(|form| form.word()).collect()),
                            current: ModForm::ALL
                                .iter()
                                .position(|form| *form == file.form)
                                .unwrap_or(0),
                        },
                        origin: self.origin_of(|built| built.form == file.form),
                    },
                ],
            },
            Group {
                title: "Shows",
                rows: vec![
                    Row {
                        label: "Shown",
                        cells: Cells::Choice {
                            options: words(vec!["yes", "no"]),
                            current: usize::from(!file.shown),
                        },
                        origin: self.origin_of(|built| built.shown == file.shown),
                    },
                    Row {
                        label: "Places",
                        cells: Cells::Ticks {
                            items: ModPlace::ALL
                                .iter()
                                .map(|place| Tick {
                                    label: place_label(*place).to_owned(),
                                    on: file.shows(*place),
                                    place: None,
                                })
                                .collect(),
                            cursor: self.place_cursor,
                        },
                        origin: self.origin_of(|built| built.places == file.places),
                    },
                ],
            },
            Group {
                title: "Numbers",
                rows: vec![source_row, every_row],
            },
            Group {
                title: "Levels",
                rows: vec![
                    level_row("Amber from", file.levels.warn, &|built| {
                        built.levels.warn == file.levels.warn
                    }),
                    level_row("Red from", file.levels.danger, &|built| {
                        built.levels.danger == file.levels.danger
                    }),
                ],
            },
        ]
    }

    /// ←→ on the selected row.
    fn step(&mut self, forward: bool) {
        fn next<T: Copy + PartialEq>(all: &[T], current: T, forward: bool) -> T {
            let at = all.iter().position(|value| *value == current).unwrap_or(0);
            all[rows::step(at, all.len(), forward)]
        }
        let file = &mut self.file;
        match self.selected {
            BODY_ROW => file.body = next(&ModBody::ALL, file.body, forward),
            FORM_ROW => file.form = next(&ModForm::ALL, file.form, forward),
            SHOWN_ROW => file.shown = !forward,
            PLACES_ROW => {
                self.place_cursor = rows::step(self.place_cursor, ModPlace::ALL.len(), forward);
            }
            SOURCE_ROW => {
                if let ModSource::Builtin { source } = &mut file.data {
                    *source = next(&BuiltinSource::ALL, *source, forward);
                }
            }
            EVERY_ROW => {
                if let ModSource::Command { every, .. } = &mut file.data {
                    let (steps, at) = with_current(&EVERY_STEPS, every.as_secs());
                    *every = Duration::from_secs(steps[rows::step(at, steps.len(), forward)]);
                }
            }
            WARN_ROW => {
                let (steps, at) = with_current(&LEVEL_STEPS, file.levels.warn);
                let warn = steps[rows::step(at, steps.len(), forward)];
                // Amber never starts above red.
                file.levels.warn = warn.min(file.levels.danger);
            }
            DANGER_ROW => {
                let (steps, at) = with_current(&LEVEL_STEPS, file.levels.danger);
                let danger = steps[rows::step(at, steps.len(), forward)];
                file.levels.danger = danger.max(file.levels.warn);
            }
            _ => {}
        }
    }

    /// Space: ticks the place under the cursor on or off, keeping the places in the order the
    /// page lists them. On any other row it's the next choice.
    fn tick(&mut self) {
        if self.selected == SHOWN_ROW {
            self.file.shown = !self.file.shown;
            return;
        }
        if self.selected != PLACES_ROW {
            self.step(true);
            return;
        }
        let place = ModPlace::ALL[self.place_cursor.min(ModPlace::ALL.len() - 1)];
        let places = &mut self.file.places;
        match places.iter().position(|have| *have == place) {
            Some(at) => {
                places.remove(at);
            }
            None => {
                places.push(place);
                places.sort();
            }
        }
    }

    fn save(&mut self) -> ViewOutcome {
        let pairs = self.file.changes_from(&self.saved);
        if pairs.is_empty() {
            return ViewOutcome::Notice("Nothing to save: the page matches the file".to_owned());
        }
        self.saved = self.file.clone();
        ViewOutcome::Send(UiCommand::Mod {
            change: ModChange::Save {
                name: self.name().to_owned(),
                pairs,
                origin: self.save_origin(),
            },
        })
    }

    /// `r`: what's unsaved goes first. With nothing unsaved, a second `r` deletes a file that
    /// stands over a built-in card, which brings that card back.
    fn reset(&mut self) -> ViewOutcome {
        if self.changed() {
            self.file = self.saved.clone();
            return ViewOutcome::Notice("Back to what the file holds".to_owned());
        }
        let Some(built_in) = self.built_in.clone() else {
            return ViewOutcome::Notice(format!(
                "{} is only in its file, so there's nothing to go back to. Delete {} to remove \
                 it",
                self.name(),
                self.path
            ));
        };
        if self.origin == ModOrigin::BuiltIn {
            return ViewOutcome::Notice(format!("Already the built-in {}", self.name()));
        }
        if !self.reset_armed {
            self.reset_armed = true;
            return ViewOutcome::Notice(format!(
                "r again deletes {} and brings the built-in {} back",
                self.path,
                self.name()
            ));
        }
        let origin = self.save_origin();
        self.saved = built_in.clone();
        self.file = built_in;
        ViewOutcome::Send(UiCommand::Mod {
            change: ModChange::Reset {
                name: self.name().to_owned(),
                origin,
            },
        })
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ViewOutcome {
        let armed = std::mem::take(&mut self.reset_armed);
        match key.code {
            // A built-in card's page has the one row.
            KeyCode::Up | KeyCode::Down if self.closed => {}
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(ROWS - 1),
            KeyCode::Left => self.step(false),
            KeyCode::Right => self.step(true),
            KeyCode::Char(' ') | KeyCode::Enter => self.tick(),
            KeyCode::Char('s') => return self.save(),
            KeyCode::Char('r') => {
                self.reset_armed = armed;
                return self.reset();
            }
            KeyCode::Char('t') if self.needs_trust => {
                self.needs_trust = false;
                return ViewOutcome::Send(UiCommand::Mod {
                    change: ModChange::Trust {
                        name: self.name().to_owned(),
                    },
                });
            }
            KeyCode::Esc => return ViewOutcome::Close,
            _ => return ViewOutcome::Ignored,
        }
        ViewOutcome::Consumed
    }

    /// One line: what it draws and from what.
    fn description(&self) -> String {
        if self.closed {
            return format!(
                "Built in, drawn from Argonauts Code's own numbers. How it draws is ui.mods.{} in \
                 /config; here you pick where it shows",
                self.name()
            );
        }
        let from = match &self.file.data {
            ModSource::Builtin { source } => {
                format!("Argonauts Code's own numbers: {}", source.describe())
            }
            ModSource::Command { command, every, .. } => {
                format!("{}, run every {}", command.join(" "), span_words(*every))
            }
        };
        let draws = self.file.body.describe();
        let mut text = format!("{draws}, from {from}");
        if let Some(first) = text.get(..1) {
            text = format!("{}{}", first.to_uppercase(), &text[1..]);
        }
        text
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let kit = Kit::new(theme);
        let width = usize::from(area.width.saturating_sub(2));
        let groups = self.groups(&kit);
        let sample;
        let reading = match &self.reading {
            Some(reading) => reading,
            None => {
                sample = ModReading::sample_for(&self.file);
                &sample
            }
        };
        let mut caption = Vec::new();
        if self.needs_trust {
            caption.push(kit.span(
                "The project's mod, and it runs a command. t trusts it as it is now; until then \
                 these are sample numbers",
                Role::Warn,
            ));
        } else if self.changed() {
            caption.push(kit.span("Not saved yet. s saves it", Role::Warn));
        } else if self.closed {
            caption.push(kit.dim("A file of this name changes neither how nor what it draws"));
        } else if self.reading.is_none() {
            let why = self.note.as_deref().unwrap_or("it has no numbers yet");
            caption.push(kit.dim(format!("Sample numbers: {why}")));
        } else if let Some(note) = &self.note {
            caption.push(kit.dim(note.clone()));
        }
        let save_to = match self.save_origin() {
            ModOrigin::Project => "s save to the project",
            _ => "s save to yours",
        };
        let mut hints = if self.closed {
            vec![
                ("←→ place", 2),
                ("space tick", 1),
                ("r reset", 1),
                (save_to, 3),
            ]
        } else {
            vec![
                ("↑↓ setting", 2),
                ("←→ change", 2),
                ("space tick", 1),
                ("r reset", 1),
                (save_to, 3),
            ]
        };
        if self.needs_trust {
            hints.push(("t trust", 3));
        }
        hints.push(("esc back", 0));
        let description = self.description();
        let title = self.file.title.clone();
        let top = Top {
            crumbs: &["Config", "Mods", &title],
            path: &self.path,
            description: &description,
            // The card draws itself elsewhere; a preview of the file wouldn't be it.
            preview: if self.closed {
                Drawn::new()
            } else {
                draw::preview(&kit, &self.file, reading, width)
            },
            caption,
        };
        let selected = if self.closed { 0 } else { self.selected };
        rows::draw_page(
            buf,
            area,
            theme,
            top,
            &groups,
            selected,
            Footer::hints(&hints),
        );
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::mods::{builtin, check};
    use crossterm::event::KeyModifiers;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn card(name: &str) -> ModFile {
        let text = builtin::builtin(name).unwrap().text;
        check::read(name, text).unwrap()
    }

    fn built_in_row(name: &str) -> ModRow {
        ModRow {
            name: name.to_owned(),
            origin: ModOrigin::BuiltIn,
            path: format!("~/.argonauts/mods/{name}.toml"),
            kind: ModRowKind::Card {
                file: Box::new(card(name)),
                built_in: Some(Box::new(card(name))),
                reading: None,
                note: None,
                needs_trust: false,
                asks_for_line: false,
                hidden_by_line: false,
            },
        }
    }

    /// A card of yours that isn't one of the built-in ones, made from the context card's file.
    fn own_row(name: &str) -> ModRow {
        let text = builtin::builtin("context").unwrap().text.replacen(
            "\"context\"",
            &format!("\"{name}\""),
            1,
        );
        ModRow {
            name: name.to_owned(),
            origin: ModOrigin::Yours,
            path: format!("~/.argonauts/mods/{name}.toml"),
            kind: ModRowKind::Card {
                file: Box::new(check::read(name, &text).unwrap()),
                built_in: None,
                reading: None,
                note: None,
                needs_trust: false,
                asks_for_line: false,
                hidden_by_line: false,
            },
        }
    }

    fn checks_row(origin: ModOrigin, needs_trust: bool) -> ModRow {
        let (name, text) = builtin::STARTERS
            .iter()
            .find(|(name, _)| *name == "checks")
            .unwrap();
        ModRow {
            name: (*name).to_owned(),
            origin,
            path: ".argonauts/mods/checks.toml".to_owned(),
            kind: ModRowKind::Card {
                file: Box::new(check::read(name, text).unwrap()),
                built_in: None,
                reading: None,
                note: Some("./checks.sh isn't there yet".to_owned()),
                needs_trust,
                asks_for_line: false,
                hidden_by_line: false,
            },
        }
    }

    #[test]
    fn arrows_change_the_body_and_s_saves_what_changed_as_yours() {
        let mut page = ModPage::for_row(&own_row("tokens-week")).unwrap();
        page.handle_key(key(KeyCode::Right));
        assert_eq!(page.file.body, ModBody::Meters);
        assert!(page.changed());
        match page.handle_key(key(KeyCode::Char('s'))) {
            ViewOutcome::Send(UiCommand::Mod {
                change:
                    ModChange::Save {
                        name,
                        pairs,
                        origin,
                    },
            }) => {
                assert_eq!(name, "tokens-week");
                assert_eq!(pairs, [("body".to_owned(), "\"meters\"".to_owned())]);
                assert_eq!(origin, ModOrigin::Yours);
            }
            other => panic!("{other:?}"),
        }
        assert!(!page.changed());
    }

    #[test]
    fn space_ticks_a_place_and_keeps_the_page_s_order() {
        let mut page = ModPage::for_row(&built_in_row("context")).unwrap();
        page.selected = PLACES_ROW;
        page.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(page.file.places, [ModPlace::Above, ModPlace::Context]);
        page.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(page.file.places, [ModPlace::Context]);
    }

    #[test]
    fn amber_never_starts_above_red() {
        let mut page = ModPage::for_row(&own_row("tokens-week")).unwrap();
        page.selected = WARN_ROW;
        for _ in 0..12 {
            page.handle_key(key(KeyCode::Right));
        }
        assert_eq!(page.file.levels.warn, 95.0);
        page.selected = DANGER_ROW;
        for _ in 0..12 {
            page.handle_key(key(KeyCode::Left));
        }
        assert_eq!(page.file.levels.danger, 95.0);
    }

    #[test]
    fn a_built_in_source_steps_through_the_sources_and_a_command_through_its_times() {
        let mut page = ModPage::for_row(&own_row("tokens-week")).unwrap();
        page.selected = SOURCE_ROW;
        page.handle_key(key(KeyCode::Right));
        assert_eq!(
            page.file.data,
            ModSource::Builtin {
                source: BuiltinSource::PlanUsage
            }
        );
        let mut checks = ModPage::for_row(&checks_row(ModOrigin::Yours, false)).unwrap();
        checks.selected = EVERY_ROW;
        checks.handle_key(key(KeyCode::Right));
        assert_eq!(checks.every(), Some(Duration::from_secs(300)));
        // The command itself is the file's to change.
        checks.selected = SOURCE_ROW;
        let before = checks.file.clone();
        checks.handle_key(key(KeyCode::Right));
        assert_eq!(checks.file, before);
    }

    #[test]
    fn r_discards_edits_then_asks_before_deleting_your_file() {
        let mut row = built_in_row("context");
        row.origin = ModOrigin::Yours;
        if let ModRowKind::Card { file, .. } = &mut row.kind {
            file.form = ModForm::Line;
        }
        let mut page = ModPage::for_row(&row).unwrap();
        page.handle_key(key(KeyCode::Char(' ')));
        assert!(page.changed());
        assert!(matches!(
            page.handle_key(key(KeyCode::Char('r'))),
            ViewOutcome::Notice(_)
        ));
        assert!(!page.changed());
        assert!(matches!(
            page.handle_key(key(KeyCode::Char('r'))),
            ViewOutcome::Notice(_)
        ));
        assert!(matches!(
            page.handle_key(key(KeyCode::Char('r'))),
            ViewOutcome::Send(UiCommand::Mod {
                change: ModChange::Reset { .. }
            })
        ));
        assert_eq!(page.file.form, ModForm::Full);
        // A mod only its file holds has nothing to go back to.
        let mut checks = ModPage::for_row(&checks_row(ModOrigin::Yours, false)).unwrap();
        for _ in 0..2 {
            assert!(matches!(
                checks.handle_key(key(KeyCode::Char('r'))),
                ViewOutcome::Notice(_)
            ));
        }
    }

    #[test]
    fn the_project_s_mod_saves_to_the_project_and_t_trusts_it() {
        let mut page = ModPage::for_row(&checks_row(ModOrigin::Project, true)).unwrap();
        assert_eq!(page.save_origin(), ModOrigin::Project);
        assert!(matches!(
            page.handle_key(key(KeyCode::Char('t'))),
            ViewOutcome::Send(UiCommand::Mod {
                change: ModChange::Trust { .. }
            })
        ));
        // Once trusted, t is no key of the page's.
        assert!(matches!(
            page.handle_key(key(KeyCode::Char('t'))),
            ViewOutcome::Ignored
        ));
    }

    #[test]
    fn a_refresh_brings_numbers_and_keeps_unsaved_edits() {
        let mut page = ModPage::for_row(&own_row("tokens-week")).unwrap();
        page.handle_key(key(KeyCode::Right));
        let mut row = own_row("tokens-week");
        if let ModRowKind::Card { reading, .. } = &mut row.kind {
            *reading = Some(ModReading::sample(ModBody::Stack));
        }
        page.refresh(&row);
        assert!(page.reading.is_some());
        assert_eq!(page.file.body, ModBody::Meters);
        assert!(page.changed());
    }

    /// A built-in card draws from settings of its own, so its page offers where it shows and
    /// nothing a file of its name would hold.
    #[test]
    fn a_built_in_card_s_page_offers_only_its_places() {
        let theme = Theme::dark();
        let mut page = ModPage::for_row(&built_in_row("plan-usage")).unwrap();
        page.handle_key(key(KeyCode::Down));
        page.handle_key(key(KeyCode::Right));
        let before = page.file.clone();
        assert_eq!(page.file, before, "←→ moves along the places");
        page.handle_key(key(KeyCode::Char(' ')));
        assert_ne!(page.file.places, before.places, "space ticks the place");
        let area = Rect::new(0, 0, 120, 30);
        let mut buf = Buffer::empty(area);
        page.render(area, &mut buf, &theme);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(text.contains("Places"), "{text}");
        assert!(text.contains("ui.mods.plan-usage"), "{text}");
        for gone in ["Body", "Form", "Source", "Amber from", "↑↓ setting"] {
            assert!(!text.contains(gone), "{gone} shows:\n{text}");
        }
    }

    #[test]
    fn a_page_draws_at_any_size() {
        let theme = Theme::dark();
        let page = ModPage::for_row(&checks_row(ModOrigin::Yours, false)).unwrap();
        for (width, height) in [(120, 40), (80, 24), (40, 12), (20, 6), (3, 2)] {
            let area = Rect::new(0, 0, width, height);
            let mut buf = Buffer::empty(area);
            page.render(area, &mut buf, &theme);
        }
    }
}
