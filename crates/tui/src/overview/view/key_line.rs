//! The panel's key line: what the keys do on the row that has them, for each tab, the way
//! out always whole and the other keys giving way from the last.

use argonauts_core::app::AppMode;
use argonauts_core::settings::ui::ModName;

use super::status::SetupPart;
use super::{OverviewView, Tab};
use crate::overview::context;
use crate::overview::periods;
use crate::overview::subtabs::Level;
use crate::views::ViewCtx;
use crate::views::dock::PANEL_HINT;

/// The key line's way out, while nothing runs behind the panel.
const CLOSES: &str = "esc closes";

impl OverviewView {
    /// The keys in `width` cells. The way out always shows whole, so the keys before it give
    /// way from the last one back.
    pub(super) fn keys(&self, ctx: &ViewCtx<'_>, more: bool, width: usize) -> String {
        let running = matches!(
            ctx.state.mode,
            Some(AppMode::Answering { .. } | AppMode::Working { .. } | AppMode::Pausing { .. })
        );
        let close = if running { PANEL_HINT } else { CLOSES };
        // On Stats' table its own way out comes last: back up a row, or the filter cleared.
        if self.tab == Tab::Stats
            && !self.stats.page_open()
            && let Some(mut keys) = self.stats.table_keys()
        {
            let last = keys.pop().unwrap_or_default();
            if more {
                keys.insert(0, "↓  more".to_owned());
            }
            if self.has_periods() {
                keys = periods::hint_first(keys);
            }
            return fit_keys(keys, &last, width);
        }
        if self.tab == Tab::Overview && self.home.on_behaviors() && self.level() == Level::Cards {
            let mut keys = self.home.behaviors.keys(width);
            let last = keys.pop().unwrap_or_default();
            return fit_keys(keys, &last, width);
        }
        let along = self.tab_words(ctx);
        let has_sub_tabs = self.sub_tabs(ctx).is_some();
        let rows: Vec<String> = match self.level() {
            // Iosevka draws the side arrows two cells wide, so each has two blanks after it.
            Level::Tabs if self.tab == Tab::Context => vec![
                "tab · ←  →  tabs".to_owned(),
                "↓  into the list".to_owned(),
                "enter opens one".to_owned(),
            ],
            Level::Tabs => vec![
                along.clone(),
                if has_sub_tabs {
                    "↓  sub-tabs, then cards".to_owned()
                } else {
                    "↓  into the cards".to_owned()
                },
            ],
            Level::SubTabs => vec![
                along.clone(),
                "↓  into the cards".to_owned(),
                "↑  tabs".to_owned(),
            ],
            Level::Cards => vec![],
        };
        let owned = |keys: Vec<&str>| keys.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let mut close = close;
        let mut keys = if self.tab == Tab::Stats && self.stats.page_open() {
            close = "esc back";
            self.stats
                .page
                .as_ref()
                .map(|page| owned(page.keys()))
                .unwrap_or_default()
        } else if self.tab == Tab::Overview {
            self.home.keys()
        } else if self.tab == Tab::Stats {
            owned(self.stats.keys())
        } else {
            rows
        };
        // The period's hint comes first, before the word that more waits below.
        let with_hint = |keys: Vec<String>| {
            if self.has_periods() {
                periods::hint_first(keys)
            } else {
                keys
            }
        };
        if self.tab == Tab::Usage {
            let mut keys = match self.level() {
                Level::Tabs => vec![along, "↓  CLIs".to_owned()],
                Level::SubTabs => vec![along, "↓  into the cards".to_owned(), "↑  tabs".to_owned()],
                Level::Cards if self.cli > 0 => vec![
                    "↑↓  move".to_owned(),
                    "enter closes the page".to_owned(),
                    along,
                ],
                Level::Cards => vec![
                    "↑↓  card".to_owned(),
                    "enter opens its page".to_owned(),
                    along,
                ],
            };
            keys.push("ctrl+r reads again".to_owned());
            if more {
                keys.insert(0, "↓  more".to_owned());
            }
            return fit_keys(with_hint(keys), close, width);
        }
        // In the cards ↑ and ↓ move a card or a row at a time, and the tab scrolls with them.
        let in_cards = self.level() == Level::Cards;
        let words = |list: &[&str]| list.iter().map(|key| (*key).to_owned()).collect::<Vec<_>>();
        match self.tab {
            Tab::Stats | Tab::Overview => {}
            Tab::Status => {
                let tasks = self.status.as_ref().map_or(0, |status| status.tasks.len());
                let has_runs = self
                    .setup
                    .as_ref()
                    .is_some_and(|view| !view.runs.is_empty());
                let on_run = self.status_focus(ctx) == Some(ModName::Run);
                let list: &[&str] = match (in_cards, self.setup_focus()) {
                    (false, _) => &[],
                    (true, Some(SetupPart::Settings)) => &["↑↓  card", "enter all settings"],
                    (true, Some(SetupPart::Runs)) if has_runs => &["↑↓  card", "enter workflow"],
                    (true, Some(_)) => &["↑↓  card"],
                    (true, None) if on_run && tasks > 0 => &["↑↓  task", "enter agent"],
                    (_, None) => &["enter workflow"],
                };
                keys.extend(words(list));
                if in_cards {
                    keys.push(along);
                }
                if self.setup_focus().is_none() {
                    keys.extend(words(&[
                        "p pin above the input",
                        "s ask the narrator",
                        "u used or left",
                    ]));
                }
            }
            Tab::Context => {
                // An agent's own block has one way out, back to the list.
                if self.pane.opened().is_some() {
                    let keys = if more {
                        vec!["↓  more".to_owned()]
                    } else {
                        Vec::new()
                    };
                    return fit_keys(keys, "esc back to the list", width);
                }
                if in_cards {
                    keys.push("↑↓  move".to_owned());
                    let enter = match self.pane.target() {
                        Some(context::Target::List(list)) if self.pane.is_open(&list) => {
                            Some("enter folds it")
                        }
                        Some(context::Target::List(_)) => Some("enter opens it"),
                        Some(context::Target::Agent { .. }) => Some("enter its own block"),
                        None => None,
                    };
                    keys.extend(enter.map(str::to_owned));
                    keys.push(along);
                }
            }
            _ => {
                if in_cards {
                    keys.push(if self.tab == Tab::Limits {
                        "↑↓  limit".to_owned()
                    } else {
                        "↑↓  card".to_owned()
                    });
                    if self.tab == Tab::Limits {
                        keys.push("enter change".to_owned());
                    }
                    keys.push(along);
                }
                keys.extend(words(&[
                    "p pin above the input",
                    "u used or left",
                    ", settings",
                ]));
            }
        }
        // On the Context tab's tabs, ↓ already says it goes into the list.
        let down_said = self.tab == Tab::Context && self.level() == Level::Tabs;
        if more && !down_said {
            keys.insert(0, "↓  more".to_owned());
        }
        fit_keys(with_hint(keys), close, width)
    }

    /// What Tab does on the row with the keys: `tab · ← → tabs` on the tabs, and from the
    /// sub-tabs down the sub-tabs it goes along, such as `tab By CLI, By model, By role`.
    fn tab_words(&self, ctx: &ViewCtx<'_>) -> String {
        let sub_tabs = self.sub_tabs(ctx).map(|(names, _)| names);
        match (self.level(), sub_tabs) {
            (Level::Tabs, _) => "tab · ←  →  tabs".to_owned(),
            (Level::SubTabs, Some(names)) => {
                format!("tab · ←  →  {}", along_words(self.tab, &names))
            }
            (Level::Cards, Some(names)) => format!("tab {}", along_words(self.tab, &names)),
            (_, None) => "tab tabs".to_owned(),
        }
    }
}

/// The keys in `width` cells, the way out always whole, the others giving way from the last.
pub(super) fn fit_keys(mut keys: Vec<String>, close: &str, width: usize) -> String {
    loop {
        let line = keys
            .iter()
            .map(String::as_str)
            .chain([close])
            .collect::<Vec<_>>()
            .join(" · ");
        if crate::text::str_width(&line) < width || keys.pop().is_none() {
            return line;
        }
    }
}

/// The sub-tabs Tab goes along, named: `Overview or Models`, `By CLI, By model, By role`, or
/// on Usage, where a name each would crowd the line, `CLIs`.
fn along_words(tab: Tab, names: &[String]) -> String {
    match names {
        _ if tab == Tab::Usage => "CLIs".to_owned(),
        [first, second] => format!("{first} or {second}"),
        names => names.join(", "),
    }
}
