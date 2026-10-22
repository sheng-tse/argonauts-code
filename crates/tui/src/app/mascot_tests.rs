use std::time::{Duration, Instant};

use argonauts_core::app::UltraColour;
use ratatui::layout::{Position, Size};

use super::*;
use crate::cell::Cell;
use crate::mascot::graphics::pictures_in;
use crate::mascot::picture;
use crate::model::ViewModel;
use crate::terminal::InlineTerminal;
use crate::theme::Glyphs;

const COLUMNS: u16 = 80;
const ROWS: u16 = 20;

fn ui() -> (Ui<Vec<u8>>, Instant) {
    ui_at(COLUMNS)
}

fn ui_at(columns: u16) -> (Ui<Vec<u8>>, Instant) {
    let terminal = InlineTerminal::new(Vec::new(), Size::new(columns, ROWS), Position::new(0, 0));
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let t0 = Instant::now();
    (
        Ui::new(terminal, theme, ViewModel::new("conversation"), t0),
        t0,
    )
}

fn header(workflow: &str, ultra: Option<UltraColour>) -> SessionHeader {
    SessionHeader {
        app: "Argonauts Code".into(),
        version: "0.0.1".into(),
        main_agent: Some("Claude Code".into()),
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        workflow: workflow.into(),
        ultra,
        folder: "~/github/argonauts".into(),
        agents: Vec::new(),
        runs_going: 0,
        runs_need_you: 0,
        way_back: None,
    }
}

fn written(ui: &mut Ui<Vec<u8>>) -> String {
    String::from_utf8_lossy(&std::mem::take(ui.terminal_mut().writer_mut())).into_owned()
}

#[test]
fn a_terminal_that_draws_pictures_gets_the_mascot_in_its_cells() {
    let (mut ui, t0) = ui();
    ui.use_pictures(Some(Graphics::Kitty), Some((9, 21)));
    assert!(ui.theme().mascot_pictures);
    ui.model_mut().history.push(header("orchestrator", None));
    ui.frame(t0).unwrap();
    let sent = written(&mut ui);
    // History prints from the top: the header's blank row, then the mascot's first row, at
    // column 1.
    let at = sent.find("\x1b_Ga=T").expect("the picture");
    assert!(
        sent[..at].ends_with("\x1b[2;2H"),
        "{:?}",
        &sent[at.saturating_sub(20)..at]
    );
    assert_eq!(
        pictures_in(sent.as_bytes()),
        [picture(Mascot::Medusa, Look::Normal, (9, 21), 0)
            .unwrap()
            .into_owned()]
    );
    // The header's own cells stay blank under it.
    assert!(!sent.contains('█'), "{sent:?}");
    // A still look has nothing to swap.
    assert_eq!(ui.next_mascot_frame(t0), None);
}

#[test]
fn a_new_look_replaces_the_picture_and_frees_the_old_one() {
    let (mut ui, t0) = ui();
    ui.use_pictures(Some(Graphics::Kitty), Some((9, 21)));
    ui.model_mut().history.push(header("council", None));
    ui.frame(t0).unwrap();
    written(&mut ui);
    assert!(
        ui.model_mut()
            .history
            .replace_header(header("council", Some(UltraColour::Blue)))
    );
    let t1 = t0 + Duration::from_millis(50);
    ui.frame(t1).unwrap();
    let sent = written(&mut ui);
    assert!(sent.contains("a=d,d=I,q=2,i="), "the old picture is freed");
    // Its first frame and the one after it; the rest go as they come due.
    assert_eq!(pictures_in(sent.as_bytes()).len(), 2);
    // Its frames now swap on the Hydra's beat while the header shows.
    let next = ui.next_mascot_frame(t1).expect("the next frame");
    assert_eq!(next, t1 + Mascot::Hydra.frame_time());
    ui.frame(next).unwrap();
    let swapped = written(&mut ui);
    assert!(
        swapped.contains("\x1b_Ga=p,q=2,C=1,c=9,r=3,i="),
        "{swapped:?}"
    );
    assert!(swapped.contains("a=d,d=i,q=2"), "the frame before leaves");
}

#[test]
fn block_cells_sweep_their_light_until_the_header_scrolls_away() {
    let (mut ui, t0) = ui();
    ui.model_mut()
        .history
        .push(header("captain", Some(UltraColour::Purple)));
    ui.frame(t0).unwrap();
    written(&mut ui);
    let step = Mascot::Argonaut.frame_time();
    let next = ui.next_mascot_frame(t0).expect("an ultra look moves");
    assert_eq!(next, t0 + step);
    // Halfway through the cycle the light is on the body: the header's rows print again.
    let half = t0 + step * (Mascot::Argonaut.frames() / 2) as u32;
    ui.frame(half).unwrap();
    let redrawn = written(&mut ui);
    assert!(redrawn.contains("Argonauts Code"), "{redrawn:?}");
    assert!(redrawn.contains("215;194;255"), "the ultracode shimmer");
    // Once the header goes up into scrollback nothing is drawn again.
    for line in 0..ROWS {
        ui.model_mut().history.push(crate::cell::Notice::new(
            crate::cell::Tone::Normal,
            format!("line {line}"),
        ));
    }
    ui.frame(half + step).unwrap();
    assert_eq!(ui.next_mascot_frame(half + step), None);
}

#[test]
fn reduced_motion_keeps_the_mascot_still_and_ui_mascot_blocks_keeps_it_in_blocks() {
    let (mut ui, t0) = ui();
    let mut still = ui.theme().clone();
    still.reduced_motion = true;
    ui.set_theme(still);
    ui.model_mut()
        .history
        .push(header("captain", Some(UltraColour::Purple)));
    ui.frame(t0).unwrap();
    assert_eq!(ui.next_mascot_frame(t0), None);

    let (mut ui, _) = self::ui();
    ui.use_pictures(Some(Graphics::Kitty), Some((9, 21)));
    let mut blocks = ui.theme().clone();
    blocks.mascot = MascotDrawing::Blocks;
    let (mut inline, _) = self::ui();
    inline.use_pictures(Some(Graphics::Kitty), Some((9, 21)));
    inline.set_theme(blocks);
    assert!(!inline.theme().mascot_pictures);
}

fn full_screen() -> (Ui<Vec<u8>>, Instant) {
    let (mut ui, t0) = ui();
    ui.use_fullscreen();
    (ui, t0)
}

/// Where the picture was last placed from: the cursor move, `CSI row;column H`, last before
/// its `a=p`.
fn placed_at(sent: &str) -> Option<&str> {
    let at = sent.rfind("\x1b_Ga=p")?;
    let start = sent[..at].rfind("\x1b[")?;
    let end = start + sent[start..].find('H')? + 1;
    Some(&sent[start..end])
}

/// The full screen draws its history again each frame, so it places the picture in the
/// header's cells as it draws: the frames are kept once, then the picture shows from the
/// mascot's first cell.
#[test]
fn the_full_screen_places_the_picture_in_the_headers_cells() {
    let (mut ui, t0) = full_screen();
    ui.use_pictures(Some(Graphics::Kitty), Some((9, 21)));
    assert!(ui.theme().mascot_pictures);
    ui.model_mut().history.push(header("orchestrator", None));
    ui.frame(t0).unwrap();
    let sent = written(&mut ui);
    assert!(sent.contains("\x1b_Ga=t,f=100,q=2,i="), "{sent:?}");
    assert_eq!(placed_at(&sent), Some("\x1b[2;2H"));
    assert!(sent.contains("\x1b_Ga=p,q=2,C=1,c=9,r=3,i=1296105472\x1b\\"));
    assert!(
        !sent.contains('█'),
        "the header's own cells stay blank under it"
    );
    // A frame that moves nothing sends nothing more.
    ui.dirty = true;
    ui.frame(t0 + Duration::from_millis(100)).unwrap();
    assert!(!written(&mut ui).contains("\x1b_G"));
}

/// The picture goes off while the header is scrolled out of view, and back into its cells
/// when you scroll up to it.
#[test]
fn on_the_full_screen_the_picture_follows_the_header_out_of_view_and_back() {
    let (mut ui, t0) = full_screen();
    ui.use_pictures(Some(Graphics::Kitty), Some((9, 21)));
    ui.model_mut().history.push(header("council", None));
    ui.frame(t0).unwrap();
    written(&mut ui);
    for line in 0..ROWS * 2 {
        ui.model_mut().history.push(crate::cell::Notice::new(
            crate::cell::Tone::Normal,
            format!("line {line}"),
        ));
    }
    ui.frame(t0 + Duration::from_millis(50)).unwrap();
    let sent = written(&mut ui);
    assert!(
        sent.contains("\x1b_Ga=d,d=i,q=2,i=1296105472\x1b\\"),
        "{sent:?}"
    );
    assert!(!sent.contains("a=p"), "{sent:?}");
    // Scrolled up as far as it goes, the header is on top again.
    ui.scrollback
        .as_mut()
        .expect("the full screen")
        .scroll(isize::try_from(ROWS).unwrap() * 4);
    ui.dirty = true;
    ui.frame(t0 + Duration::from_millis(100)).unwrap();
    let sent = written(&mut ui);
    assert_eq!(placed_at(&sent), Some("\x1b[2;2H"), "{sent:?}");
}

/// An ultra look's frames are kept as they come due and swapped by id while the header shows,
/// and block cells sweep their light on the full screen as they do inline.
#[test]
fn on_the_full_screen_an_ultra_look_moves_picture_or_blocks() {
    let (mut ui, t0) = full_screen();
    ui.use_pictures(Some(Graphics::Kitty), Some((9, 21)));
    ui.model_mut()
        .history
        .push(header("captain", Some(UltraColour::Purple)));
    ui.frame(t0).unwrap();
    let sent = written(&mut ui);
    // The first frame and the one after it are kept; the rest go as they come due.
    assert_eq!(sent.matches("a=t,f=100,q=2,i=").count(), 2);
    let next = ui.next_mascot_frame(t0).expect("an ultra look moves");
    ui.frame(next).unwrap();
    let swapped = written(&mut ui);
    assert!(
        swapped
            .contains("\x1b_Ga=d,d=i,q=2,i=1296105472\x1b\\\x1b_Ga=p,q=2,C=1,c=9,r=3,i=1296105473"),
        "{swapped:?}"
    );

    let (mut ui, t0) = full_screen();
    ui.model_mut()
        .history
        .push(header("captain", Some(UltraColour::Purple)));
    ui.frame(t0).unwrap();
    written(&mut ui);
    let step = Mascot::Argonaut.frame_time();
    assert_eq!(ui.next_mascot_frame(t0), Some(t0 + step));
    ui.frame(t0 + step * (Mascot::Argonaut.frames() / 2) as u32)
        .unwrap();
    let redrawn = written(&mut ui);
    assert!(
        redrawn.contains("215;194;255"),
        "the ultracode shimmer: {redrawn:?}"
    );
}

/// On the full screen a frame that looks like the one shown places nothing again: iTerm2 gets
/// at most its 18 pictures a cycle, and the kitty protocol never puts the picture already
/// showing back in its place.
#[test]
fn the_full_screen_sends_a_picture_only_when_another_is_due() {
    for (workflow, mascot) in [
        ("orchestrator", Mascot::Medusa),
        ("captain", Mascot::Argonaut),
    ] {
        for graphics in [Graphics::Iterm, Graphics::Kitty] {
            let (mut ui, t0) = full_screen();
            ui.use_pictures(Some(graphics), Some((9, 21)));
            ui.model_mut()
                .history
                .push(header(workflow, Some(UltraColour::Purple)));
            ui.frame(t0).unwrap();
            written(&mut ui);
            let (mut pngs, mut again) = (0, 0);
            let mut shown: Option<String> = None;
            let mut now = t0;
            for _ in 0..mascot.frames() {
                let Some(next) = ui.next_mascot_frame(now) else {
                    break;
                };
                now = next;
                ui.frame(now).unwrap();
                let sent = written(&mut ui);
                pngs += pictures_in(sent.as_bytes()).len();
                for (at, _) in sent.match_indices("\x1b_Ga=p,") {
                    let id = sent[at..]
                        .split("i=")
                        .nth(1)
                        .and_then(|rest| rest.split('\x1b').next())
                        .map(str::to_owned);
                    if id.is_some() && id == shown {
                        again += 1;
                    }
                    shown = id;
                }
            }
            match graphics {
                Graphics::Iterm => assert!(pngs <= 18, "{workflow}: {pngs} pictures a cycle"),
                Graphics::Kitty => assert_eq!(again, 0, "{workflow}: placed what showed"),
            }
        }
    }
}

/// Narrower than the header needs for its mascot, the header's words start where the mascot
/// would, so no picture goes over them. Inline, a resize that takes the room away forgets the
/// picture; on the full screen, which draws the header again at each width, it also brings the
/// picture back once there's room.
#[test]
fn a_header_too_narrow_for_its_mascot_gets_no_picture() {
    let narrow = u16::try_from(crate::cell::MARK_MIN_WIDTH - 2).unwrap();
    let (mut inline, t0) = ui_at(narrow);
    inline.use_pictures(Some(Graphics::Kitty), Some((9, 21)));
    inline
        .model_mut()
        .history
        .push(header("orchestrator", None));
    inline.frame(t0).unwrap();
    let sent = written(&mut inline);
    assert!(!sent.contains("\x1b_G"), "{sent:?}");
    assert!(sent.contains("Argonauts"), "{sent:?}");

    let (mut inline, t0) = ui();
    inline.use_pictures(Some(Graphics::Kitty), Some((9, 21)));
    inline
        .model_mut()
        .history
        .push(header("orchestrator", None));
    inline.frame(t0).unwrap();
    assert!(written(&mut inline).contains("\x1b_Ga=T"));
    inline.resize(Size::new(narrow, ROWS), None);
    inline.frame(t0 + Duration::from_millis(50)).unwrap();
    assert!(written(&mut inline).contains("a=d,d=I,q=2,i="));

    let (mut full, t0) = ui_at(narrow);
    full.use_fullscreen();
    full.use_pictures(Some(Graphics::Kitty), Some((9, 21)));
    full.model_mut().history.push(header("orchestrator", None));
    full.frame(t0).unwrap();
    let sent = written(&mut full);
    assert!(!sent.contains("\x1b_G"), "{sent:?}");

    full.resize(Size::new(COLUMNS, ROWS), None);
    full.frame(t0 + Duration::from_millis(50)).unwrap();
    let sent = written(&mut full);
    assert!(sent.contains("\x1b_Ga=t,f=100,q=2,i="), "{sent:?}");
    assert_eq!(placed_at(&sent), Some("\x1b[2;2H"), "{sent:?}");

    full.resize(Size::new(narrow, ROWS), None);
    full.frame(t0 + Duration::from_millis(100)).unwrap();
    let sent = written(&mut full);
    assert!(sent.contains("a=d,d=I,q=2,i="), "{sent:?}");
    assert!(!sent.contains("a=p"), "{sent:?}");
}

fn greeting() -> Cell {
    Cell::Custom(Box::new(crate::welcome::Greeting {
        version: "0.1.0".into(),
        usage: None,
        picture: true,
    }))
}

/// Before any header prints, the greeting's argonaut goes out as a picture in the cells the
/// header's mascot takes. The header that prints after setup takes the mark, and inline the
/// greeting's picture stays up in scrollback with its rows.
#[test]
fn the_greeting_gets_the_argonaut_as_a_picture_until_the_header_prints() {
    let (mut ui, t0) = ui();
    ui.use_pictures(Some(Graphics::Kitty), Some((9, 21)));
    ui.model_mut().history.push(greeting());
    ui.frame(t0).unwrap();
    let sent = written(&mut ui);
    let at = sent.find("\x1b_Ga=T").expect("the picture");
    assert!(
        sent[..at].ends_with("\x1b[2;2H"),
        "{:?}",
        &sent[at.saturating_sub(20)..at]
    );
    assert_eq!(
        pictures_in(sent.as_bytes()),
        [
            picture(Mascot::Argonaut, Look::new(None, false), (9, 21), 0)
                .unwrap()
                .into_owned()
        ]
    );

    ui.model_mut().history.push(header("orchestrator", None));
    ui.frame(t0 + Duration::from_millis(50)).unwrap();
    let sent = written(&mut ui);
    assert!(
        !sent.contains("a=d,d=I"),
        "the greeting's picture stays: {sent:?}"
    );
    assert_eq!(
        pictures_in(sent.as_bytes()),
        [picture(Mascot::Medusa, Look::Normal, (9, 21), 0)
            .unwrap()
            .into_owned()]
    );
}

/// The full screen places the greeting's picture in its cells as it draws them, and a
/// greeting too narrow for the picture keeps the argonaut's own cells.
#[test]
fn the_greeting_gets_its_picture_on_the_full_screen_and_none_when_narrow() {
    let (mut full, t0) = full_screen();
    full.use_pictures(Some(Graphics::Kitty), Some((9, 21)));
    full.model_mut().history.push(greeting());
    full.frame(t0).unwrap();
    let sent = written(&mut full);
    assert!(sent.contains("\x1b_Ga=t,f=100,q=2,i="), "{sent:?}");
    assert_eq!(placed_at(&sent), Some("\x1b[2;2H"));

    let narrow = u16::try_from(crate::cell::MARK_MIN_WIDTH - 2).unwrap();
    let (mut inline, t0) = ui_at(narrow);
    inline.use_pictures(Some(Graphics::Kitty), Some((9, 21)));
    inline.model_mut().history.push(greeting());
    inline.frame(t0).unwrap();
    let sent = written(&mut inline);
    assert!(!sent.contains("\x1b_G"), "{sent:?}");
    assert!(sent.contains('▗'), "the argonaut's own cells: {sent:?}");
}
