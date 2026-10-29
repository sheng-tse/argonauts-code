//! The Overview tab on the panel: its keys, what Behaviors' world reads from each key and
//! frame, and Behaviors' groupings beside its sub-tabs.

use argonauts_core::app::ViewRequest;
use argonauts_core::behavior_map::Grouping;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Span;

use super::{Follow, OverviewView, Tab};
use crate::kit::Kit;
use crate::overview::home::HomeKey;
use crate::overview::stats::behaviors;
use crate::overview::subtabs::{self, Level};
use crate::views::{ViewCtx, ViewOutcome, list};

impl OverviewView {
    /// Coming down from the sub-tabs onto Behaviors, the keys land on its groupings first.
    pub(super) fn behaviors_from_above(&mut self) {
        if self.tab == Tab::Overview && self.home.on_behaviors() && self.level() == Level::Cards {
            self.home.behaviors.arrive_from_above();
        }
    }

    /// What Behaviors' world reads from each key and frame: when, whether motion holds still,
    /// and whether the keys are vim's.
    pub(super) fn sync_behaviors(&mut self, ctx: &ViewCtx<'_>) {
        self.home.behaviors.deep.sync(&ctx.state.deep);
        self.home.behaviors.moment = behaviors::Moment {
            now: ctx.now,
            still: ctx.theme.reduced_motion,
            skips_frames: ctx.theme.skips_frames,
            vim: list::vim(ctx.state),
        };
    }

    /// The Overview tab's keys. Enter on a model opens it in Stats, for the same days.
    pub(super) fn home_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let ctrl = key.modifiers == KeyModifiers::CONTROL;
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        self.sync_behaviors(ctx);
        let names = &ctx.state.names;
        match self.home.key(key.code, ctrl, names) {
            Some(HomeKey::Moved) => {
                self.follow = Some(Follow::Focus);
                ViewOutcome::Consumed
            }
            Some(HomeKey::Ask(request)) => {
                self.scroll = 0;
                ViewOutcome::Request(request)
            }
            Some(HomeKey::Copy(text)) => ViewOutcome::Copy(text),
            Some(HomeKey::Send(command)) => ViewOutcome::Send(command),
            Some(HomeKey::OpenModel {
                agent,
                model,
                period,
            }) => {
                self.tab = Tab::Stats;
                self.scroll = 0;
                self.follow = Some(Follow::Focus);
                self.stats.open_model(agent, model);
                self.period = period;
                ViewOutcome::Request(ViewRequest::Usage { period })
            }
            None if key.code == KeyCode::Char('q') => ViewOutcome::Close,
            None => ViewOutcome::Ignored,
        }
    }
}

/// Behaviors' groupings at the right of its sub-tabs, drawn as Stats draws By CLI, By model
/// and By role: every one, else the chosen one between its neighbours, else the chosen one
/// alone; the chosen one filled while the row has the keys.
pub(super) fn grouping_choices(
    kit: &Kit<'_>,
    shown: Grouping,
    keys_here: bool,
) -> [Vec<Span<'static>>; 3] {
    let titles = Grouping::ALL.map(grouping_title);
    let at = Grouping::ALL
        .iter()
        .position(|grouping| *grouping == shown)
        .unwrap_or(0);
    let count = titles.len();
    let near = [
        titles[(at + count - 1) % count],
        titles[at],
        titles[(at + 1) % count],
    ];
    let with_edge = |mut spans: Vec<Span<'static>>| {
        spans.push(Span::raw(" "));
        spans
    };
    [
        with_edge(subtabs::row(kit, &titles, at, keys_here)),
        with_edge(subtabs::row(kit, &near, 1, keys_here)),
        with_edge(subtabs::row(kit, &[titles[at]], 0, keys_here)),
    ]
}

/// `By model`, as the grouping's selector names it.
fn grouping_title(grouping: Grouping) -> &'static str {
    match grouping {
        Grouping::Model => "By model",
        Grouping::Cli => "By CLI",
        Grouping::Role => "By role",
        Grouping::Workflow => "By workflow",
        Grouping::Project => "By project",
        Grouping::Outcome => "By outcome",
    }
}
