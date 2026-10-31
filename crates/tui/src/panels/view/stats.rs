//! The Stats tab on the panel: its blocks or the stats mod's page, what `mods/stats.toml`
//! sets once the numbers carry it, and the tab's keys.

use argonauts_core::app::ViewRequest;
use argonauts_core::settings::stats::StatsGroup;
use argonauts_core::settings::ui::ModName;
use crossterm::event::{KeyCode, KeyEvent};

use super::{Follow, OverviewView, Tab, note, pin};
use crate::base::kit::{Drawn, Kit};
use crate::frame::view::list;
use crate::frame::view::{ViewCtx, ViewOutcome};
use crate::panels::stats::StatsKey;

impl OverviewView {
    pub(super) fn stats_blocks(
        &self,
        kit: &Kit<'_>,
        ctx: &ViewCtx<'_>,
        width: usize,
    ) -> Vec<Drawn> {
        let usage = self.usage(ctx).filter(|_| self.stats_opened_on.is_none());
        if self.stats.page_open() {
            let blocks = self
                .stats
                .page_blocks(kit, usage, &ctx.state.names, self.wall(), width);
            // The page's rows are the keys' focus, last, under its preview.
            self.focus_block.set(blocks.len().saturating_sub(1));
            return blocks;
        }
        let Some(usage) = usage else {
            return vec![note(kit, "Reading the crew's stats…")];
        };
        let (blocks, focus) = self.stats.blocks(
            kit,
            usage,
            (&ctx.state.names, self.wall()),
            width,
            self.home.every_day_tokens(),
        );
        self.focus_block.set(focus);
        blocks
    }

    /// Takes what `mods/stats.toml` sets from the first numbers that carry it: the period,
    /// when `/stats` named none, then the group and the measure. Numbers for another period
    /// are asked for as the tab draws.
    pub(super) fn settle_stats(&mut self, ctx: &ViewCtx<'_>) {
        if self.tab != Tab::Stats {
            return;
        }
        let Some(usage) = ctx.state.usage.as_deref() else {
            return;
        };
        let Some(stats) = &usage.stats else {
            return;
        };
        if self.stats_opened_on.take() == Some(self.period) {
            self.period = stats.settings.period.usage();
        }
        if usage.period == self.period {
            self.stats.settle(stats);
            self.stats.pick_wanted(usage, &ctx.state.names, self.wall());
        }
    }

    pub(super) fn stats_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let usage = self.usage(ctx);
        let names = &ctx.state.names;
        match self.stats.key(key.code, usage, names, self.wall()) {
            None if key.code == KeyCode::Char('q') => ViewOutcome::Close,
            stats_key => self.stats_outcome(stats_key),
        }
    }

    /// The stats mod's page has the keys. Esc goes back to the tab.
    pub(super) fn stats_page_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        if self.stats.page.is_none() {
            // Still waiting for the file's settings.
            return match key.code {
                KeyCode::Esc => {
                    self.stats.close_page();
                    ViewOutcome::Consumed
                }
                _ => ViewOutcome::Ignored,
            };
        }
        let stats_key = self.stats.page_key(key);
        self.stats_outcome(stats_key)
    }

    pub(super) fn stats_outcome(&mut self, key: Option<StatsKey>) -> ViewOutcome {
        match key {
            Some(StatsKey::Moved) => {
                self.follow = Some(Follow::Focus);
                ViewOutcome::Consumed
            }
            Some(StatsKey::Period(period)) => {
                self.period = period;
                self.scroll = 0;
                ViewOutcome::Request(ViewRequest::Usage { period })
            }
            Some(StatsKey::Pin) => pin(ModName::Stats),
            Some(StatsKey::Send(command)) => ViewOutcome::Send(command),
            None => ViewOutcome::Ignored,
        }
    }
}

/// Stats' sub-tabs, in display order.
pub(super) const STATS_GROUPS: [(StatsGroup, &str); 3] = [
    (StatsGroup::Cli, "By CLI"),
    (StatsGroup::Model, "By model"),
    (StatsGroup::Role, "By role"),
];
