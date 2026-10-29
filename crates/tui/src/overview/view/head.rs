//! The panel's head: the tabs with the periods at their right, and the row under them, what
//! the tab is about with its sub-tabs or what the period came to.

use argonauts_core::overview::OverviewPeriod;
use ratatui::text::Span;

use super::home::grouping_choices;
use super::{OverviewView, Tab};
use crate::format::{self, Figures};
use crate::kit::{Kit, spread};
use crate::overview::cards::period_words;
use crate::overview::context;
use crate::overview::periods;
use crate::overview::stats::behaviors;
use crate::overview::subtabs::{Level, TAB_GAP, chosen, chosen_sub_tab};
use crate::text::{spans_width, truncate_spans};
use crate::theme::Role;
use crate::views::ViewCtx;

impl OverviewView {
    /// `Overview  Status  [Usage]  Context  Limits  Stats`, and on a usage tab the periods at
    /// the right, as Claude Code's Settings dialog lays its tabs out.
    pub(super) fn tabs(&self, kit: &Kit<'_>, width: usize) -> Vec<Span<'static>> {
        // The panel's title in bold, as Claude Code's Settings, then every tab but the open one
        // in plain text, all alike, so none looks picked.
        let mut left = vec![Span::styled(" Settings", kit.bold(Role::Text))];
        for tab in Tab::ALL {
            left.push(Span::raw(TAB_GAP));
            // Config's search box and rows belong to its tab, so it stays lit while they
            // have the keys.
            let lit = self.level() == Level::Tabs || tab == Tab::Config;
            left.push(if tab == self.tab {
                chosen(kit, tab.title(), lit)
            } else {
                Span::raw(format!(" {} ", tab.title()))
            });
        }
        let mut right = Vec::new();
        if self.tab.shows_usage() && self.tab != Tab::Overview {
            // A cell between the last tab and the periods, however narrow the screen.
            left.push(Span::raw(" "));
            let words = periods::PERIODS.map(|(period, word)| (word, period == self.period));
            right = period_choices(kit, &words);
        } else if self.tab == Tab::Overview && self.home.on_behaviors() {
            // Behaviors' periods as Stats draws its own, the chosen one alone where all three
            // don't fit.
            left.push(Span::raw(" "));
            let shown = self.home.behaviors.request.period;
            let all = OverviewPeriod::ALL.map(|period| (period.title(), period == shown));
            right = first_fitting(
                spans_width(&left),
                width,
                [
                    period_choices(kit, &all),
                    period_choices(kit, &[(shown.title(), true)]),
                ],
            );
        }
        if spans_width(&left) + spans_width(&right) > width {
            return truncate_spans(&left, width);
        }
        spread(left, right, width)
    }

    /// The open tab's own tabs, the chosen one filled while Tab moves them.
    fn sub_tab_spans(&self, kit: &Kit<'_>, ctx: &ViewCtx<'_>) -> Vec<Span<'static>> {
        let Some((names, at)) = self.sub_tabs(ctx) else {
            return Vec::new();
        };
        let mut spans = Vec::new();
        for (index, name) in names.into_iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw(TAB_GAP));
            }
            spans.push(if index == at {
                chosen_sub_tab(kit, &name, self.level().moves_sub_tabs())
            } else {
                kit.dim(format!(" {name} "))
            });
        }
        spans
    }

    /// What the tab is about, and at the right what the period came to.
    pub(super) fn about(
        &self,
        kit: &Kit<'_>,
        ctx: &ViewCtx<'_>,
        width: usize,
    ) -> Vec<Span<'static>> {
        if self.tab == Tab::Stats
            && let Some(page) = &self.stats.page
        {
            return page.about(kit, width);
        }
        if self.tab == Tab::Usage
            && let Some(tab) = self.plan_usage_tab(ctx)
        {
            let mut row = vec![Span::raw(" ")];
            row.extend(tab.sub_tab_row(kit, self.level().moves_sub_tabs()));
            // When the plans were read, at the right where the periods were before.
            let read = vec![kit.dim(tab.read_words()), Span::raw(" ")];
            if spans_width(&row) + spans_width(&read) + 2 > width {
                return truncate_spans(&row, width);
            }
            return spread(row, read, width);
        }
        if self.tab == Tab::Overview {
            let mut row = vec![Span::raw(" ")];
            row.extend(self.home.about(kit, &ctx.state.names, width - 1));
            if self.home.on_behaviors() {
                let tab = &self.home.behaviors;
                let keys_here =
                    self.level() == Level::Cards && tab.stop() == behaviors::Stop::Groupings;
                let right = first_fitting(
                    spans_width(&row) + 1,
                    width,
                    grouping_choices(kit, tab.request.grouping, keys_here),
                );
                return spread(row, right, width);
            }
            return row;
        }
        if self.tab == Tab::Stats {
            let left = vec![kit.dim(format!(" {}", self.stats.description()))];
            let mut right = self.sub_tab_spans(kit, ctx);
            right.push(Span::raw(" "));
            if spans_width(&left) + spans_width(&right) + 1 > width {
                return spread(Vec::new(), truncate_spans(&right, width), width);
            }
            return spread(left, right, width);
        }
        let left = vec![kit.dim(format!(" {}", self.tab.description()))];
        let right = match self.summary(ctx) {
            Some(summary) => vec![kit.dim(format!("{summary} "))],
            None => Vec::new(),
        };
        if spans_width(&left) + spans_width(&right) + 2 > width {
            return truncate_spans(&left, width);
        }
        spread(left, right, width)
    }

    /// What the open tab comes to, for the right of the row under the tabs.
    fn summary(&self, ctx: &ViewCtx<'_>) -> Option<String> {
        match self.tab {
            Tab::Status => {
                let status = self.status.as_ref()?;
                let run = status.run.as_ref()?.to_string();
                Some(match &status.spend {
                    Some(limit) => format!("{run} · {}", format::spend(&limit.spent)),
                    None => run,
                })
            }
            Tab::Context => Some(context::summary(&self.context.as_ref()?.rows)),
            _ => {
                let usage = self.usage(ctx)?;
                let tokens = usage.total.total_tokens();
                Some(format!(
                    "{} {} · {} tokens",
                    format::spend(&usage.total),
                    period_words(usage.period),
                    Figures::for_tokens_of(tokens).tokens(tokens)
                ))
            }
        }
    }
}

/// Periods side by side at the right of the tabs, the one shown bold in the accent, as the
/// usage tabs and Behaviors draw theirs.
fn period_choices(kit: &Kit<'_>, words: &[(&str, bool)]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (at, (word, shown)) in words.iter().enumerate() {
        if at > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(if *shown {
            Span::styled(word.to_string(), kit.bold(Role::Question))
        } else {
            kit.dim(word.to_string())
        });
    }
    spans.push(Span::raw(" "));
    spans
}

/// The first of `choices`, widest first, that fits in `width` after the `used` cells, which
/// count any gap before it; none where even the last doesn't.
fn first_fitting<const N: usize>(
    used: usize,
    width: usize,
    choices: [Vec<Span<'static>>; N],
) -> Vec<Span<'static>> {
    choices
        .into_iter()
        .find(|spans| used + spans_width(spans) <= width)
        .unwrap_or_default()
}
