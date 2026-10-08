//! Keeps the band above the input in step with what you pinned (`ui.above_input`), with the
//! usage numbers the controller sends, and with the run and the context the screen holds.

use std::sync::Arc;

use argonauts_core::app::UiCommand;
use argonauts_core::events::context::ContextDetail;
use argonauts_core::settings::bottom_line::STATS;
use argonauts_core::settings::ui::{ContextDetail as Wanted, WidgetName, WidgetWidth};

use crate::bottom::{PinnedRun, wall_now};
use crate::overview::{conversation_context, session_context};
use crate::state::AppState;
use crate::widgets::ContextCard;

use super::Cx;

pub fn sync(cx: &mut Cx<'_>) {
    let ui = cx.settings;
    let band = &mut cx.model.bottom.band;
    if band.mods != ui.above_input.mods {
        band.mods.clone_from(&ui.above_input.mods);
    }
    band.folded = band
        .mods
        .iter()
        .copied()
        .filter(|name| !ui.widget(*name).open)
        .collect();
    band.halves = band
        .mods
        .iter()
        .copied()
        .filter(|name| ui.widget(*name).width == WidgetWidth::Half)
        .collect();
    band.side_by_side_at = usize::from(ui.above_input.side_by_side_at);
    // A span the settings check refused lands at once.
    band.settle = ui
        .above_input
        .settle
        .parse()
        .ok()
        .flatten()
        .unwrap_or_default();
    band.shown = ui.above_input.shown;
    band.max_rows = usize::from(ui.above_input.max_rows);
    band.min_width = usize::from(ui.above_input.min_width);
    band.when_question = ui.above_input.when_question;
    let fresh = match (&band.usage, &cx.state.usage) {
        (Some(shown), Some(latest)) => !Arc::ptr_eq(shown, latest),
        (None, None) => false,
        _ => true,
    };
    if fresh {
        band.usage.clone_from(&cx.state.usage);
        band.names = cx.state.names.clone();
        band.wall = wall_now();
    }
    band.line_stats = cx
        .state
        .status
        .as_ref()
        .is_some_and(|status| status.line.settings.has(STATS));
    let runs = [WidgetName::Run, WidgetName::Budget, WidgetName::Waiting];
    band.run = if runs.into_iter().any(|name| band.pins(name)) {
        cx.state.followed_run().map(PinnedRun::of)
    } else {
        None
    };
    let context = band.pins(WidgetName::Context);
    band.context = if context {
        context_card(cx.state)
    } else {
        None
    };
    // The run's cards name who has each task, by the names as they stand now.
    if band.run.is_some() && band.names != cx.state.names {
        band.names = cx.state.names.clone();
    }
    // The conversation's session counts its context only while something shows it. During
    // a replay nothing is sent, so the ask waits for the first event after it.
    let wanted = match ui.mods.context.own.detail {
        Wanted::Summary => ContextDetail::Summary,
        Wanted::Full => ContextDetail::Full,
    };
    let detail = context.then_some(wanted);
    if band.context_watched != detail && !cx.state.replaying() {
        band.context_watched = detail;
        cx.out.send(UiCommand::WatchContext { detail });
    }
}

/// The conversation's context by category where its CLI has counted it, and otherwise the
/// fullest session of the run the screen follows.
fn context_card(state: &AppState) -> Option<ContextCard> {
    if let Some(breakdown) = &state.chat.context {
        return Some(conversation_context(breakdown));
    }
    let run = state.followed_run()?;
    let share = |session: &&crate::state::SessionState| {
        session
            .context
            .filter(|context| context.cap > 0)
            .map(|context| context.used as f64 / context.cap as f64)
    };
    let fullest = run
        .running_sessions()
        .filter(|session| share(session).is_some())
        .max_by(|a, b| share(a).unwrap_or(0.0).total_cmp(&share(b).unwrap_or(0.0)))?;
    let context = fullest.context?;
    let subject = match &fullest.label {
        Some(label) => label.clone(),
        None => crate::state::task_label(fullest.role, fullest.step, fullest.card.as_ref()),
    };
    Some(session_context(
        subject,
        context.used,
        context.cap,
        fullest.context_window,
    ))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::app::AppEvent;
    use argonauts_core::events::card::CardState;
    use argonauts_core::settings::ui::UiSettings;
    use argonauts_core::{CardId, RunId, Verbosity};

    use super::*;
    use crate::ViewModel;
    use crate::bridge::Bridge;
    use crate::options::UiOptions;
    use crate::state::{CardRow, RunState};

    fn pinning(mods: &[WidgetName]) -> AppEvent {
        let mut ui = UiSettings::default();
        ui.above_input.mods = mods.to_vec();
        AppEvent::UiSettings {
            vim: false,
            verbosity: Verbosity::Full,
            ui: Some(Box::new(ui)),
        }
    }

    #[test]
    fn the_runs_cards_draw_from_the_run_the_screen_follows() {
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let now = Instant::now();
        bridge.on_event(&mut model, &pinning(&[WidgetName::Run]), now);
        assert_eq!(model.bottom.band.run, None, "no run is followed yet");

        let id = RunId::new("run-03").expect("a run id");
        let mut run = RunState::new(id.clone());
        for (n, state) in [(1, CardState::Integrated), (2, CardState::Running)] {
            let mut task = CardRow::new(CardId::numbered(n), "CSV writer");
            task.state = Some(state);
            run.cards.insert(task.id.clone(), task);
        }
        bridge.state.runs.insert(id.clone(), run);
        bridge.state.attached = Some(id.clone());
        bridge.on_event(&mut model, &pinning(&[WidgetName::Run]), now);
        let pinned = model.bottom.band.run.as_ref().expect("the run's card");
        assert_eq!(pinned.summary.run, Some(id));
        let states: Vec<CardState> = pinned.summary.cards.iter().map(|task| task.state).collect();
        assert_eq!(states, [CardState::Integrated, CardState::Running]);

        // Unpinned, nothing about the run is kept for the band.
        bridge.on_event(&mut model, &pinning(&[WidgetName::Spend]), now);
        assert_eq!(model.bottom.band.run, None);
    }

    #[test]
    fn a_pinned_context_mod_has_the_session_count_its_context_until_it_is_unpinned() {
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let now = Instant::now();
        let watch = |detail| UiCommand::WatchContext { detail };

        let out = bridge.on_event(&mut model, &pinning(&[WidgetName::Context]), now);
        assert_eq!(out.commands, [watch(Some(ContextDetail::Summary))]);
        let out = bridge.on_event(&mut model, &pinning(&[WidgetName::Context]), now);
        assert!(out.commands.is_empty(), "asked once, not with every event");
        let out = bridge.on_event(&mut model, &pinning(&[]), now);
        assert_eq!(out.commands, [watch(None)]);
    }
}
