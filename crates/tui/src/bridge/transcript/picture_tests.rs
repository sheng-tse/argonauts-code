//! A message that waited for the turn before it to end, or joined a running one, shows a row
//! for each picture its chips name, as one sent at once does.

use std::time::Instant;

use argonauts_core::app::{AppEvent, ConversationEvent};
use argonauts_core::names::{ModelRef, TurnId};
use argonauts_core::settings::ui::UiSettings;
use time::macros::datetime;

use super::Transcript;
use crate::bridge::{Cx, Effects};
use crate::format::WallClock;
use crate::model::ViewModel;
use crate::render::{Clock, RenderCtx};
use crate::state::AppState;
use crate::text::plain_text;
use crate::theme::{Glyphs, Theme};

fn send(transcript: &mut Transcript, model: &mut ViewModel, event: ConversationEvent) {
    let state = AppState::default();
    let settings = UiSettings::default();
    let mut out = Effects::default();
    let mut cx = Cx {
        model,
        state: &state,
        settings: &settings,
        now: Instant::now(),
        out: &mut out,
    };
    transcript.on_event(&AppEvent::Conversation { event }, &mut cx);
}

fn printed(model: &mut ViewModel) -> String {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
    plain_text(&model.history.drain_printable(&ctx))
}

#[test]
fn a_message_that_waited_or_joined_has_a_row_for_each_of_its_pictures() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shot.png");
    std::fs::write(&path, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();
    let mut model = ViewModel::default();
    let composer = &mut model.bottom.composer;
    composer.handle_paste(&path.display().to_string());
    composer.handle_paste(&path.display().to_string());
    composer.clear();
    let mut transcript = Transcript::with_clock(WallClock::pinned(datetime!(2026-10-09 10:42 UTC)));
    let turn = TurnId::numbered(1);
    send(
        &mut transcript,
        &mut model,
        ConversationEvent::TurnStarted {
            turn: turn.clone(),
            text: "the header [Image #1]".into(),
            worker: ModelRef::new("claude-code", Some("haiku".into())),
            read_only: false,
            commit: "8b41d07".into(),
            uncommitted: vec![],
            settings_sha256: None,
            continues: false,
        },
    );
    send(
        &mut transcript,
        &mut model,
        ConversationEvent::Joined {
            turn,
            text: "the footer [Image #2], not [Image #9]".into(),
        },
    );
    let printed = printed(&mut model);
    let header = printed.find("❯ the header [Image #1]").expect(&printed);
    let footer = printed.find("❯ the footer [Image #2]").expect(&printed);
    assert!(
        printed[header..footer].contains("⎿ [Image #1]"),
        "{printed}"
    );
    assert!(printed[footer..].contains("⎿ [Image #2]"), "{printed}");
    assert!(
        !printed.contains("⎿ [Image #9]"),
        "no picture of this conversation has that number:\n{printed}"
    );
}
