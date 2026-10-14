//! Mods on screen: the one drawer that turns a mod's file and numbers into rows from the kit,
//! `/mods` with every mod and its live preview, a mod's settings page, and the bottom line's
//! page.

pub mod bottom_line;
pub mod draw;
pub mod keys;
pub mod list;
pub mod page;
pub mod rows;

pub use list::ModsScreen;

use argonauts_core::input::SlashCommand;
use argonauts_core::settings::mods::view::{ModRowKind, ModsView};
use argonauts_core::settings::mods::{ModFile, ModForm, ModOrigin, ModPlace, ModReading, builtin};
use ratatui::text::Span;

use crate::kit::{Drawn, Kit, stack_cards};
use crate::text::sanitize_line;

/// The mods this drawer draws in `place`, in the order `/mods` lists them: those that show
/// there and have numbers. A mod without numbers draws nothing where it shows; `/mods` says
/// why it has none. A built-in card isn't among them where a place list or the bottom line
/// records it, since the band above the input, the usage panel's tabs and the line draw those
/// themselves, and it would show twice.
pub fn placed(view: &ModsView, place: ModPlace) -> impl Iterator<Item = (&ModFile, &ModReading)> {
    let drawn_elsewhere = place.list().is_some() || place == ModPlace::BottomLine;
    view.rows
        .iter()
        .filter(move |row| !(drawn_elsewhere && builtin::closed_card(&row.name).is_some()))
        .filter_map(move |row| match &row.kind {
            ModRowKind::Card {
                file,
                reading: Some(reading),
                ..
            } if file.shows(place) => Some((file.as_ref(), reading)),
            _ => None,
        })
}

/// What the band above the input has for your mods, under the cards it draws itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandRoom {
    /// The screen's width in cells.
    pub screen: usize,
    /// The rows left for them.
    pub rows: usize,
    /// Every mod as its one row, as while a question has the keys.
    pub folded: bool,
}

/// Every mod of yours pinned above the input, one under another, each in the form its file
/// names. With fewer rows than that takes they step down the way the band's own cards do:
/// the last mod first, from full to compact to its one row. A mod with no row even then is
/// left out, the last first.
pub fn above_input(kit: &Kit<'_>, view: &ModsView, room: BandRoom) -> Drawn {
    let mut mods: Vec<_> = placed(view, ModPlace::Above).collect();
    let mut forms: Vec<ModForm> = mods
        .iter()
        .map(|(file, _)| {
            if room.folded {
                ModForm::Line
            } else {
                file.form
            }
        })
        .collect();
    loop {
        let mut drawn = Drawn::new();
        for ((file, reading), form) in mods.iter().zip(&forms) {
            drawn.append(draw::above_as(kit, file, reading, *form, room.screen));
        }
        if drawn.height() <= room.rows {
            return drawn;
        }
        match forms.iter().rposition(|form| *form != ModForm::Line) {
            Some(last_open) if forms[last_open] == ModForm::Full => {
                forms[last_open] = ModForm::Compact;
            }
            Some(last_open) => forms[last_open] = ModForm::Line,
            // One row each, and still more mods than rows.
            None => {
                mods.truncate(room.rows);
                forms.truncate(room.rows);
            }
        }
    }
}

/// Every mod of yours that shows on the usage panel's tab `place`, as cards one under another.
/// The tab puts them under the cards it draws itself.
pub fn on_tab(kit: &Kit<'_>, view: &ModsView, place: ModPlace, screen: usize) -> Drawn {
    stack_cards(placed(view, place).map(|(file, reading)| draw::on_tab(kit, file, reading, screen)))
}

/// A typed `/name` that is no command but one of your mods: the command that shows or hides
/// it. `None` for anything else, the built-in cards included, whose names are tabs' commands.
pub fn own_command(typed: &str, view: Option<&ModsView>) -> Option<SlashCommand> {
    let name = typed.trim().strip_prefix('/')?;
    let row = view?.row(name)?;
    let yours = matches!(row.kind, ModRowKind::Card { .. }) && row.origin != ModOrigin::BuiltIn;
    yours.then(|| SlashCommand::ModFile {
        name: name.to_owned(),
        words: vec!["toggle".to_owned()],
    })
}

/// The names of your own mods, each of which is a command that shows or hides it.
pub fn own_command_names(view: Option<&ModsView>) -> Vec<String> {
    let rows = view.map(|view| view.rows.as_slice()).unwrap_or_default();
    rows.iter()
        .filter(|row| {
            matches!(row.kind, ModRowKind::Card { .. }) && row.origin != ModOrigin::BuiltIn
        })
        .map(|row| row.name.clone())
        .collect()
}

/// Every mod the bottom line's file lists that has numbers, by name, as the few words it
/// draws there. The line puts each where its file says, and draws its own parts itself.
pub fn line_parts(
    kit: &Kit<'_>,
    view: &ModsView,
) -> impl Iterator<Item = (String, Vec<Span<'static>>)> {
    placed(view, ModPlace::BottomLine)
        .map(|(file, reading)| (file.name.clone(), draw::chip(kit, file, reading)))
}

/// The mod `name` as a part of the bottom line, when the line lists it and it has numbers.
pub fn line_part(kit: &Kit<'_>, view: &ModsView, name: &str) -> Option<Vec<Span<'static>>> {
    line_parts(kit, view)
        .find(|(part, _)| part == name)
        .map(|(_, words)| words)
}

/// `/mods` for a screen reader: one line a mod with what it draws, where, and its numbers in
/// words, then the files that didn't load.
pub fn spoken(view: &ModsView) -> Vec<String> {
    let one = |text: &str| sanitize_line(text).trim().to_owned();
    let mut lines = vec![format!("mods: {}", view.rows.len())];
    for row in &view.rows {
        let line = match &row.kind {
            ModRowKind::Card {
                file,
                reading,
                note,
                asks_for_line,
                hidden_by_line,
                ..
            } => {
                let places: Vec<&str> = file.places.iter().map(|place| place.phrase()).collect();
                let mut shows = if !file.shown {
                    "hidden".to_owned()
                } else if places.is_empty() {
                    "shown nowhere yet".to_owned()
                } else {
                    places.join(", ")
                };
                if *asks_for_line {
                    shows.push_str(" (its file asks for the bottom line)");
                }
                if *hidden_by_line {
                    shows.push_str(&format!(
                        " (the bottom line draws its own {} part instead; rename the file)",
                        row.name
                    ));
                }
                let mut line = format!(
                    "{}: {}, {shows}, {}",
                    one(&file.title),
                    file.body.describe(),
                    row.origin.word()
                );
                match reading {
                    Some(reading) => {
                        line.push_str(&format!("; {}", one(&reading.summary)));
                        if let Some(badge) = reading.badge_words() {
                            line.push_str(&format!(", {}", one(&badge)));
                        }
                        for part in &reading.parts {
                            line.push_str(&format!(
                                "; {} {}",
                                one(&part.label),
                                one(&part.amount())
                            ));
                        }
                    }
                    None => {
                        let why = note.as_deref().unwrap_or("no numbers yet");
                        line.push_str(&format!("; {}", one(why)));
                    }
                }
                line
            }
            ModRowKind::Own { about, .. } => {
                format!("{}: {}, {}", row.name, one(about), row.origin.word())
            }
        };
        lines.push(line);
    }
    for problem in &view.problems {
        lines.push(format!(
            "didn't load: {}: {}",
            one(&problem.path),
            one(&problem.problems.join("; "))
        ));
    }
    lines.push("mods: /mods and a name opens that mod's page.".to_owned());
    lines
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::mods::view::ModRow;
    use argonauts_core::settings::mods::{ModBody, check};

    use super::*;
    use crate::theme::{ColorLevel, Theme};

    fn starter(name: &str, reading: Option<ModReading>) -> ModRow {
        let (name, text) = builtin::STARTERS
            .iter()
            .find(|(starter, _)| *starter == name)
            .unwrap();
        ModRow {
            name: (*name).to_owned(),
            origin: ModOrigin::Yours,
            path: format!("~/.argonauts/mods/{name}.toml"),
            kind: ModRowKind::Card {
                file: Box::new(check::read(name, text).unwrap()),
                built_in: None,
                reading,
                note: None,
                needs_trust: false,
                asks_for_line: false,
                hidden_by_line: false,
            },
        }
    }

    fn view() -> ModsView {
        ModsView {
            rows: vec![
                starter("checks", Some(ModReading::sample(ModBody::Stack))),
                starter("branch", Some(ModReading::from_output("main").unwrap())),
                starter("context", None),
            ],
            ..ModsView::default()
        }
    }

    #[test]
    fn a_mod_shows_where_its_file_says_once_it_has_numbers() {
        let view = view();
        let names = |place| -> Vec<&str> {
            placed(&view, place)
                .map(|(file, _)| file.name.as_str())
                .collect()
        };
        assert_eq!(names(ModPlace::Above), ["checks"]);
        assert_eq!(names(ModPlace::BottomLine), ["branch"]);
        // The context card has a place and no numbers yet, so it draws nothing there.
        assert!(names(ModPlace::Context).is_empty());
    }

    #[test]
    fn a_built_in_card_is_left_to_whoever_draws_it_from_the_place_lists() {
        let mut view = view();
        let reading = ModReading::sample(ModBody::Stack);
        view.rows[2] = starter("context", Some(reading));
        if let ModRowKind::Card { file, .. } = &mut view.rows[2].kind {
            file.places = vec![
                ModPlace::Above,
                ModPlace::BottomLine,
                ModPlace::Overview,
                ModPlace::Context,
            ];
        }
        let names = |place| -> Vec<&str> {
            placed(&view, place)
                .map(|(file, _)| file.name.as_str())
                .collect()
        };
        // The band, the line and the Context tab draw the context card themselves.
        assert_eq!(names(ModPlace::Above), ["checks"]);
        assert_eq!(names(ModPlace::BottomLine), ["branch"]);
        assert!(names(ModPlace::Context).is_empty());
        // No list keeps the Overview tab, so there the mod's own file is all there is.
        assert_eq!(names(ModPlace::Overview), ["context"]);
    }

    #[test]
    fn pinned_mods_draw_above_the_input_and_a_line_mod_on_the_bottom_line() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let view = view();
        let band = above_input(&kit, &view, room(40));
        assert_eq!(band.height(), 5);
        let part = line_part(&kit, &view, "branch").unwrap();
        let words: String = part.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(words, "main");
        assert!(line_part(&kit, &view, "checks").is_none());
        // Every mod on the line by name, as the line's slots take them.
        let names: Vec<String> = line_parts(&kit, &view).map(|(name, _)| name).collect();
        assert_eq!(names, ["branch"]);
        assert!(
            above_input(&kit, &ModsView::default(), room(40))
                .lines
                .is_empty()
        );
    }

    /// A band 80 cells wide with `rows` rows for your mods.
    fn room(rows: usize) -> BandRoom {
        BandRoom {
            screen: 80,
            rows,
            folded: false,
        }
    }

    /// Two mods above the input, `checks` then `build`, both full cards.
    fn two_above() -> ModsView {
        let mut build = starter("checks", Some(ModReading::sample(ModBody::Stack)));
        build.name = "build".to_owned();
        if let ModRowKind::Card { file, .. } = &mut build.kind {
            file.name = "build".to_owned();
            file.title = "build".to_owned();
        }
        ModsView {
            rows: vec![
                starter("checks", Some(ModReading::sample(ModBody::Stack))),
                build,
            ],
            ..ModsView::default()
        }
    }

    fn rows_of(drawn: &Drawn) -> Vec<String> {
        drawn
            .lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn mods_above_the_input_step_down_from_the_last_as_the_rows_run_short() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let view = two_above();
        let (file, reading) = placed(&view, ModPlace::Above).next().unwrap();
        let tall = |form| draw::above_as(&kit, file, reading, form, 80).height();
        let (full, compact, line) = (
            tall(ModForm::Full),
            tall(ModForm::Compact),
            tall(ModForm::Line),
        );
        assert!(full > compact && compact > line && line == 1);

        let height = |rows| above_input(&kit, &view, room(rows)).height();
        assert_eq!(height(2 * full), 2 * full);
        // The last mod gives way first: compact, then its one row.
        assert_eq!(height(2 * full - 1), full + compact);
        assert_eq!(height(full + compact - 1), full + line);
        // Then the first does the same.
        assert_eq!(height(full + line - 1), compact + line);
        assert_eq!(height(compact + line - 1), 2 * line);
        // With one row the first mod keeps it, and with none nothing shows.
        let one = rows_of(&above_input(&kit, &view, room(1)));
        assert_eq!(one.len(), 1);
        assert!(one[0].contains("checks"), "{one:?}");
        assert_eq!(height(0), 0);
    }

    #[test]
    fn while_a_question_has_the_keys_each_mod_above_is_its_one_row() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let folded = BandRoom {
            folded: true,
            ..room(40)
        };
        let rows = rows_of(&above_input(&kit, &two_above(), folded));
        assert_eq!(rows.len(), 2);
        assert!(rows[0].contains("checks") && rows[0].contains("7 of 9 passed"));
        assert!(rows[1].contains("build"), "{rows:?}");
    }

    #[test]
    fn a_mod_of_yours_is_a_card_on_the_overview_tab_its_file_names() {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let kit = Kit::new(&theme);
        let mut view = two_above();
        if let ModRowKind::Card { file, .. } = &mut view.rows[1].kind {
            file.places = vec![ModPlace::Usage];
        }
        let rows = rows_of(&on_tab(&kit, &view, ModPlace::Usage, 80));
        assert!(rows.iter().any(|row| row.contains("build")), "{rows:?}");
        assert!(!rows.iter().any(|row| row.contains("checks")), "{rows:?}");
        assert!(on_tab(&kit, &view, ModPlace::Status, 80).lines.is_empty());
    }

    #[test]
    fn a_mod_of_yours_is_a_command_by_its_name() {
        let view = view();
        assert_eq!(
            own_command("/checks", Some(&view)),
            Some(SlashCommand::ModFile {
                name: "checks".to_owned(),
                words: vec!["toggle".to_owned()],
            })
        );
        assert_eq!(own_command("/checks now", Some(&view)), None);
        assert_eq!(own_command("/nothing", Some(&view)), None);
        assert_eq!(own_command("/checks", None), None);
        assert_eq!(own_command("checks", Some(&view)), None);
    }

    #[test]
    fn a_hidden_mod_shows_nowhere() {
        let mut view = view();
        if let ModRowKind::Card { file, .. } = &mut view.rows[0].kind {
            file.shown = false;
        }
        assert_eq!(placed(&view, ModPlace::Above).count(), 0);
    }

    #[test]
    fn a_screen_reader_hears_each_mod_and_each_file_that_didnt_load() {
        let mut view = view();
        view.problems
            .push(argonauts_core::settings::mods::view::FileProblem {
                path: "~/.argonauts/mods/broken.toml".to_owned(),
                problems: vec!["it needs a body".to_owned()],
            });
        let lines = spoken(&view);
        assert_eq!(lines[0], "mods: 3");
        assert_eq!(
            lines[1],
            "checks: a stacked bar with its legend, above the input, yours; 7 of 9 passed, \
             1 failed; passed 7; running 1; failed 1"
        );
        assert!(lines[3].ends_with("; no numbers yet"), "{}", lines[3]);
        if let ModRowKind::Card { asks_for_line, .. } = &mut view.rows[2].kind {
            *asks_for_line = true;
        }
        let asking = &spoken(&view)[3];
        assert!(
            asking.contains("on the Context tab (its file asks for the bottom line), yours"),
            "{asking}"
        );
        assert_eq!(
            lines[4],
            "didn't load: ~/.argonauts/mods/broken.toml: it needs a body"
        );
    }
}
