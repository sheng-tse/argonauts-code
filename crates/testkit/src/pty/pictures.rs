//! Pictures sent with the kitty graphics protocol, kept the way Ghostty keeps them: one store for
//! the main screen and one for the alternate screen. Clearing a screen whole (`CSI 2 J`) empties
//! its store, and so does `CSI ? 1049 h` for the alternate one each time, since it clears that
//! screen on the way in. kitty keeps a store for each screen too and empties the alternate one
//! when it enters it. A cell that prints a placeholder, U+10EEEE with a picture's id in its
//! foreground colour, shows that picture only while the screen it is on holds it with a virtual
//! placement (`a=p,U=1`). So a picture sent while the other screen showed leaves its
//! placeholders blank.
//!
//! vt100 drops APC strings, so a scanner reads the commands out of the bytes beside it. Only
//! each command's keys are read; no payload is decoded.

use std::collections::HashMap;

use crate::screen::Grid;

/// The most bytes of keys kept for one command. Real ones take a few dozen.
const MOST_KEY_BYTES: usize = 256;

const ESC: u8 = 0x1b;

/// The pictures a terminal that takes them keeps, and the commands that found none.
#[derive(Default)]
pub(super) struct Pictures {
    /// A terminal that takes no pictures keeps none, whatever it is sent.
    takes: bool,
    scanner: Scanner,
    /// The command whose last byte the scanner just read, for the emulator to run once vt100
    /// has caught up and says which screen is current.
    ended: Option<Command>,
    stores: Stores,
    /// Placements sent for a picture their screen didn't hold, once per picture and screen.
    misplaced: Vec<Misplaced>,
}

/// What each screen holds.
#[derive(Debug, Clone, Default)]
pub(super) struct Stores {
    main: Store,
    alternate: Store,
}

#[derive(Debug, Clone, Default)]
struct Store {
    pictures: HashMap<u32, Picture>,
    /// A transmission whose last chunk hasn't come yet.
    loading: Option<Command>,
}

#[derive(Debug, Clone, Default)]
struct Picture {
    /// Each placement's id, 0 for one placed without an id, and whether it is virtual, the kind
    /// placeholders show.
    placements: Vec<(u32, bool)>,
}

/// How far a screen's store goes to show a picture through placeholders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holding {
    /// The store is empty.
    Nothing,
    /// The store holds other pictures.
    Others,
    /// The store holds it, with no virtual placement.
    Unplaced,
    /// The store holds it with a virtual placement, so its placeholders show.
    Shown,
}

/// The placeholders of one picture that don't show.
struct Blank {
    id: u32,
    /// The first one's row and column.
    first: (u16, u16),
    cells: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Misplaced {
    id: u32,
    alternate: bool,
    /// Whether the screen held any picture then.
    held_any: bool,
    times: usize,
}

impl Pictures {
    pub(super) fn taking() -> Self {
        Self {
            takes: true,
            ..Self::default()
        }
    }

    pub(super) fn stores(&self) -> &Stores {
        &self.stores
    }

    /// Reads one byte of output, saying whether it ended a graphics command.
    pub(super) fn scan(&mut self, byte: u8) -> bool {
        if !self.takes {
            return false;
        }
        self.ended = self.scanner.scan(byte);
        self.ended.is_some()
    }

    /// Runs the command the last byte ended, on the main or the alternate screen.
    pub(super) fn run(&mut self, alternate: bool) {
        let Some(command) = self.ended.take() else {
            return;
        };
        let store = self.stores.on_mut(alternate);
        let held_any = !store.pictures.is_empty();
        let Err(id) = store.run(command) else {
            return;
        };
        let same = |miss: &&mut Misplaced| miss.id == id && miss.alternate == alternate;
        match self.misplaced.iter_mut().find(same) {
            Some(miss) => miss.times += 1,
            None => self.misplaced.push(Misplaced {
                id,
                alternate,
                held_any,
                times: 1,
            }),
        }
    }

    /// Empties the main or the alternate screen's store, as clearing that screen whole does.
    pub(super) fn empty(&mut self, alternate: bool) {
        *self.stores.on_mut(alternate) = Store::default();
    }

    /// Whether a placeholder for picture `id` on the main or the alternate screen shows it, the
    /// screen drawing from `stores`.
    pub(super) fn shows(&self, stores: &Stores, alternate: bool, id: u32) -> bool {
        self.takes && stores.on(alternate).holding(id) == Holding::Shown
    }

    /// What keeps a picture the program meant to show from showing: each picture that
    /// placeholders in `cells` name and their screen doesn't show, and each placement sent for
    /// a picture its screen didn't hold.
    pub(super) fn problems(&self, cells: &Grid, stores: &Stores) -> Vec<String> {
        let alternate = cells.alternate();
        let screen = screen_name(alternate);
        let placeholders = blanks(cells).into_iter().map(|blank| {
            let why = match stores.on(alternate).holding(blank.id) {
                _ if !self.takes => "in a terminal that takes no pictures",
                Holding::Nothing => "which holds none",
                Holding::Unplaced => "which holds it with no placement for placeholders",
                Holding::Others | Holding::Shown => "which doesn't hold it",
            };
            let Blank { id, first, cells } = blank;
            let noun = if cells == 1 { "cell" } else { "cells" };
            format!(
                "placeholder for picture {id} on the {screen}, {why} \
                 ({cells} {noun}, the first at row {}, column {})",
                first.0, first.1
            )
        });
        let placements = self.misplaced.iter().map(|miss| {
            let placements = if miss.times == 1 {
                "a placement".to_owned()
            } else {
                format!("{} placements", miss.times)
            };
            let why = if miss.held_any {
                "which didn't hold it"
            } else {
                "which held none"
            };
            format!(
                "{placements} of picture {} on the {}, {why}",
                miss.id,
                screen_name(miss.alternate)
            )
        });
        placeholders.chain(placements).collect()
    }

    /// Whether a placement was ever sent for a picture its screen didn't hold, which no later
    /// output can make right.
    pub(super) fn misplaced_any(&self) -> bool {
        !self.misplaced.is_empty()
    }
}

/// The placeholders in `cells` that don't show, by picture, in the order they first appear.
fn blanks(cells: &Grid) -> Vec<Blank> {
    let mut blanks: Vec<Blank> = Vec::new();
    for (row, line) in (0..).zip(cells.cell_rows()) {
        for (col, cell) in (0..).zip(line) {
            let Some(picture) = cell.picture.filter(|picture| !picture.shows) else {
                continue;
            };
            match blanks.iter_mut().find(|blank| blank.id == picture.id) {
                Some(blank) => blank.cells += 1,
                None => blanks.push(Blank {
                    id: picture.id,
                    first: (row, col),
                    cells: 1,
                }),
            }
        }
    }
    blanks
}

fn screen_name(alternate: bool) -> &'static str {
    if alternate {
        "alternate screen"
    } else {
        "main screen"
    }
}

impl Stores {
    fn on(&self, alternate: bool) -> &Store {
        if alternate {
            &self.alternate
        } else {
            &self.main
        }
    }

    fn on_mut(&mut self, alternate: bool) -> &mut Store {
        if alternate {
            &mut self.alternate
        } else {
            &mut self.main
        }
    }
}

impl Store {
    /// Runs `command`, failing with the picture's id when it places one this store lacks.
    fn run(&mut self, command: Command) -> Result<(), u32> {
        if command.continues {
            if !command.more
                && let Some(first) = self.loading.take()
            {
                self.hold(&first);
            }
            return Ok(());
        }
        match command.action {
            b't' | b'T' if command.more => self.loading = Some(command),
            b't' | b'T' => {
                self.loading = None;
                self.hold(&command);
            }
            b'p' => match self.pictures.get_mut(&command.id) {
                Some(picture) => picture.place(command.placement, command.virtual_placement),
                None => return Err(command.id),
            },
            b'd' => self.delete(&command),
            _ => {}
        }
        Ok(())
    }

    /// Keeps a whole transmission. A picture sent again under an id keeps its placements.
    fn hold(&mut self, command: &Command) {
        if command.id == 0 {
            return;
        }
        let picture = self.pictures.entry(command.id).or_default();
        if command.action == b'T' {
            picture.place(command.placement, command.virtual_placement);
        }
    }

    /// `a=d`: by id or range of ids, or all. Lowercase takes placements off and keeps the
    /// pictures; uppercase frees the pictures too. The other kinds pick placements by where they
    /// sit, which a virtual placement never does, so placeholders don't notice them.
    fn delete(&mut self, command: &Command) {
        let free = command.delete.is_ascii_uppercase();
        match command.delete.to_ascii_lowercase() {
            b'a' if free => self.pictures.clear(),
            b'a' => {
                for picture in self.pictures.values_mut() {
                    picture.placements.clear();
                }
            }
            b'i' => self.take_off(command.id, command.placement, free),
            b'r' => {
                let (first, last) = command.range;
                let ids: Vec<u32> = self
                    .pictures
                    .keys()
                    .copied()
                    .filter(|id| (first..=last).contains(id))
                    .collect();
                for id in ids {
                    self.take_off(id, 0, free);
                }
            }
            _ => {}
        }
    }

    /// Takes picture `id`'s placement `placement` off, or every one for 0, and with `free` the
    /// picture too once nothing places it.
    fn take_off(&mut self, id: u32, placement: u32, free: bool) {
        let Some(picture) = self.pictures.get_mut(&id) else {
            return;
        };
        if placement == 0 {
            picture.placements.clear();
        } else {
            picture.placements.retain(|(kept, _)| *kept != placement);
        }
        if free && picture.placements.is_empty() {
            self.pictures.remove(&id);
        }
    }

    fn holding(&self, id: u32) -> Holding {
        match self.pictures.get(&id) {
            Some(picture) if picture.placements.iter().any(|(_, virtual_)| *virtual_) => {
                Holding::Shown
            }
            Some(_) => Holding::Unplaced,
            None if self.pictures.is_empty() => Holding::Nothing,
            None => Holding::Others,
        }
    }
}

impl Picture {
    /// A placement with an id replaces the one under it; without one, it is one more.
    fn place(&mut self, placement: u32, virtual_: bool) {
        if placement != 0 {
            self.placements.retain(|(kept, _)| *kept != placement);
        }
        self.placements.push((placement, virtual_));
    }
}

/// One graphics command's keys, as far as which pictures a screen holds goes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Command {
    /// `a`: `t` transmits, `T` transmits and places, `p` places, `d` deletes, `q` asks.
    action: u8,
    /// `i`.
    id: u32,
    /// `p`.
    placement: u32,
    /// `U=1`: a virtual placement, which shows through placeholder cells only.
    virtual_placement: bool,
    /// `m=1`: more chunks of this transmission follow.
    more: bool,
    /// `d`: what a deletion takes.
    delete: u8,
    /// `x` and `y`: the ids `d=r` takes.
    range: (u32, u32),
    /// Only `m` and `q` keys: a chunk after a transmission's first.
    continues: bool,
}

impl Command {
    fn parse(keys: &[u8]) -> Self {
        let mut command = Self {
            action: b't',
            delete: b'a',
            ..Self::default()
        };
        let text = String::from_utf8_lossy(keys);
        let mut only_chunk_keys = !text.is_empty();
        for pair in text.split(',') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            let number = || value.parse::<u32>().unwrap_or_default();
            let letter = value.bytes().next().unwrap_or_default();
            match key {
                "a" => command.action = letter,
                "i" => command.id = number(),
                "p" => command.placement = number(),
                "U" => command.virtual_placement = number() == 1,
                "m" => command.more = number() == 1,
                "d" => command.delete = letter,
                "x" => command.range.0 = number(),
                "y" => command.range.1 = number(),
                _ => {}
            }
            only_chunk_keys &= matches!(key, "m" | "q");
        }
        command.continues = only_chunk_keys;
        command
    }
}

/// Reads graphics commands, `ESC _ G keys ; payload ESC \`, out of the output a byte at a time.
/// An APC string ends where vte ends it, at any ESC, or at CAN or SUB, so this and vt100 agree
/// on where each one stops.
#[derive(Default)]
struct Scanner {
    state: State,
    keys: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Escape,
    /// Just inside `ESC _`, where `G` makes it a graphics command.
    Apc,
    Keys,
    /// Past a graphics command's keys, or inside an APC string of another kind.
    Rest {
        graphics: bool,
    },
    /// After an ESC inside an APC string, where `\` ends it.
    End {
        graphics: bool,
    },
}

impl Scanner {
    /// Reads `byte`, giving back the graphics command it ends.
    fn scan(&mut self, byte: u8) -> Option<Command> {
        if let State::End { graphics } = self.state {
            if byte == b'\\' {
                self.state = State::Ground;
                return graphics.then(|| Command::parse(&self.keys));
            }
            // The ESC began something else.
            self.state = State::Escape;
        }
        self.state = match (self.state, byte) {
            (_, 0x18 | 0x1a) => State::Ground,
            (State::Apc, ESC) => State::End { graphics: false },
            (State::Keys, ESC) => State::End { graphics: true },
            (State::Rest { graphics }, ESC) => State::End { graphics },
            (_, ESC) => State::Escape,
            (State::Escape, b'_') => {
                self.keys.clear();
                State::Apc
            }
            // vte runs C0 controls inside an escape and stays in it.
            (State::Escape, 0x00..=0x1f | 0x7f) => State::Escape,
            (State::Escape | State::Ground, _) => State::Ground,
            (State::Apc, b'G') => State::Keys,
            (State::Apc, _) => State::Rest { graphics: false },
            (State::Keys, b';') => State::Rest { graphics: true },
            (State::Keys, _) => {
                if self.keys.len() < MOST_KEY_BYTES {
                    self.keys.push(byte);
                }
                State::Keys
            }
            (state @ (State::Rest { .. } | State::End { .. }), _) => state,
        };
        None
    }
}

#[cfg(test)]
#[path = "pictures_tests.rs"]
mod tests;
