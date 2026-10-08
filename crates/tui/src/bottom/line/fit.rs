//! What the line gives up on a narrow terminal, and in what order.
//!
//! The order is fixed, and each step is taken only when the line still doesn't fit:
//!
//! 1. the `(shift+tab to cycle)` hint. The mode has two more hints that go with it:
//!    `? for shortcuts` or `esc to interrupt` goes just before it, so a terminal wider
//!    than 150 columns still shows how to change the mode, and `↓ to manage` goes just after
//! 2. the parts you added, in your own drop order
//! 3. words shorten: `resets 4:40pm` to `4:40pm`, `out until Tue` to `out · Tue`
//! 4. a CLI that is fine: its bar becomes one cell
//! 5. a CLI that is out: its bar goes, its words stay
//! 6. the mode: `auto mode on` to `auto`
//! 7. the main CLI's second window becomes one cell
//! 8. `out · Tue 3:45pm` becomes `out`
//! 9. a CLI that is fine keeps only its name
//! 10. the main CLI's second window goes
//! 11. a CLI that is fine becomes a dot in its color
//! 12. the main CLI's bar goes, its number stays
//! 13. the mode becomes its sign alone
//!
//! Never given up: the main CLI's fuller window, and any CLI at 90% or out. Four more steps
//! follow for a terminal narrower than about 40 columns, so that even there no CLI is cut off:
//! the vim mode and the arrow's count shorten, the reset time goes, then the names, then the
//! chips move one column apart. Past all of them the left side is cut, never the chips.
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
    OutBar,
    ModeToWord,
    SecondToCell,
    OutTime,
    FineToName,
    Second,
    FineToDot,
    MainBar,
    ModeToSign,
    /// `-- INSERT --` to `-- I --`, `← 2 agents` to `← 2`, `← 1 needs you` to `← 1!`.
    ShortPieces,
    ResetTime,
    Names,
    ChipGap,
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
    pub out_bar_gone: bool,
    pub mode: ModeForm,
    pub second: Second,
    pub out_time_gone: bool,
    pub main_bar_gone: bool,
    pub reset_gone: bool,
    pub names_gone: bool,
    pub tight: bool,
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
            Give::ManageHint | Give::FooterHint | Give::CycleHint | Give::ShortPieces => {}
            Give::Added(at) => self
                .dropped
                .extend(order.added.get(at).map(|name| (*name).to_owned())),
            Give::ShortWords => self.short_words = true,
            Give::FineBarToCell => self.fine = self.fine.max(Fine::Cell),
            Give::OutBar => self.out_bar_gone = true,
            Give::ModeToWord => self.mode = self.mode.max(ModeForm::Word),
            Give::SecondToCell => self.second = self.second.max(Second::Cell),
            Give::OutTime => self.out_time_gone = true,
            Give::FineToName => self.fine = self.fine.max(Fine::Name),
            Give::Second => self.second = Second::Gone,
            Give::FineToDot => self.fine = Fine::Dot,
            Give::MainBar => self.main_bar_gone = true,
            Give::ModeToSign => self.mode = ModeForm::Sign,
            Give::ResetTime => self.reset_gone = true,
            Give::Names => self.names_gone = true,
            Give::ChipGap => self.tight = true,
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
    Give::ShortWords,
    Give::FineBarToCell,
    Give::OutBar,
    Give::ModeToWord,
    Give::SecondToCell,
    Give::OutTime,
    Give::FineToName,
    Give::Second,
    Give::FineToDot,
    Give::MainBar,
    Give::ModeToSign,
];

/// A pinned form never changes its look while there's any other way.
const PINNED: [Give; 9] = [
    Give::ModeToWord,
    Give::ModeToSign,
    Give::ShortWords,
    Give::OutBar,
    Give::OutTime,
    Give::FineToName,
    Give::FineToDot,
    Give::Second,
    Give::MainBar,
];

const LAST_RESORT: [Give; 3] = [Give::ResetTime, Give::Names, Give::ChipGap];
