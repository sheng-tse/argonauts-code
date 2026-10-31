//! The words of the sub-tab's key line: the keys every line starts with, then what each
//! level and each place in the list answers to, in short words where the long ones don't fit.

use argonauts_core::behaviors::behavior_map::{Placing, Scope};
use crossterm::event::KeyCode;
use unicode_width::UnicodeWidthStr;

use super::level::Level;
use super::{At, BehaviorsTab, Stop};
use crate::panels::stats::deep_flow::Shown;

impl BehaviorsTab {
    /// `←  →`, or `h  l` with vim keys. Each arrow is followed by two spaces, since many fonts
    /// draw it two cells wide.
    pub(super) fn arrows(&self) -> &'static str {
        if self.moment.vim { "h  l" } else { "←  →" }
    }

    pub(super) fn down(&self) -> &'static str {
        if self.moment.vim { "j" } else { "↓" }
    }

    /// The keys every key line of the sub-tab starts with, on its rows and on the tabs alike:
    /// the period, the projects and what `d` switches to now, then `u` while Summaries wait
    /// to add actions, or `x` while Summaries are made.
    pub fn own_keys(&self) -> Vec<String> {
        if self.deep.asking() {
            return Vec::new();
        }
        let mut keys = vec![
            crate::panels::view::periods::CYCLE_HINT.to_owned(),
            self.projects_key().to_owned(),
        ];
        if self.deep.running() {
            keys.push("x stops it".to_owned());
        } else if self.claims(KeyCode::Char('d'), true)
            && let Some(shown) = self.shown()
        {
            keys.push(d_key(shown).to_owned());
            if self.claims(KeyCode::Char('u'), true) {
                keys.push("u update".to_owned());
            }
        }
        keys
    }

    pub(super) fn projects_key(&self) -> &'static str {
        match self.request.scope {
            Scope::Everywhere => "s this project",
            Scope::ThisProject => "s all projects",
        }
    }

    /// The keys, in display order, in their short words where the long ones don't fit in
    /// `width` cells, with a cell to spare.
    pub fn keys(&self, width: usize) -> Vec<String> {
        let long = self.keys_in(false);
        let fits = long.join(" · ").width() < width;
        if fits { long } else { self.keys_in(true) }
    }

    fn keys_in(&self, short: bool) -> Vec<String> {
        // Summaries' questions take the keys while they show.
        if self.deep.asking() {
            return ["↑  ↓  choose", "enter answer", "esc back"]
                .map(str::to_owned)
                .to_vec();
        }
        let (lr, down) = (self.arrows(), self.down());
        let owned = |keys: &[&str]| keys.iter().map(|k| (*k).to_owned()).collect::<Vec<_>>();
        let cycle = crate::panels::view::periods::CYCLE_HINT;
        if self.stop == Stop::Groupings {
            let mut keys = vec![cycle.to_owned(), format!("tab · {lr}  groupings")];
            keys.extend(self.own_keys().into_iter().skip(1));
            keys.push(format!("{down}  into the map"));
            keys.push("esc closes".to_owned());
            return keys;
        }
        match self.level {
            Level::Universe => {
                return owned(&[
                    cycle,
                    &format!("{lr}  pick a galaxy"),
                    "enter go in",
                    "scroll in or out",
                    "esc back to the map",
                ]);
            }
            Level::Galaxy(_) => {
                return owned(&[
                    cycle,
                    &format!("{lr}  pick a star"),
                    "enter its system",
                    "scroll in or out",
                    "esc out to the universe",
                ]);
            }
            Level::System(_) => {
                return owned(&[
                    cycle,
                    &format!("{lr}  pick a body"),
                    "scroll out",
                    "esc out to the galaxy",
                ]);
            }
            Level::Map | Level::Planet => {}
        }
        let word = self.request.grouping.word();
        let on_summaries = matches!(
            self.map.as_ref().map(|map| &map.placing),
            Some(Placing::Summaries { .. })
        );
        let pick = |long: &str, brief: &str| if short { brief } else { long }.to_owned();
        let listed = self.listed_now();
        let world = self.draws_world();
        let mut keys: Vec<String> = match self.at {
            At::Map if !self.has_actions() => vec!["esc back".to_owned()],
            At::Map if self.level == Level::Planet && world => owned(&[
                "tab groupings",
                &format!("{lr}  compare"),
                "drag to turn",
                if self.night {
                    "n day side"
                } else {
                    "n night side"
                },
                "esc back",
            ]),
            // The picture's own toggle is the first to give way where the line is short.
            At::Map if world => {
                let mut keys = owned(&["tab groupings", &format!("{lr}  compare")]);
                if listed {
                    keys.push(format!("{down}  the list"));
                }
                if !short {
                    keys.push(
                        if self.flat {
                            "t stand up"
                        } else {
                            "t top view"
                        }
                        .to_owned(),
                    );
                }
                keys.push("esc back".to_owned());
                keys
            }
            At::Map => vec![
                "tab grouping".to_owned(),
                pick(
                    &format!("{lr}  compare a {word}"),
                    &format!("{lr}  compare"),
                ),
                pick(
                    &format!("{down}  into the list, which lights each behavior"),
                    &format!("{down}  the list"),
                ),
                "esc back".to_owned(),
            ],
            // ← → also work on the map, so they are the first to give way where the line is
            // short of room.
            At::List(_) => vec![
                pick("↑  ↓  behavior, lit on the map", "↑  ↓  behavior"),
                pick("enter its actions", "enter actions"),
                format!("{lr}  {word}"),
                "esc back".to_owned(),
            ],
            At::Actions { .. } if on_summaries => vec![
                pick("↑  ↓  action, ringed on the map", "↑  ↓  action"),
                pick("enter its card", "enter card"),
                pick("esc back to the behaviors", "esc back"),
            ],
            At::Actions { .. } => vec![
                "↑  ↓  example".to_owned(),
                pick("enter opens it", "enter opens"),
                "esc back".to_owned(),
            ],
            At::Card { .. } => vec![
                format!("{lr}  example"),
                pick("o opens the run", "o run"),
                pick("c copies the action", "c copy"),
                "esc back".to_owned(),
            ],
        };
        // The planet keeps to its own keys after the period's.
        let mut all = if self.level == Level::Planet && self.at == At::Map {
            vec![cycle.to_owned()]
        } else {
            self.own_keys()
        };
        all.append(&mut keys);
        all
    }
}

/// What `d` switches to from the map on screen: Summaries from the label map, or their setting
/// up where none are kept; the labels from Summaries.
fn d_key(shown: Shown) -> &'static str {
    match shown {
        Shown::Labels { kept: true } => "d summaries",
        Shown::Labels { kept: false } => "d set up summaries",
        Shown::Summaries { .. } => "d labels",
    }
}
