//! `?` on the world: every key the level shown answers, in three columns of dim `x for y`
//! hints as Claude Code's own `?` lists them, under the pointer's line in place of the
//! summary. The key line keeps to the keys most wanted; this is where the rest are, `m`
//! among them, which nothing else lists.

use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

use super::BehaviorsTab;
use super::level::Level;
use crate::base::kit::Kit;

/// Cells between two columns, as the prompt's `?` leaves.
const GAP: usize = 4;

impl BehaviorsTab {
    /// Whether `?` has the list of every key open.
    pub fn shortcuts_open(&self) -> bool {
        self.shortcuts
    }

    /// Every key the level shown answers, in the columns' order: getting about, then what the
    /// level itself does, then the period, the projects and the world's own toggles.
    pub fn shortcuts(&self) -> Vec<String> {
        let (lr, down) = (self.arrows(), self.down());
        let mut hints = vec![
            "1 2 3 for map, planet, universe".to_owned(),
            "+ - to zoom in or out".to_owned(),
            "scroll to zoom".to_owned(),
        ];
        let mut toggles = Vec::new();
        match self.level {
            Level::Map | Level::Planet => {
                hints.push("tab to step groupings".to_owned());
                hints.push(format!("{lr}  to compare"));
                if self.level == Level::Map {
                    hints.push(format!("{down}  for the list"));
                    toggles.push(if self.flat {
                        "t to stand the map up"
                    } else {
                        "t to lay the map flat"
                    });
                } else {
                    hints.push("drag to turn".to_owned());
                    toggles.push(if self.night {
                        "n for the day side"
                    } else {
                        "n for the night side"
                    });
                }
                toggles.push(if self.nature_hidden {
                    "m to show nature"
                } else {
                    "m to hide nature"
                });
            }
            Level::Universe => {
                hints.push(format!("{lr}  to pick a galaxy"));
                hints.push("enter to go in".to_owned());
            }
            Level::Galaxy(_) => {
                hints.push(format!("{lr}  to pick a star"));
                hints.push("enter for its system".to_owned());
            }
            Level::System(_) => {
                hints.push(format!("{lr}  to pick a body"));
                hints.push("enter to pick what you point at".to_owned());
            }
        }
        hints.push("g G for the first and last".to_owned());
        hints.push("esc to go back".to_owned());
        // Braille draws no relief, no night side and no nature to turn on or off.
        if self.draws_world() {
            hints.extend(toggles.into_iter().map(str::to_owned));
        }
        hints.push(crate::panels::view::periods::CYCLE_HINT.to_owned());
        hints.push("shift + r for the period before".to_owned());
        hints.push(self.projects_key().replacen(' ', " for ", 1));
        if self.claims(crossterm::event::KeyCode::Char('d'), true) {
            hints.push("d to switch labels and summaries".to_owned());
        }
        hints
    }

    /// `?` opens the list, and the next key closes it: Esc and `?` only close it, any other
    /// key does what it does as well. Whether the key is spent.
    pub(super) fn shortcuts_key(&mut self, code: crossterm::event::KeyCode) -> bool {
        use crossterm::event::KeyCode;

        if std::mem::take(&mut self.shortcuts) {
            return matches!(code, KeyCode::Esc | KeyCode::Char('?'));
        }
        if code == KeyCode::Char('?') && !self.deep.asking() {
            self.shortcuts = true;
            return true;
        }
        false
    }
}

/// `hints` in up to three columns `width` cells wide, each column as wide as its widest hint,
/// filled down first; fewer columns where three don't fit.
pub fn rows(kit: &Kit<'_>, hints: &[String], width: usize) -> Vec<Vec<Span<'static>>> {
    let layout = |columns: usize| {
        let per_column = hints.len().div_ceil(columns).max(1);
        let widths: Vec<usize> = hints
            .chunks(per_column)
            .map(|column| column.iter().map(|hint| hint.width()).max().unwrap_or(0))
            .collect();
        let total = 1 + widths.iter().sum::<usize>() + GAP * widths.len().saturating_sub(1);
        (per_column, widths, total)
    };
    let (per_column, widths, _) = (1..=3)
        .rev()
        .map(layout)
        .find(|(_, _, total)| *total <= width)
        .unwrap_or_else(|| layout(1));
    (0..per_column)
        .map(|row| {
            let mut line = String::from(" ");
            for column in 0..widths.len() {
                let Some(hint) = hints.get(column * per_column + row) else {
                    continue;
                };
                let start = 1 + widths[..column].iter().sum::<usize>() + GAP * column;
                let pad = start.saturating_sub(line.width());
                line.push_str(&" ".repeat(pad));
                line.push_str(hint);
            }
            vec![kit.dim(line)]
        })
        .collect()
}
