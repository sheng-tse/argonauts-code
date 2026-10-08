//! The bottom line's page: `/mods bottom-line`. It edits `mods/bottom-line.toml`: the line's
//! form, the parts on each side in order, how plan usage draws there, and what a narrow
//! terminal gives up first. Every change shows in the preview at once, and `s` writes it.
//! Each CLI's color is picked here too. That one is saved at once into your settings, since a
//! CLI keeps its color in every mod and not only on this line.
//!
//! The preview draws the settings being edited, so the page can show a change before any
//! file has it, with the live line's own drawer ([`preview_line`]).

use argonauts_core::app::UiCommand;
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::bottom_line::BottomLineSettings;
use argonauts_core::settings::mods::bottom_line::{
    Choice, LEFT_OFFERS, PageEdits as _, RIGHT_OFFERS, part_label, read,
};
use argonauts_core::settings::mods::view::{LineCli, LineNumbers, ModChange};
use argonauts_core::settings::mods::{ModOrigin, builtin};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use super::rows::{self, Cells, Group, PageGlyphs, Row, Tick, Top};
use crate::bridge::bottom_line::{Sample, SampleCli};
use crate::text::marks::decorative;
use crate::theme::{AgentColor, Role, Theme};
use crate::views::ViewOutcome;
use crate::views::panes::Footer;
use crate::widgets::{Drawn, Frame, Kit, spread};

/// The colors a CLI can take: Claude Code's eight for telling agents apart, less the amber
/// and the red, which on this line mean a limit is near or reached.
const PALETTE: [(AgentColor, &str); 6] = [
    (AgentColor::Blue, "blue"),
    (AgentColor::Purple, "purple"),
    (AgentColor::Cyan, "cyan"),
    (AgentColor::Orange, "orange"),
    (AgentColor::Pink, "pink"),
    (AgentColor::Green, "green"),
];

const FORM_ROW: usize = 0;
const COLOURS_ROW: usize = 1;
const LEFT_ROW: usize = 3;
const RIGHT_ROW: usize = 4;
const CLIS_ROW: usize = 5;
const WINDOWS_ROW: usize = 6;
const SHOWS_ROW: usize = 7;
const RESETS_ROW: usize = 8;
const GIVES_UP_ROW: usize = 9;
const ROWS: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub struct LinePage {
    /// What the file in use holds.
    saved: BottomLineSettings,
    built_in: BottomLineSettings,
    /// As edited here.
    pub file: BottomLineSettings,
    numbers: LineNumbers,
    /// Your mods that can show on the bottom line, offered on both sides.
    own_mods: Vec<String>,
    path: String,
    /// Where `s` writes: yours, or the project's when the file in use is the project's.
    origin: ModOrigin,
    /// Why the file couldn't be read as it is, when the page starts from the built-in one.
    problem: Option<String>,
    selected: usize,
    /// The part the keys act on, on the left and on the right.
    cursors: [usize; 2],
    /// And among the parts a narrow terminal gives up.
    given_up_cursor: usize,
    /// And the CLI whose color they pick.
    colour_cursor: usize,
    /// `r` was pressed once on a file of yours, and again deletes it.
    reset_armed: bool,
}

impl LinePage {
    pub fn new(
        text: &str,
        built_in_text: &str,
        origin: ModOrigin,
        path: String,
        numbers: LineNumbers,
        own_mods: Vec<String>,
    ) -> Self {
        let built_in = read(built_in_text).unwrap_or_default();
        let (saved, problem) = match read(text) {
            Ok(file) => (file, None),
            Err(problem) => (built_in.clone(), Some(problem)),
        };
        Self {
            file: saved.clone(),
            saved,
            built_in,
            numbers,
            own_mods,
            path,
            origin: match origin {
                ModOrigin::Project => ModOrigin::Project,
                _ => ModOrigin::Yours,
            },
            problem,
            selected: LEFT_ROW,
            cursors: [0, 0],
            given_up_cursor: 0,
            colour_cursor: 0,
            reset_armed: false,
        }
    }

    /// Newer numbers, and the file again when nothing here is unsaved.
    pub fn refresh(&mut self, text: &str, origin: ModOrigin, numbers: LineNumbers) {
        self.numbers = numbers;
        if self.changed() {
            return;
        }
        if let Ok(file) = read(text) {
            self.saved = file.clone();
            self.file = file;
            self.problem = None;
            if origin == ModOrigin::Project {
                self.origin = ModOrigin::Project;
            }
        }
    }

    pub fn changed(&self) -> bool {
        self.file != self.saved
    }

    /// The parts a side's row lists: its own in order, then what it could take.
    fn ticks(&self, left: bool) -> Vec<Tick> {
        let side = self.file.side(left);
        let other = self.file.side(!left);
        let offers: &[&str] = if left { &LEFT_OFFERS } else { &RIGHT_OFFERS };
        let mut items: Vec<Tick> = side
            .iter()
            .enumerate()
            .map(|(index, part)| Tick {
                label: part_label(part),
                on: true,
                place: Some(index + 1),
            })
            .collect();
        let offered = offers
            .iter()
            .copied()
            .chain(self.own_mods.iter().map(String::as_str))
            .filter(|part| !side.iter().any(|have| have == part))
            .filter(|part| !other.iter().any(|have| have == part));
        for part in offered {
            if !items.iter().any(|item| item.label == part_label(part)) {
                items.push(Tick {
                    label: part_label(part),
                    on: false,
                    place: None,
                });
            }
        }
        items
    }

    /// The part names behind [`Self::ticks`], in the same order.
    fn tick_names(&self, left: bool) -> Vec<String> {
        let side = self.file.side(left);
        let other = self.file.side(!left);
        let offers: &[&str] = if left { &LEFT_OFFERS } else { &RIGHT_OFFERS };
        let mut names: Vec<String> = side.to_vec();
        for part in offers
            .iter()
            .copied()
            .chain(self.own_mods.iter().map(String::as_str))
        {
            let taken = |list: &[String]| list.iter().any(|have| have == part);
            if !taken(side) && !taken(other) && !names.iter().any(|have| have == part) {
                names.push(part.to_owned());
            }
        }
        names
    }

    fn origin_of(&self, same: bool) -> &'static str {
        if same {
            ModOrigin::BuiltIn.word()
        } else {
            self.origin.word()
        }
    }

    /// A key that takes one of a few words: its choices as the page says them, the one in
    /// the file marked, and whose it is.
    fn choice<T: Choice>(&self, label: &'static str, current: T, built_in: T) -> Row {
        Row {
            label,
            cells: Cells::Choice {
                options: T::ALL
                    .iter()
                    .map(|value| value.label().to_owned())
                    .collect(),
                current: T::ALL
                    .iter()
                    .position(|value| *value == current)
                    .unwrap_or(0),
            },
            origin: self.origin_of(current == built_in),
        }
    }

    fn groups(&self, kit: &Kit<'_>) -> Vec<Group> {
        let (file, built) = (&self.file, &self.built_in);
        let (plan, built_plan) = (&file.plan_usage, &built.plan_usage);
        let (colours, colours_from) = self.colour_words(kit);
        let gives_up = self.gives_up_words(kit);
        vec![
            Group {
                title: "Look",
                rows: vec![
                    self.choice("Form", file.form, built.form),
                    Row {
                        label: "Colours",
                        cells: Cells::Words(colours),
                        origin: colours_from,
                    },
                    Row {
                        label: "Warn from",
                        cells: Cells::Words(vec![
                            kit.strong("90%"),
                            kit.dim(" amber, with the reset time · out is red"),
                        ]),
                        origin: ModOrigin::BuiltIn.word(),
                    },
                ],
            },
            Group {
                title: "Parts",
                rows: vec![
                    Row {
                        label: "Left",
                        cells: Cells::Ticks {
                            items: self.ticks(true),
                            cursor: self.cursors[0],
                        },
                        origin: self.origin_of(file.left == built.left),
                    },
                    Row {
                        label: "Right",
                        cells: Cells::Ticks {
                            items: self.ticks(false),
                            cursor: self.cursors[1],
                        },
                        origin: self.origin_of(file.right == built.right),
                    },
                ],
            },
            Group {
                title: "Plan usage",
                rows: vec![
                    self.choice("CLIs", plan.show, built_plan.show),
                    self.choice("Windows", plan.windows, built_plan.windows),
                    self.choice("Shows", plan.shown_as, built_plan.shown_as),
                    self.choice("Resets", plan.resets, built_plan.resets),
                ],
            },
            Group {
                title: "Narrow terminal",
                rows: vec![Row {
                    label: "Gives up first",
                    cells: Cells::Words(gives_up),
                    origin: self.origin_of(file.narrow.drop == built.narrow.drop),
                }],
            },
        ]
    }

    /// `■ Claude  ■ Codex  ■ agy`, each square in the CLI's color, then what the keys do, and
    /// whose the colors are: the theme's, or yours once you picked one. While the row is
    /// selected the CLI the keys act on is underlined and its color is named.
    fn colour_words(&self, kit: &Kit<'_>) -> (Vec<Span<'static>>, &'static str) {
        let glyphs = PageGlyphs::for_kit(kit);
        let on_row = self.selected == COLOURS_ROW;
        let clis = clis(&self.numbers);
        let cursor = self.colour_cursor.min(clis.len().saturating_sub(1));
        let mut words = Vec::new();
        for (index, cli) in clis.iter().enumerate() {
            words.push(Span::styled(
                format!("{} ", glyphs.square),
                kit.fg(cli_role(&cli.id)),
            ));
            let mut name = kit.strong(cli.label.clone());
            if on_row && index == cursor {
                name.style = name.style.add_modifier(Modifier::UNDERLINED);
            }
            words.push(name);
            words.push(Span::raw("  "));
        }
        let picked = |cli: &LineCli| own_role(&cli.id).and_then(|role| picked(kit.theme, role));
        let from = if clis.iter().any(|cli| picked(cli).is_some()) {
            ModOrigin::Yours.word()
        } else {
            "theme"
        };
        let hint = match clis.get(cursor).filter(|_| on_row) {
            None => "one per CLI, from the theme · pick…".to_owned(),
            Some(cli) if own_role(&cli.id).is_none() => format!(
                "{} has no colour of its own in the theme, so it draws in grey",
                cli.label
            ),
            Some(cli) => {
                let color = picked(cli).map_or("the theme's", |(_, name)| name);
                format!("{} is {color} · space picks another", cli.label)
            }
        };
        words.push(kit.dim(hint));
        (words, from)
    }

    /// Gives the CLI under the cursor the next color, or the one before. After the last comes
    /// the theme's own again. It is saved at once into your settings, as `[ui.colors]`, since
    /// a CLI keeps its color in every mod and not only on this line.
    fn pick_colour(&self, theme: &Theme, forward: bool) -> ViewOutcome {
        let clis = clis(&self.numbers);
        let Some(cli) = clis.get(self.colour_cursor.min(clis.len().saturating_sub(1))) else {
            return ViewOutcome::Consumed;
        };
        let Some(role) = own_role(&cli.id) else {
            return ViewOutcome::Notice(format!(
                "{} has no colour of its own in the theme to change",
                cli.label
            ));
        };
        // `None` is the theme's own. A color that is this role, or follows it, can't be
        // picked: it would follow itself.
        let mut choices: Vec<Option<AgentColor>> = vec![None];
        choices.extend(
            PALETTE
                .iter()
                .map(|(color, _)| *color)
                .filter(|color| color.role() != role && theme.clone().link(role, color.role()))
                .map(Some),
        );
        let now = picked(theme, role).map(|(color, _)| color);
        let at = choices
            .iter()
            .position(|choice| *choice == now)
            .unwrap_or(0);
        let step = if forward { 1 } else { choices.len() - 1 };
        let key = format!("ui.colors.{}", role.name());
        ViewOutcome::Send(match choices[(at + step) % choices.len()] {
            Some(color) => UiCommand::SetSetting {
                key,
                value: format!("\"{}\"", color.role().name()),
                scope: SettingScope::Global,
            },
            None => UiCommand::ResetSetting {
                key,
                scope: SettingScope::Global,
            },
        })
    }

    /// `spend → branch → then the line's own order`: the parts a narrow terminal gives up, the
    /// first to go first, with the one the keys act on underlined while the row is selected.
    fn gives_up_words(&self, kit: &Kit<'_>) -> Vec<Span<'static>> {
        let on_row = self.selected == GIVES_UP_ROW;
        let mut words = Vec::new();
        for (index, part) in self.file.gives_up().into_iter().enumerate() {
            let mut word = kit.strong(part_label(part));
            if on_row && index == self.given_up_cursor {
                word.style = word.style.add_modifier(Modifier::UNDERLINED);
            }
            words.push(word);
            words.push(kit.strong(" → "));
        }
        let rest = if words.is_empty() {
            "the line's own order"
        } else {
            "then the line's own order"
        };
        words.push(kit.strong(rest));
        words
    }

    /// ←→ on the selected row: the next choice, or the next part.
    fn step(&mut self, forward: bool) {
        fn next<T: Choice>(current: T, forward: bool) -> T {
            let at = T::ALL
                .iter()
                .position(|value| *value == current)
                .unwrap_or(0);
            T::ALL[rows::step(at, T::ALL.len(), forward)]
        }
        let file = &mut self.file;
        let plan = &mut file.plan_usage;
        match self.selected {
            FORM_ROW => file.form = next(file.form, forward),
            CLIS_ROW => plan.show = next(plan.show, forward),
            WINDOWS_ROW => plan.windows = next(plan.windows, forward),
            SHOWS_ROW => plan.shown_as = next(plan.shown_as, forward),
            RESETS_ROW => plan.resets = next(plan.resets, forward),
            LEFT_ROW | RIGHT_ROW => {
                let left = self.selected == LEFT_ROW;
                let count = self.tick_names(left).len();
                let cursor = &mut self.cursors[usize::from(!left)];
                *cursor = rows::step(*cursor, count, forward);
            }
            GIVES_UP_ROW => {
                let count = file.gives_up().len();
                self.given_up_cursor = rows::step(self.given_up_cursor, count, forward);
            }
            COLOURS_ROW => {
                let count = clis(&self.numbers).len();
                self.colour_cursor = rows::step(self.colour_cursor, count, forward);
            }
            _ => {}
        }
    }

    /// The side the selected row edits, and the part under its cursor.
    fn part_at_cursor(&self) -> Option<(bool, String)> {
        let left = match self.selected {
            LEFT_ROW => true,
            RIGHT_ROW => false,
            _ => return None,
        };
        let names = self.tick_names(left);
        let cursor = self.cursors[usize::from(!left)].min(names.len().saturating_sub(1));
        names.get(cursor).map(|name| (left, name.clone()))
    }

    fn tick(&mut self, theme: &Theme) -> ViewOutcome {
        if self.selected == COLOURS_ROW {
            return self.pick_colour(theme, true);
        }
        let Some((left, part)) = self.part_at_cursor() else {
            self.step(true);
            return ViewOutcome::Consumed;
        };
        if !self.file.toggle(left, &part) {
            return ViewOutcome::Notice(
                "mode stays on the line: it tells you whether Argonauts will ask. Shift+←→ \
                 moves it"
                    .to_owned(),
            );
        }
        // The cursor follows the part to where it now stands.
        self.follow(left, &part);
        let given_up = self.file.gives_up().len();
        self.given_up_cursor = self.given_up_cursor.min(given_up.saturating_sub(1));
        ViewOutcome::Consumed
    }

    fn follow(&mut self, left: bool, part: &str) {
        if let Some(at) = self.tick_names(left).iter().position(|name| name == part) {
            self.cursors[usize::from(!left)] = at;
        }
    }

    fn reorder(&mut self, earlier: bool, theme: &Theme) -> ViewOutcome {
        if self.selected == COLOURS_ROW {
            return self.pick_colour(theme, !earlier);
        }
        if self.selected == GIVES_UP_ROW {
            if let Some(to) = self.file.shift_given_up(self.given_up_cursor, earlier) {
                self.given_up_cursor = to;
            }
            return ViewOutcome::Consumed;
        }
        if let Some((left, part)) = self.part_at_cursor()
            && self.file.shift(left, &part, earlier)
        {
            self.follow(left, &part);
        }
        ViewOutcome::Consumed
    }

    fn save(&mut self) -> ViewOutcome {
        let pairs = self.file.changes_from(&self.saved);
        if pairs.is_empty() {
            return ViewOutcome::Notice("Nothing to save: the page matches the file".to_owned());
        }
        // The page shows it as saved at once; the file's own text follows from the app.
        self.saved = self.file.clone();
        ViewOutcome::Send(UiCommand::Mod {
            change: ModChange::Save {
                name: builtin::BOTTOM_LINE.to_owned(),
                pairs,
                origin: self.origin,
            },
        })
    }

    /// `r`: what's unsaved goes first. With nothing unsaved, a second `r` deletes your file,
    /// which brings the built-in line back.
    fn reset(&mut self) -> ViewOutcome {
        if self.changed() {
            self.file = self.saved.clone();
            return ViewOutcome::Notice("Back to what the file holds".to_owned());
        }
        if self.saved == self.built_in {
            return ViewOutcome::Notice("Already the built-in bottom line".to_owned());
        }
        if !self.reset_armed {
            self.reset_armed = true;
            return ViewOutcome::Notice(format!(
                "r again deletes {} and brings the built-in line back",
                self.path
            ));
        }
        self.saved = self.built_in.clone();
        self.file = self.built_in.clone();
        ViewOutcome::Send(UiCommand::Mod {
            change: ModChange::Reset {
                name: builtin::BOTTOM_LINE.to_owned(),
                origin: self.origin,
            },
        })
    }

    /// `theme` is the one on screen, which says what color each CLI has now.
    pub fn handle_key(&mut self, key: KeyEvent, theme: &Theme) -> ViewOutcome {
        let armed = std::mem::take(&mut self.reset_armed);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(ROWS - 1),
            KeyCode::Left if shift => return self.reorder(true, theme),
            KeyCode::Right if shift => return self.reorder(false, theme),
            KeyCode::Char('<' | ',') => return self.reorder(true, theme),
            KeyCode::Char('>' | '.') => return self.reorder(false, theme),
            KeyCode::Left => self.step(false),
            KeyCode::Right => self.step(true),
            KeyCode::Char(' ') | KeyCode::Enter => return self.tick(theme),
            KeyCode::Char('s') => return self.save(),
            KeyCode::Char('r') => {
                self.reset_armed = armed;
                return self.reset();
            }
            KeyCode::Esc => return ViewOutcome::Close,
            _ => return ViewOutcome::Ignored,
        }
        ViewOutcome::Consumed
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let kit = Kit::new(theme);
        let width = usize::from(area.width.saturating_sub(2));
        let groups = self.groups(&kit);
        let mut caption = Vec::new();
        if let Some(problem) = &self.problem {
            caption.push(kit.span(
                format!("The file didn't read, so this starts from the built-in line: {problem}"),
                Role::Warning,
            ));
        } else if self.changed() {
            caption.push(kit.span("Not saved yet. s saves it", Role::Warning));
        } else if !has_numbers(&self.numbers) {
            caption.push(kit.dim("Sample numbers, until a CLI reports its plan usage"));
        }
        let save_to = match self.origin {
            ModOrigin::Project => "s save to the project",
            _ => "s save to yours",
        };
        let footer = Footer::hints(&[
            ("↑↓ setting", 2),
            ("←→ change", 2),
            ("space tick", 1),
            ("shift+←→ order", 1),
            ("r reset", 1),
            (save_to, 3),
            ("esc back", 0),
        ]);
        let top = Top {
            crumbs: &["Config", "Mods", "bottom line"],
            path: &self.path,
            description: "The line under the input: its parts, their order and their side",
            preview: preview(&kit, &self.file, &self.numbers, width),
            caption,
        };
        rows::draw_page(buf, area, theme, top, &groups, self.selected, footer);
    }
}

fn has_numbers(numbers: &LineNumbers) -> bool {
    numbers
        .clis
        .iter()
        .any(|cli| cli.five_hours.is_some() || cli.seven_days.is_some())
}

/// Sample numbers, for a preview before any CLI has reported.
fn sample_clis(numbers: &LineNumbers) -> Vec<LineCli> {
    let sample = [
        ("Claude", "claude-code", Some(0.01), Some(0.56)),
        ("Codex", "codex", Some(0.01), Some(0.11)),
        ("agy", "antigravity-cli", Some(0.02), Some(0.01)),
    ];
    let main = numbers
        .clis
        .iter()
        .find(|cli| cli.main)
        .map_or("claude-code", |cli| cli.id.as_str());
    sample
        .into_iter()
        .map(|(label, id, five_hours, seven_days)| LineCli {
            label: label.to_owned(),
            id: id.to_owned(),
            main: id == main,
            five_hours,
            seven_days,
        })
        .collect()
}

/// The CLIs to draw: the crew's with what they reported, or the sample ones.
fn clis(numbers: &LineNumbers) -> Vec<LineCli> {
    if has_numbers(numbers) {
        numbers.clis.clone()
    } else {
        sample_clis(numbers)
    }
}

/// The theme's role for a CLI's color, which `[ui.colors]` can change. The bottom line names
/// them `cli-claude`, `cli-codex` and `cli-agy`. In a theme that doesn't have them, the agent
/// color nearest each stands in, so the page draws and picks the same way either way.
fn own_role(id: &str) -> Option<Role> {
    let (name, stand_in) = match id {
        "claude-code" => ("cli-claude", Role::AgentOrange),
        "codex" => ("cli-codex", Role::AgentBlue),
        "antigravity-cli" => ("cli-agy", Role::AgentCyan),
        _ => return None,
    };
    Some(Role::from_name(name).unwrap_or(stand_in))
}

/// One color per CLI, the same in every mod. A CLI the theme has no color for is grey.
fn cli_role(id: &str) -> Role {
    own_role(id).unwrap_or(Role::CatOther)
}

/// The color you picked for `role` over the theme's own, with its name.
fn picked(theme: &Theme, role: Role) -> Option<(AgentColor, &'static str)> {
    let follows = theme.linked_to(role)?;
    PALETTE
        .iter()
        .copied()
        .find(|(color, _)| color.role() == follows)
}

/// The bottom line as `file` describes it, `width` cells wide, drawn from `numbers` by the
/// live line's own drawer, so it gives things up at the same widths as the real line.
pub fn preview_line(
    kit: &Kit<'_>,
    file: &BottomLineSettings,
    numbers: &LineNumbers,
    width: usize,
) -> Vec<Span<'static>> {
    let sample = Sample {
        mode: numbers.mode.clone(),
        branch: numbers.branch.clone(),
        clis: clis(numbers)
            .into_iter()
            .map(|cli| SampleCli {
                label: cli.label,
                id: cli.id,
                main: cli.main,
                five_hours: cli.five_hours,
                seven_days: cli.seven_days,
            })
            .collect(),
        spend: numbers.spend.clone(),
    };
    crate::bridge::bottom_line::preview(kit.theme, file, &sample, width)
}

/// The preview's box: the input's row with the model at the right, the rule, and the line.
pub fn preview(
    kit: &Kit<'_>,
    file: &BottomLineSettings,
    numbers: &LineNumbers,
    width: usize,
) -> Drawn {
    let frame = Frame {
        left: 0,
        width,
        toggle: None,
        active: false,
    };
    let inner = frame.inner();
    let model = if numbers.model.is_empty() {
        "Opus 5.5"
    } else {
        numbers.model.as_str()
    };
    let mut body = Drawn::new();
    body.push(spread(
        vec![kit.strong(PageGlyphs::for_kit(kit).pointer)],
        vec![kit.dim(model.to_owned())],
        inner,
    ));
    body.push(vec![decorative(
        kit.glyphs.across.repeat(inner),
        kit.style(Role::Rule),
    )]);
    body.push(preview_line(kit, file, numbers, inner));
    frame.draw(kit, body)
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::bottom_line::LineForm;

    use super::*;
    use crate::theme::{ColorLevel, Theme};

    fn text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn page() -> LinePage {
        let built_in = builtin::builtin(builtin::BOTTOM_LINE).unwrap().text;
        LinePage::new(
            built_in,
            built_in,
            ModOrigin::BuiltIn,
            "~/.argonauts/mods/bottom-line.toml".to_owned(),
            LineNumbers::default(),
            vec!["my-ci".to_owned()],
        )
    }

    /// A file of yours with the branch and spend ticked on.
    fn picture_page() -> LinePage {
        LinePage::new(
            "left = [\"mode\", \"branch\"]\nright = [\"plan-usage\", \"spend\"]\n",
            builtin::builtin(builtin::BOTTOM_LINE).unwrap().text,
            ModOrigin::Yours,
            "~/.argonauts/mods/bottom-line.toml".to_owned(),
            LineNumbers::default(),
            Vec::new(),
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A key, with the dark theme on screen.
    fn press(page: &mut LinePage, key: KeyEvent) -> ViewOutcome {
        page.handle_key(key, &Theme::dark())
    }

    #[test]
    fn space_ticks_a_part_and_shift_arrows_reorder_it() {
        let mut page = page();
        assert_eq!(page.selected, LEFT_ROW);
        // The left side holds mode and artifact, and the cursor is on mode, which never goes.
        assert!(matches!(
            press(&mut page, key(KeyCode::Char(' '))),
            ViewOutcome::Notice(_)
        ));
        // Onto `branch`, the first offer, and tick it.
        press(&mut page, key(KeyCode::Right));
        press(&mut page, key(KeyCode::Right));
        press(&mut page, key(KeyCode::Char(' ')));
        assert_eq!(page.file.left, ["mode", "artifact", "branch"]);
        press(&mut page, KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        assert_eq!(page.file.left, ["mode", "branch", "artifact"]);
        assert!(page.changed());
        // Unticking it puts things back.
        press(&mut page, key(KeyCode::Char(' ')));
        assert_eq!(page.file.left, ["mode", "artifact"]);
        assert!(!page.changed());
    }

    #[test]
    fn the_order_parts_are_given_up_in_changes_on_its_own_row() {
        let mut page = picture_page();
        page.selected = GIVES_UP_ROW;
        let words = |page: &LinePage| {
            let theme = Theme::dark().with_level(ColorLevel::None);
            text(&page.gives_up_words(&Kit::new(&theme)))
        };
        assert_eq!(words(&page), "spend → branch → then the line's own order");
        press(
            &mut page,
            KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT),
        );
        assert_eq!(words(&page), "branch → spend → then the line's own order");
        assert_eq!(page.file.narrow.drop, ["branch", "spend"]);
        assert!(page.changed());
        // The cursor went with spend, so the same key at the end does nothing more.
        press(
            &mut page,
            KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT),
        );
        assert_eq!(page.file.narrow.drop, ["branch", "spend"]);
        // Unticking a part takes it out of the row and leaves the order you wrote.
        page.selected = RIGHT_ROW;
        press(&mut page, key(KeyCode::Right));
        press(&mut page, key(KeyCode::Char(' ')));
        assert_eq!(words(&page), "branch → then the line's own order");
        assert_eq!(page.file.narrow.drop, ["branch", "spend"]);
    }

    /// The Colours row's words and whose they are, with `theme` on screen.
    fn colours(page: &LinePage, theme: &Theme) -> (String, &'static str) {
        let (words, from) = page.colour_words(&Kit::new(theme));
        (text(&words), from)
    }

    fn on_colours() -> LinePage {
        let mut page = picture_page();
        page.selected = COLOURS_ROW;
        page
    }

    fn set(role: &str, color: &str) -> UiCommand {
        UiCommand::SetSetting {
            key: format!("ui.colors.{role}"),
            value: format!("\"{color}\""),
            scope: SettingScope::Global,
        }
    }

    fn sent(outcome: ViewOutcome) -> UiCommand {
        match outcome {
            ViewOutcome::Send(command) => command,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_colours_row_offers_to_pick_and_names_the_cli_the_keys_act_on() {
        let theme = Theme::dark();
        let (words, from) = colours(&picture_page(), &theme);
        assert_eq!(
            words,
            "■ Claude  ■ Codex  ■ agy  one per CLI, from the theme · pick…"
        );
        assert_eq!(from, "theme");
        let mut page = on_colours();
        let (words, _) = colours(&page, &theme);
        assert!(
            words.ends_with("Claude is the theme's · space picks another"),
            "{words}"
        );
        press(&mut page, key(KeyCode::Right));
        let (words, _) = colours(&page, &theme);
        assert!(words.ends_with("Codex is the theme's · space picks another"));
        // Each square is its CLI's own color.
        let (spans, _) = page.colour_words(&Kit::new(&theme));
        assert_eq!(spans[0].style, theme.style(cli_role("claude-code")));
        assert_eq!(spans[3].style, theme.style(cli_role("codex")));
        assert_ne!(spans[0].style, spans[3].style);
    }

    #[test]
    fn space_gives_a_cli_the_next_color_and_after_the_last_the_theme_s_again() {
        let mut theme = Theme::dark();
        let claude = own_role("claude-code").unwrap();
        let mut page = on_colours();
        let first = sent(press(&mut page, key(KeyCode::Char(' '))));
        assert_eq!(first, set(claude.name(), "agent-blue"));
        // The screen's theme follows the setting, and the row says so.
        assert!(theme.link(claude, Role::AgentBlue));
        let (words, from) = colours(&page, &theme);
        assert!(words.contains("Claude is blue · space"), "{words}");
        assert_eq!(from, "yours");
        assert_eq!(
            sent(page.handle_key(key(KeyCode::Char(' ')), &theme)),
            set(claude.name(), "agent-purple")
        );
        // Shift+← goes back: from the first color to the theme's own.
        assert_eq!(
            sent(page.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT), &theme)),
            UiCommand::ResetSetting {
                key: format!("ui.colors.{}", claude.name()),
                scope: SettingScope::Global,
            }
        );
        // And from the theme's own back to the last.
        let last = PALETTE[PALETTE.len() - 1].0.role();
        let before = sent(press(&mut page, key(KeyCode::Char('<'))));
        let KeyOf(role, color) = KeyOf::of(&before);
        assert_eq!(role, claude.name());
        // A color that is this role already is passed over, so it isn't always the last.
        assert!(PALETTE.iter().any(|(have, _)| have.role().name() == color));
        if last != claude {
            assert_eq!(color, last.name());
        }
    }

    /// The role and the color of a `SetSetting` the picker sent.
    struct KeyOf(String, String);

    impl KeyOf {
        fn of(command: &UiCommand) -> Self {
            match command {
                UiCommand::SetSetting { key, value, .. } => Self(
                    key.trim_start_matches("ui.colors.").to_owned(),
                    value.trim_matches('"').to_owned(),
                ),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_color_that_would_follow_itself_is_never_offered() {
        let theme = Theme::dark();
        let mut page = on_colours();
        for cli in ["claude-code", "codex", "antigravity-cli"] {
            let role = own_role(cli).unwrap();
            let mut seen = Vec::new();
            let mut theme = theme.clone();
            // Round once through every color the picker offers for this CLI.
            for _ in 0..=PALETTE.len() {
                match sent(page.handle_key(key(KeyCode::Char(' ')), &theme)) {
                    UiCommand::SetSetting { key, value, .. } => {
                        assert_eq!(key, format!("ui.colors.{}", role.name()));
                        let color = Role::from_name(value.trim_matches('"')).unwrap();
                        assert_ne!(color, role);
                        assert!(theme.link(role, color), "{cli}: {value}");
                        seen.push(color);
                    }
                    UiCommand::ResetSetting { .. } => break,
                    other => panic!("{other:?}"),
                }
            }
            let offered = PALETTE.iter().filter(|(c, _)| c.role() != role).count();
            assert_eq!(seen.len(), offered, "{cli}");
            press(&mut page, key(KeyCode::Right));
        }
    }

    #[test]
    fn a_cli_the_theme_has_no_color_for_is_grey_and_says_why() {
        let theme = Theme::dark();
        let numbers = LineNumbers {
            clis: vec![LineCli {
                label: "OpenCode".to_owned(),
                id: "opencode".to_owned(),
                main: true,
                five_hours: Some(0.2),
                seven_days: None,
            }],
            ..LineNumbers::default()
        };
        let mut page = on_colours();
        page.numbers = numbers;
        assert_eq!(cli_role("opencode"), Role::CatOther);
        let (words, from) = colours(&page, &theme);
        assert!(
            words.contains("OpenCode has no colour of its own"),
            "{words}"
        );
        assert_eq!(from, "theme");
        assert!(matches!(
            press(&mut page, key(KeyCode::Char(' '))),
            ViewOutcome::Notice(_)
        ));
    }

    #[test]
    fn your_own_line_mods_are_offered_on_both_sides() {
        let page = page();
        for left in [true, false] {
            assert!(
                page.tick_names(left).contains(&"my-ci".to_owned()),
                "{left}"
            );
        }
        let labels: Vec<String> = page.ticks(false).into_iter().map(|t| t.label).collect();
        assert_eq!(
            labels,
            ["plan usage", "spend", "context", "tokens", "stats", "my ci"]
        );
    }

    #[test]
    fn s_sends_only_what_changed_and_r_goes_back() {
        let mut page = page();
        assert!(matches!(
            press(&mut page, key(KeyCode::Char('s'))),
            ViewOutcome::Notice(_)
        ));
        page.selected = FORM_ROW;
        press(&mut page, key(KeyCode::Right));
        assert_eq!(page.file.form, LineForm::Bars);
        press(&mut page, key(KeyCode::Char('r')));
        assert_eq!(page.file.form, LineForm::Fit);
        press(&mut page, key(KeyCode::Right));
        match press(&mut page, key(KeyCode::Char('s'))) {
            ViewOutcome::Send(UiCommand::Mod {
                change:
                    ModChange::Save {
                        name,
                        pairs,
                        origin,
                    },
            }) => {
                assert_eq!(name, "bottom-line");
                assert_eq!(pairs, [("form".to_owned(), "\"bars\"".to_owned())]);
                assert_eq!(origin, ModOrigin::Yours);
            }
            other => panic!("{other:?}"),
        }
        assert!(!page.changed());
    }

    #[test]
    fn a_second_r_deletes_your_file() {
        let built_in = builtin::builtin(builtin::BOTTOM_LINE).unwrap().text;
        let mut page = LinePage::new(
            "form = \"text\"\n",
            built_in,
            ModOrigin::Yours,
            "~/.argonauts/mods/bottom-line.toml".to_owned(),
            LineNumbers::default(),
            Vec::new(),
        );
        assert_eq!(page.file.form, LineForm::Text);
        assert!(matches!(
            press(&mut page, key(KeyCode::Char('r'))),
            ViewOutcome::Notice(_)
        ));
        assert!(matches!(
            press(&mut page, key(KeyCode::Char('r'))),
            ViewOutcome::Send(UiCommand::Mod {
                change: ModChange::Reset { .. }
            })
        ));
        assert_eq!(page.file, BottomLineSettings::default());
    }

    #[test]
    fn a_file_that_doesnt_read_starts_from_the_built_in_line_and_says_so() {
        let built_in = builtin::builtin(builtin::BOTTOM_LINE).unwrap().text;
        let page = LinePage::new(
            "form = \"wide\"\n",
            built_in,
            ModOrigin::Yours,
            String::new(),
            LineNumbers::default(),
            Vec::new(),
        );
        assert_eq!(page.file, BottomLineSettings::default());
        assert!(
            page.problem
                .as_deref()
                .is_some_and(|why| why.starts_with("form: ")),
            "{:?}",
            page.problem
        );
    }
}
