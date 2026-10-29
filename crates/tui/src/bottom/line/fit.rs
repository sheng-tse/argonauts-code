//! What the line gives up on a narrow terminal, and in what order.
//!
//! The order is fixed, and each step is taken only when the line still doesn't fit:
//!
//! 1. the `(shift+tab to cycle)` hint. The mode has two more hints that go with it:
//!    `? for shortcuts` or `esc to interrupt` goes just before it, so a terminal wider
//!    than 150 columns still shows how to change the mode, and `↓ to manage` goes just after
//! 2. the parts you added, in your own drop order, and then the keys of a message or task you
//!    picked shorten: `↑ ↓  to pick`, `enter to open`, `esc to go back`
//! 3. the main CLI's second window becomes one cell
//! 4. words shorten: `resets 4:40pm` to `4:40pm`, `out until Tue` to `out · Tue`
//! 5. the main CLI's second window goes
//! 6. the mode: `auto mode on` to `auto`
//! 7. `out · Tue 3:45pm` becomes `out`
//! 8. a CLI at 90% that isn't in use: its bar goes, its number and reset time stay
//! 9. a CLI that is fine: its bar becomes one cell
//! 10. a CLI that is fine keeps only its name, or its logo
//! 11. a CLI that is fine becomes its dot and two letters, such as `○ CX`
//! 12. the main CLI's bar goes, its number stays
//! 13. the mode becomes its sign alone
//!
//! The CLIs on the line keep their bars longest, since the line shows only a few: the CLI in
//! use gives up its second window before another CLI's bar shrinks. Never given up: the main
//! CLI's fuller window, the number of any CLI at 90%, and the word of one that is out. Four more
//! steps follow for a terminal narrower than about 40 columns, so that even there no CLI is cut
//! off: the vim mode and the arrow's count shorten, the reset time goes, then every name becomes
//! its two letters, `● CC`, then the chips move one column apart. Only where even that leaves
//! no room do the letters go, a fine CLI's first, so that no CLI is cut off nor any state's word
//! lost. Past all of them the left side is cut, never the chips.
//!
//! A pinned form keeps its look for as long as anything else can go: the hints, your added
//! parts, the mode's words and the longer wordings. Only then do CLIs that are fine shrink to
//! a name and a dot, and the CLI in use to its fuller window's number.

use argonauts_core::settings::bottom_line::LineForm;

use super::BottomLine;

/// One thing the line gives up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::bottom) enum Give {
    FooterHint,
    CycleHint,
    ManageHint,
    /// The added part at this place in the drop order.
    Added(usize),
    ShortWords,
    FineBarToCell,
    ModeToWord,
    SecondToCell,
    OutTime,
    NearBar,
    FineToName,
    Second,
    FineToDot,
    MainBar,
    ModeToSign,
    /// `← 2 agents` to `← 2`, `← 1 needs you` to `← 1!`.
    ShortPieces,
    /// A picked message's or task's keys in fewer words: `↑ ↓  to pick`, `enter to open`,
    /// `esc to go back`.
    ShortKeys,
    ResetTime,
    Names,
    ChipGap,
    /// The two letters of a CLI that is fine, where even that leaves no room.
    FineLetters,
    /// Every CLI's two letters, before any CLI is cut off the line.
    Letters,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(super) enum ModeForm {
    #[default]
    Full,
    Word,
    Sign,
}

/// How much of a CLI that is fine still shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(super) enum Fine {
    #[default]
    Bar,
    Cell,
    Name,
    Dot,
}

/// How much of the main CLI's less full window still shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(super) enum Second {
    #[default]
    Bar,
    Cell,
    Gone,
}

/// The line after some steps of the order: what is left of each thing that can shrink.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Shape {
    given: Vec<Give>,
    dropped: Vec<String>,
    pub short_words: bool,
    pub fine: Fine,
    pub mode: ModeForm,
    pub second: Second,
    pub out_time_gone: bool,
    /// A CLI at 90% that isn't the one in use shows its number without its bar.
    pub near_bar_gone: bool,
    pub main_bar_gone: bool,
    pub reset_gone: bool,
    /// Every name stands as its two letters.
    pub names_gone: bool,
    pub tight: bool,
    /// Past every other step: a CLI that is fine shows its dot alone.
    pub fine_letters_gone: bool,
    /// Last of all: every CLI shows its dot alone, its state's word beside it.
    pub letters_gone: bool,
}

impl Shape {
    /// The line with the first `steps` of `order` given up.
    pub fn after(order: &Order<'_>, steps: usize) -> Self {
        let mut shape = Self::default();
        for give in order.gives.iter().take(steps) {
            shape.give(*give, order);
        }
        shape
    }

    fn give(&mut self, give: Give, order: &Order<'_>) {
        self.given.push(give);
        match give {
            Give::ManageHint
            | Give::FooterHint
            | Give::CycleHint
            | Give::ShortPieces
            | Give::ShortKeys => {}
            Give::Added(at) => self
                .dropped
                .extend(order.added.get(at).map(|name| (*name).to_owned())),
            Give::ShortWords => self.short_words = true,
            Give::FineBarToCell => self.fine = self.fine.max(Fine::Cell),
            Give::ModeToWord => self.mode = self.mode.max(ModeForm::Word),
            Give::SecondToCell => self.second = self.second.max(Second::Cell),
            Give::OutTime => self.out_time_gone = true,
            Give::NearBar => self.near_bar_gone = true,
            Give::FineToName => self.fine = self.fine.max(Fine::Name),
            Give::Second => self.second = Second::Gone,
            Give::FineToDot => self.fine = Fine::Dot,
            Give::MainBar => self.main_bar_gone = true,
            Give::ModeToSign => self.mode = ModeForm::Sign,
            Give::ResetTime => self.reset_gone = true,
            Give::Names => self.names_gone = true,
            Give::ChipGap => self.tight = true,
            Give::FineLetters => self.fine_letters_gone = true,
            Give::Letters => self.letters_gone = true,
        }
    }

    pub fn gave(&self, give: Give) -> bool {
        self.given.contains(&give)
    }

    /// The added part `name` left the line.
    pub fn dropped(&self, name: &str) -> bool {
        self.dropped.iter().any(|dropped| dropped == name)
    }
}

/// The steps for one line, first to go first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Order<'a> {
    gives: Vec<Give>,
    /// The added parts' names, by their place in the drop order.
    added: Vec<&'a str>,
}

impl Order<'_> {
    pub fn len(&self) -> usize {
        self.gives.len()
    }
}

pub(super) fn order(line: &BottomLine) -> Order<'_> {
    let added = line.added_in_drop_order();
    let mut gives = vec![Give::FooterHint, Give::CycleHint, Give::ManageHint];
    gives.extend((0..added.len()).map(Give::Added));
    // Keys picking lasts a moment, so they shorten before anything of the line's own does.
    gives.push(Give::ShortKeys);
    gives.extend(match line.form {
        LineForm::Fit => FIT.as_slice(),
        LineForm::Bars | LineForm::Compact | LineForm::Gauges | LineForm::Text => PINNED.as_slice(),
    });
    gives.push(Give::ShortPieces);
    gives.extend(LAST_RESORT);
    Order { gives, added }
}

/// Steps 3 to 13 of the order.
const FIT: [Give; 11] = [
    Give::SecondToCell,
    Give::ShortWords,
    Give::Second,
    Give::ModeToWord,
    Give::OutTime,
    Give::NearBar,
    Give::FineBarToCell,
    Give::FineToName,
    Give::FineToDot,
    Give::MainBar,
    Give::ModeToSign,
];

/// A pinned form never changes its look while there's any other way.
const PINNED: [Give; 9] = [
    Give::ModeToWord,
    Give::ModeToSign,
    Give::ShortWords,
    Give::OutTime,
    Give::FineToName,
    Give::FineToDot,
    Give::Second,
    Give::NearBar,
    Give::MainBar,
];

const LAST_RESORT: [Give; 5] = [
    Give::ResetTime,
    Give::Names,
    Give::ChipGap,
    Give::FineLetters,
    Give::Letters,
];
