//! Keeps the band above the input in step with what you pinned (`ui.above_input`), with the
//! usage numbers the controller sends, and with the run and the context the screen holds.

use std::sync::Arc;

use argonauts_core::app::UiCommand;
use argonauts_core::runs::events::context::ContextDetail;
use argonauts_core::settings::bottom_line::STATS;
use argonauts_core::settings::ui::{ContextDetail as Wanted, ModName, ModWidth};

use crate::base::format::wall_now;
use crate::base::kit::ContextCard;
use crate::frame::state::AppState;
use crate::panels::pinned_run::PinnedRun;
use crate::panels::{ContextPrinted, conversation_context, session_context};

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
        .filter(|name| !ui.the_mod(*name).open)
        .collect();
    band.halves = band
        .mods
        .iter()
        .copied()
        .filter(|name| ui.the_mod(*name).width == ModWidth::Half)
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
    let runs = [ModName::Run, ModName::RunLimits, ModName::Waiting];
    band.run = if runs.into_iter().any(|name| band.pins(name)) {
        cx.state.followed_run().map(PinnedRun::of)
    } else {
        None
    };
    let context = band.pins(ModName::Context);
    band.context = if context {
        context_card(cx.state)
    } else {
        None
    };
    // The run's card names who has each task, by the names as they stand now.
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

/// The conversation's context by category where its CLI has counted it, else the main
/// session's start, before that count and for a CLI that says only how full it is, and
/// otherwise the fullest session of the run the screen follows.
fn context_card(state: &AppState) -> Option<ContextCard> {
    if let Some(breakdown) = &state.conversation.context {
        return Some(conversation_context(breakdown));
    }
    if let Some(card) = ContextPrinted::started_card(state) {
        return Some(card);
    }
    let run = state.followed_run()?;
    let share = |session: &&crate::frame::state::SessionState| session.context?.of_cap();
    let fullest = run
        .running_sessions()
        .filter(|session| share(session).is_some())
        .max_by(|a, b| share(a).unwrap_or(0.0).total_cmp(&share(b).unwrap_or(0.0)))?;
    let context = fullest.context?;
    let subject = match &fullest.label {
        Some(label) => label.clone(),
        None => crate::frame::state::task_label(fullest.role, fullest.step, fullest.task.as_ref()),
    };
    Some(session_context(
        subject,
        context.used,
        context.cap?,
        context.window,
    ))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::app::AppEvent;
    use argonauts_core::runs::events::task::TaskState;
    use argonauts_core::settings::ui::UiSettings;
    use argonauts_core::{RunId, TaskId, Verbosity};

    use super::*;
    use crate::ViewModel;
    use crate::app::options::UiOptions;
    use crate::bridge::Bridge;
    use crate::frame::state::{RunState, TaskRow};

    fn pinning(mods: &[ModName]) -> AppEvent {
        let mut ui = UiSettings::default();
        ui.above_input.mods = mods.to_vec();
        AppEvent::UiSettings {
            vim: false,
            verbosity: Verbosity::Full,
            ui: Some(Box::new(ui)),
        }
    }

    #[test]
    fn the_runs_card_draws_from_the_run_the_screen_follows() {
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let now = Instant::now();
        bridge.on_event(&mut model, &pinning(&[ModName::Run]), now);
        assert_eq!(model.bottom.band.run, None, "no run is followed yet");

        let id = RunId::new("run-03").expect("a run id");
        let mut run = RunState::new(id.clone());
        for (n, state) in [(1, TaskState::Merged), (2, TaskState::Running)] {
            let mut task = TaskRow::new(TaskId::numbered(n), "CSV writer");
            task.state = Some(state);
            run.tasks.insert(task.id.clone(), task);
        }
        bridge.state.runs.insert(id.clone(), run);
        bridge.state.attached = Some(id.clone());
        bridge.on_event(&mut model, &pinning(&[ModName::Run]), now);
        let pinned = model.bottom.band.run.as_ref().expect("the run's card");
        assert_eq!(pinned.summary.run, Some(id));
        let states: Vec<TaskState> = pinned.summary.tasks.iter().map(|task| task.state).collect();
        assert_eq!(states, [TaskState::Merged, TaskState::Running]);

        // Unpinned, nothing about the run is kept for the band.
        bridge.on_event(&mut model, &pinning(&[ModName::Spend]), now);
        assert_eq!(model.bottom.band.run, None);
    }

    /// Codex's start as the controller sends it, before its session starts and before the
    /// Context tab first opens.
    fn codex_start() -> AppEvent {
        use argonauts_core::runs::events::context::{
            FirstCount, StartHolds, StartPart, StartedFrom, StartingCount,
        };

        let start = StartingCount {
            agent: "codex".to_owned(),
            model: Some("gpt-6-luna".to_owned()),
            window: Some(372_000),
            buffer: 0,
            parts: vec![
                StartPart::holding(StartHolds::OwnPromptAndTools, 15_236, "promptBorder"),
                StartPart::holding(StartHolds::InstructionFiles, 1_200, "claude"),
            ],
            files: Vec::new(),
            tools: None,
            from: StartedFrom::LastSession,
            first_count: Some(FirstCount::Turn),
        };
        AppEvent::MainStart {
            start: Box::new(start),
            cli_window: false,
            estimated: true,
        }
    }

    fn conversation(event: argonauts_core::app::ConversationEvent) -> AppEvent {
        AppEvent::Conversation { event }
    }

    /// The card above the input draws the main session's start before its CLI's first count,
    /// while the CLI loads, and for Codex, which says only how full the session is, once it
    /// has said: as an estimate, though the Context tab opened before the session started.
    #[test]
    fn the_context_card_draws_the_start_before_loading_and_for_codex() {
        use argonauts_core::AgentEvent;
        use argonauts_core::app::views::ContextView;
        use argonauts_core::app::{ConversationEvent, ViewData};
        use argonauts_core::base::names::{ModelRef, TurnId};

        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let now = Instant::now();
        bridge.on_event(&mut model, &pinning(&[ModName::Context]), now);
        assert_eq!(model.bottom.band.context, None, "nothing kept yet");

        bridge.on_event(&mut model, &codex_start(), now);
        let card = model.bottom.band.context.clone().expect("the start's card");
        let before = card.before.expect("a card before the count");
        assert_eq!((card.used, before.loading), (Some(16_436), false));

        let AppEvent::MainStart { start, .. } = codex_start() else {
            unreachable!()
        };
        let tab = ContextView {
            main_start: Some(*start),
            ..ContextView::default()
        };
        let opened = AppEvent::View {
            data: Box::new(ViewData::Context { view: tab }),
        };
        bridge.on_event(&mut model, &opened, now);

        let turn = TurnId::numbered(1);
        let started = ConversationEvent::TurnStarted {
            turn: turn.clone(),
            text: "hi".to_owned(),
            worker: ModelRef::new("codex", Some("gpt-6-luna".to_owned())),
            read_only: true,
            commit: String::new(),
            uncommitted: Vec::new(),
            settings_sha256: None,
            continues: false,
        };
        bridge.on_event(&mut model, &conversation(started), now);
        let card = model.bottom.band.context.clone().expect("the loading card");
        assert!(card.before.is_some_and(|before| before.loading), "{card:?}");

        let fill = ConversationEvent::Agent {
            turn,
            event: AgentEvent::ContextFill {
                used: 30_500,
                size: None,
                sent: None,
            },
        };
        bridge.on_event(&mut model, &conversation(fill), now);
        let card = model.bottom.band.context.clone().expect("Codex's card");
        assert_eq!((card.used, card.before), (Some(30_500), None));
        assert!(
            card.used_estimated,
            "Codex works its fill out from its totals"
        );
    }

    #[test]
    fn a_pinned_context_mod_has_the_session_count_its_context_until_it_is_unpinned() {
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let now = Instant::now();
        let watch = |detail| UiCommand::WatchContext { detail };

        let out = bridge.on_event(&mut model, &pinning(&[ModName::Context]), now);
        assert_eq!(out.commands, [watch(Some(ContextDetail::Summary))]);
        let out = bridge.on_event(&mut model, &pinning(&[ModName::Context]), now);
        assert!(out.commands.is_empty(), "asked once, not with every event");
        let out = bridge.on_event(&mut model, &pinning(&[]), now);
        assert_eq!(out.commands, [watch(None)]);
    }
}
