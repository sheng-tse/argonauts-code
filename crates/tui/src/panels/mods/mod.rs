//! Your mods where the panels draw them: above the input and on the usage panel's tabs, and
//! the one drawer that turns a mod's file and numbers into rows from the kit.

pub mod draw;
pub mod rows;

use argonauts_core::settings::mods::view::{ModRowKind, ModsView};
use argonauts_core::settings::mods::{ModFile, ModForm, ModPlace, ModReading, builtin};

use crate::base::kit::{Drawn, Kit, stack_cards};

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
