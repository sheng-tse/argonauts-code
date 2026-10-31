//! The council's screens, cell for cell against `tests/golden/council/`: the conversation the
//! council prints, as the history prints it, then the live typing row under one blank row. A
//! screen's other rows (the working line, the input, the rows under the bottom line) belong to
//! their own goldens.
//!
//! A screen with bubbles is drawn as a terminal that draws pictures draws it, logos as pictures
//! too, which its golden writes as `▐▌` and a numbered logo as `▐1`. The transcript's screens
//! are drawn without pictures, and their goldens hold the cells as they are: `●` for a logo and
//! `①` for a numbered seat.
use std::time::Instant;

use argonauts_core::base::names::SeatId;
use ratatui::style::Modifier;
use ratatui::text::Line;

use crate::HistoryCell;
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::theme::{Role, Theme};
use crate::council::tests::story::{self, *};
use crate::council::{Look, Mark, Spacing, TALKING, TIP, TalkCell, WORKING};

use crate::council::tests::golden::*;

#[test]
fn sent_shows_the_header_your_message_and_four_seats_typing() {
    let items = [header(), line("Today 10:41 AM"), prompt(), alone_first()];
    let mut rows = printed(&items, Look::Pictures, 120);
    // A fresh conversation fills down from under the header, so a blank row more waits between
    // the centred line and the typing row.
    rows.push(String::new());
    let rows = with_typing(rows, typing(&all(), &[], Look::Pictures, 120));
    assert_screen("sent", &rows, 23);
}

/// Over the council, the screen the council opens on starts as every conversation does: the
/// session's header, then the command that switched to the council and where that saved.
#[test]
fn the_council_opens_under_the_header_and_the_command_that_switched_to_it() {
    use argonauts_core::app::AppEvent;
    use argonauts_core::settings::SettingScope;

    use crate::app::UiAction;
    use crate::base::theme::Glyphs;
    use crate::cells::cell::SessionHeader;

    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let ctx = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
    let header = SessionHeader {
        app: "Argonauts Code".into(),
        version: "0.2.0".into(),
        main_agent: Some("Claude Code".into()),
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        workflow: "council".into(),
        ultra: None,
        folder: "~/code/shop-api".into(),
        agents: Vec::new(),
        runs_going: 0,
        runs_need_you: 0,
        way_back: None,
    };
    let mut rows: Vec<String> = HistoryCell::lines(&header, &ctx).iter().map(text).collect();
    let mut bridge = crate::bridge::Bridge::new(&crate::UiOptions::default());
    let mut model = crate::app::model::ViewModel::default();
    let typed = UiAction::Submit {
        text: "/council".into(),
        shown: "/council".into(),
    };
    bridge.on_action(&mut model, &typed, Instant::now());
    let switched = AppEvent::SettingChanged {
        key: "workflow".into(),
        value: "\"council\"".into(),
        scope: SettingScope::Global,
        previous: None,
        reset: false,
        why: None,
        kept: false,
    };
    bridge.on_event(&mut model, &switched, Instant::now());
    rows.extend(model.history.drain_printable(&ctx).iter().map(text));
    assert_rows_from("sent", &rows, 0);
}

#[test]
fn first_answers_fold_after_their_opening_and_two_points() {
    let mut items = vec![prompt(), alone_first()];
    items.extend(first_answers());
    items.push(everyone_answered());
    let rows = with_typing(
        printed(&items, Look::Pictures, 120),
        typing(&[opus()], &[], Look::Pictures, 120),
    );
    assert_screen("first_answers", &rows, 24);
}

/// The note keeper's first list, right after its notes, draws from the engine's own events as
/// the proposal golden draws it, with the line under it.
#[test]
fn the_note_keepers_first_list_draws_as_the_proposal() {
    use argonauts_core::base::names::MessageId;
    use argonauts_core::runs::events::council::{
        CouncilEventKind, CouncilMessage, MessageKind, Point, PointState, Seat, SeatRole, Seating,
        Side, Speaker, TextPart, held_seat,
    };

    let seating = Seating {
        seats: all()
            .into_iter()
            .map(|id| Seat {
                id,
                model: "claude-code/claude-opus-5-5".parse().unwrap(),
                role: SeatRole::Proposer,
                number: None,
            })
            .collect(),
        notes: Some(opus()),
    };
    let mut council = crate::council::Conversation::new(&seating, story::faces());
    let point = |number: u32, words: String, state, sides: Vec<Side>| Point {
        number,
        words,
        state,
        sides,
        raised_by: None,
    };
    let side = |words: &str, backers: &[SeatId]| Side {
        words: words.to_owned(),
        backers: backers.to_vec(),
        reason: String::new(),
    };
    let yours = vec![
        point(
            1,
            "the limiter by Maker-checker".into(),
            PointState::Yours,
            vec![],
        ),
        point(
            2,
            "the route tests across agents".into(),
            PointState::Yours,
            vec![],
        ),
        point(
            3,
            format!(
                "the plan limits by {} in its own session",
                held_seat(&luna())
            ),
            PointState::Yours,
            vec![],
        ),
    ];
    council.on(
        &CouncilEventKind::Points {
            points: yours.clone(),
        },
        None,
    );
    let message = |n: u32, seat: SeatId, kind, words: &str| CouncilMessage {
        id: MessageId::numbered(n),
        from: Speaker::Seat(seat),
        kind,
        text: vec![TextPart::Words(words.to_owned())],
        reply_to: None,
        mentions: Vec::new(),
        stances: Vec::new(),
        hidden: 0,
        hidden_names: Vec::new(),
        guesses_hidden: 0,
        rewritten: false,
        declined: false,
        started_before: None,
    };
    for (n, seat) in all().into_iter().enumerate() {
        let first = message(
            n as u32 + 1,
            seat,
            MessageKind::FirstAnswer,
            "A first answer.",
        );
        council.on(&CouncilEventKind::Message(Box::new(first)), None);
    }
    let notes = message(
        5,
        opus(),
        MessageKind::Notes,
        "Where our four answers meet: 3 points agreed, 3 still open.",
    );
    council.on(&CouncilEventKind::Message(Box::new(notes)), None);
    let open = PointState::Open;
    let mut listed = yours;
    listed.extend([
        point(
            4,
            "Limits from the key's plan: free 60 a minute, pro 600; importer 2,000".into(),
            open,
            vec![],
        ),
        point(
            5,
            "An empty bucket answers 429 with Retry-After and X-RateLimit headers".into(),
            open,
            vec![],
        ),
        point(
            6,
            "Tests use an in-memory store behind a RateStore interface".into(),
            open,
            vec![],
        ),
        point(
            7,
            "The limit's shape".into(),
            open,
            vec![
                side("A token bucket", &[opus(), flash(), qwen()]),
                side("a window counter", &[luna()]),
            ],
        ),
        point(
            8,
            "Where counters live".into(),
            open,
            vec![
                side("Kept in Redis", &[opus(), luna(), flash()]),
                side("in memory on each node", &[qwen()]),
            ],
        ),
        point(
            9,
            "Calls with no key".into(),
            open,
            vec![
                side("Calls with no key: one shared bucket", &[opus(), flash()]),
                side("401", &[luna(), qwen()]),
            ],
        ),
    ]);
    let said = council.on(&CouncilEventKind::Points { points: listed }, None);
    assert_eq!(said.changed, Some(MessageId::numbered(5)));
    let mut items = vec![council.item(&MessageId::numbered(5)).expect("the notes")];
    items.extend(said.items);
    assert_eq!(
        printed(&items, Look::Pictures, 120),
        printed(&[proposal(), talk_until_agreed()], Look::Pictures, 120)
    );
}

#[test]
fn the_proposal_sits_inset_in_the_note_keepers_bubble() {
    let mut items = first_answers();
    items.push(everyone_answered());
    items.push(proposal());
    items.push(talk_until_agreed());
    items.push(luna_on_buckets(true));
    let rows = with_typing(
        printed(&items, Look::Pictures, 120),
        typing(
            &[opus()],
            &reason("{opus} is writing · {luna} replied to its message"),
            Look::Pictures,
            120,
        ),
    );
    assert_screen("proposal", &rows, 23);
}

/// A point too long for one row wraps inside the bubble under its own words, and an open
/// point's other way goes on the row under it; at a width where it fits, it keeps one row.
#[test]
fn long_points_wrap_inside_the_proposal_bubble() {
    use crate::council::Piece;
    use crate::council::Tone;
    use crate::council::talk::item::{Inset, Item, ProposalPoint};

    const FIRST: &str = "One TokenBucket per API key, kept in a dict on the limiter and made on \
                         the key's first request";
    const LOCK: &str = "Look the key's bucket up inside allow(), under the limiter's lock";
    const MIDDLEWARE: &str = "fetch or instantiate a TokenBucket in the middleware";
    let way = |words: &str, seats: &[SeatId]| {
        let mut way = vec![Piece::text(words)];
        for seat in seats {
            way.push(Piece::dim(" "));
            way.push(Piece::Badge(seat.clone()));
        }
        way
    };
    let mut message = says(
        opus(),
        &["Where our four answers meet: 1 point agreed, 2 still open."],
    );
    message.inset = Some(Inset::Proposal {
        head: vec![
            Piece::tone("Proposal", Tone::Bold),
            Piece::dim(" · 3 points from 4 answers"),
        ],
        points: vec![
            ProposalPoint {
                mark: Piece::tone("✔  ", Tone::Ok),
                ways: vec![way(FIRST, &[])],
            },
            ProposalPoint {
                mark: Piece::tone("?  ", Tone::WarnBold),
                ways: vec![
                    way(LOCK, &[opus(), qwen()]),
                    way(MIDDLEWARE, &[luna(), flash()]),
                ],
            },
            ProposalPoint {
                mark: Piece::tone("?  ", Tone::WarnBold),
                ways: vec![
                    way("Drop a bucket after 10 idle minutes", &[opus(), flash()]),
                    way(
                        "keep every bucket for the life of the process",
                        &[luna(), qwen()],
                    ),
                ],
            },
        ],
        yours: Vec::new(),
    });
    let items = [Item::Seat(vec![message])];
    let room = crate::council::draw::bubbles::words_room(120);
    let narrow = printed(&items, Look::Pictures, 120);
    let row_of = |rows: &[String], words: &str| {
        rows.iter()
            .position(|row| row.contains(words))
            .unwrap_or_else(|| panic!("no row holds {words:?}:\n{}", rows.join("\n")))
    };
    for row in &narrow {
        let used = row.trim_end();
        assert!(
            crate::base::text::str_width(used) <= room + 10,
            "a row runs past the bubble:\n{}",
            narrow.join("\n")
        );
    }
    let first = row_of(&narrow, "✔  One TokenBucket");
    assert_eq!(row_of(&narrow, "first request"), first + 1);
    let lock = row_of(&narrow, "?  Look the key's bucket");
    let other = row_of(&narrow, "or  fetch or instantiate");
    assert_eq!(other, lock + 1, "{}", narrow.join("\n"));
    let at = |row: &str, words: &str| {
        crate::base::text::str_width(&row[..row.find(words).expect("the words")])
    };
    assert_eq!(
        at(&narrow[other], "or  fetch"),
        at(&narrow[lock], "Look"),
        "the other way starts under the first:\n{}",
        narrow.join("\n")
    );

    let idle = row_of(&narrow, "?  Drop a bucket");
    assert_eq!(row_of(&narrow, "or  keep every bucket"), idle + 1);

    let wide = printed(&items, Look::Pictures, 177);
    let first = row_of(&wide, "✔  One TokenBucket");
    assert!(wide[first].contains("first request"), "{}", wide.join("\n"));
    let idle = row_of(&wide, "?  Drop a bucket");
    assert!(
        wide[idle].contains("   or   keep every bucket"),
        "{}",
        wide.join("\n")
    );
}

#[test]
fn the_talk_quotes_what_each_reply_answers_and_shows_tapbacks() {
    let rows = with_typing(
        printed(&talk(true, false), Look::Pictures, 120),
        typing(&[qwen()], &reason(QWEN_WRITES), Look::Pictures, 120),
    );
    assert_screen("talk", &rows, 23);
}

#[test]
fn a_picked_message_keeps_its_words_where_they_were() {
    let rows = with_typing(
        printed(&talk(true, true), Look::Pictures, 120),
        typing(&[qwen()], &reason(QWEN_WRITES), Look::Pictures, 120),
    );
    assert_screen("you_reply", &rows, 22);
}

#[test]
fn your_reply_mid_talk_says_who_reads_it_and_when() {
    let mut items = talk(true, false);
    items.push(story::your_reply(false));
    let rows = with_typing(
        printed(&items, Look::Pictures, 120),
        typing(
            &[qwen(), flash()],
            &reason("{flash} is answering you · {qwen} finishes first, then reads yours"),
            Look::Pictures,
            120,
        ),
    );
    assert_screen("you_mid_talk", &rows, 23);
}

#[test]
fn everyone_agreed_ends_the_talk_with_a_centred_line() {
    let mut items = talk(true, false);
    items.push(story::your_reply(true));
    items.push(qwen_agrees(&[
        (opus(), Mark::Object),
        (luna(), Mark::Agree),
        (flash(), Mark::Object),
    ]));
    items.push(qwen_moved());
    items.push(flash_on_deprecation(3));
    items.push(everyone_agreed());
    let rows = with_typing(
        printed(&items, Look::Pictures, 120),
        typing(
            &[opus()],
            &reason("{opus} keeps the notes: it writes the plan and who does what"),
            Look::Pictures,
            120,
        ),
    );
    assert_screen("everyone_agreed", &rows, 24);
}

#[test]
fn the_talk_at_80_columns_wraps_inside_narrower_bubbles() {
    let rows = with_typing(
        printed(&talk(false, false), Look::Pictures, 80),
        typing(
            &[qwen()],
            &reason("{flash} replied to it"),
            Look::Pictures,
            80,
        ),
    );
    assert_screen("talk_at_80_columns", &rows, 23);
}

#[test]
fn without_pictures_the_talk_is_a_transcript() {
    let rows = with_typing(
        printed(&talk(true, false), Look::Transcript, 120),
        typing(&[qwen()], &reason(QWEN_WRITES), Look::Transcript, 120),
    );
    assert_screen("transcript", &rows, 23);
}

#[test]
fn block_bubbles_draw_their_corners_in_cells() {
    let rows = with_typing(
        printed(&talk(true, false), Look::Blocks, 120),
        typing(&[qwen()], &reason(QWEN_WRITES), Look::Blocks, 120),
    );
    assert_screen("blocks", &rows, 23);
}

const LUNA_ASKED: &str = "{luna} is writing · {opus_one} asked it";

#[test]
fn a_model_that_sits_twice_shows_each_seat_by_its_number() {
    let faces = story::faces_one_model_twice();
    let rows = with_typing(
        printed_with(&one_model_twice_talk(true), Look::Pictures, 120, &faces),
        typing_with(&[luna()], &reason(LUNA_ASKED), Look::Pictures, 120, &faces),
    );
    assert_screen("two_seats_one_model", &rows, 23);
}

#[test]
fn a_model_that_sits_twice_keeps_its_numbers_in_the_transcript() {
    let faces = story::faces_one_model_twice();
    let rows = with_typing(
        printed_with(&one_model_twice_talk(false), Look::Transcript, 120, &faces),
        typing_with(
            &[luna()],
            &reason(LUNA_ASKED),
            Look::Transcript,
            120,
            &faces,
        ),
    );
    assert_screen("two_seats_one_model_transcript", &rows, 23);
}

#[test]
fn a_move_from_a_private_message_says_so_and_keeps_your_words() {
    let faces = story::faces_one_model_twice();
    let rows = with_typing(
        printed_with(&story::moved_in_private(), Look::Pictures, 120, &faces),
        typing_with(
            &[opus_two()],
            &reason("{opus_two} is writing · point 6 would close on a reason no seat can read"),
            Look::Pictures,
            120,
            &faces,
        ),
    );
    assert_screen("moved_in_private", &rows, 23);
}

#[test]
fn a_held_message_fades_and_marks_the_words_that_match() {
    let faces = story::faces_one_model_twice();
    let rows = printed_with(&story::held(), Look::Pictures, 120, &faces);
    assert_screen("held_message", &rows, 13);
}

#[test]
fn names_are_dim_mentions_bold_in_their_cli_colour_and_marks_in_their_state() {
    let theme = Theme::dark();
    let faces = story::faces();
    let ctx = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
    let mut spacing = Spacing::default();
    let mut lines = Vec::new();
    let mut items = talk(true, false);
    items.push(story::your_reply(false));
    for item in items {
        lines.extend(TalkCell::new(item, Look::Pictures, faces.clone(), &mut spacing).lines(&ctx));
    }
    assert_eq!(
        style_of(&lines, "Opus 5.5 · keeps").fg,
        theme.style(Role::Dim).fg
    );
    let mention = style_of(&lines, "3.8 Flash's");
    assert_eq!(mention.fg, theme.style(Role::CliAgy).fg);
    assert!(mention.add_modifier.contains(Modifier::BOLD));
    assert_eq!(style_of(&lines, "✘").fg, theme.style(Role::Danger).fg);
    assert_eq!(style_of(&lines, "✔").fg, theme.style(Role::Success).fg);
    let quote = style_of(&lines, "A token bucket per API key");
    assert!(quote.add_modifier.contains(Modifier::ITALIC));
    assert_eq!(
        style_of(&lines, "Calls with no key: don't").fg,
        Some(crate::council::talk::words::ON_YOUR_BUBBLE)
    );
}

#[test]
fn a_held_message_marks_its_note_and_matching_words_amber() {
    let theme = Theme::dark();
    let faces = story::faces_one_model_twice();
    let ctx = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
    let mut spacing = Spacing::default();
    let lines: Vec<Line<'static>> = story::held()
        .into_iter()
        .flat_map(|item| {
            TalkCell::new(item, Look::Pictures, faces.clone(), &mut spacing).lines(&ctx)
        })
        .collect();
    let warn = theme.style(Role::Warn).fg;
    assert_eq!(style_of(&lines, " · held back").fg, warn);
    assert_eq!(style_of(&lines, "contract promises").fg, warn);
    assert_eq!(style_of(&lines, "A customer").fg, theme.style(Role::Dim).fg);
}

#[test]
fn the_plan_and_who_does_what_come_from_the_note_keeper_in_a_row() {
    let items = [
        flash_on_deprecation(3),
        everyone_agreed(),
        story::plan_and_who(),
        story::signed(),
    ];
    assert_screen("plan", &printed(&items, Look::Pictures, 120), 16);
}

#[test]
fn approve_with_edits_keeps_the_plan_above_its_sheet() {
    let items = [story::plan_and_who(), story::signed()];
    assert_screen(
        "approve_with_edits",
        &printed(&items, Look::Pictures, 120),
        11,
    );
}

#[test]
fn the_work_starts_with_a_message_per_task() {
    let rows = printed(&story::work_starts(None), Look::Pictures, 120);
    assert_screen("work_starts", &rows, 24);
}

#[test]
fn only_the_picked_task_says_enter_opens_it() {
    let rows = printed(&story::work_starts(Some(flash())), Look::Pictures, 120);
    assert_screen("task_picked", &rows, 24);
}

#[test]
fn the_work_nests_and_a_finished_task_is_checked() {
    let rows = printed(&story::work_nests(), Look::Pictures, 120);
    assert_screen("work_nests", &rows, 19);
}

#[test]
fn the_work_at_80_columns_drops_what_the_tasks_can_do_without() {
    let rows = printed(&story::work_nests(), Look::Pictures, 80);
    assert_screen("work_at_80_columns", &rows, 19);
}

#[test]
fn a_picked_task_in_the_transcript_says_enter_opens_it() {
    let rows = printed(&story::work_starts(Some(flash())), Look::Transcript, 120);
    assert_screen("transcript_task_picked", &rows, 24);
}

#[test]
fn a_blind_check_says_what_the_checker_read_and_never_who_made_it() {
    let rows = printed_with(
        &story::blind_check(),
        Look::Pictures,
        120,
        &story::faces_one_model_twice(),
    );
    assert_screen("blind_check", &rows, 23);
}

#[test]
fn a_same_maker_check_says_so() {
    let rows = printed_with(
        &story::same_maker_check(),
        Look::Pictures,
        120,
        &story::faces_one_maker(),
    );
    assert_screen("same_maker_check", &rows, 23);
}

#[test]
fn a_talk_that_moves_nobody_stops_and_says_why() {
    let rows = printed(&story::no_agreement(), Look::Pictures, 120);
    assert_screen("no_agreement", &rows, 15);
}

#[test]
fn the_edit_sheet_moves_a_way_and_keeps_an_objection_beside_its_task() {
    let theme = theme_for(Look::Pictures);
    let rows: Vec<String> = story::edit_sheet()
        .lines(120, &story::faces(), &theme)
        .iter()
        .map(text)
        .collect();
    assert_eq!(rows.len(), 14);
    assert_rows_from("approve_with_edits", &rows, 13);
}

fn screen_rows(lines: &[Line<'_>]) -> Vec<String> {
    lines.iter().map(text).collect()
}

#[test]
fn a_thread_shows_one_message_and_its_replies_alone() {
    let theme = theme_for(Look::Pictures);
    let lines = story::thread().lines(120, 38, &story::faces(), &theme);
    assert_eq!(lines.len(), 38);
    assert_rows_from("thread", &screen_rows(&lines), 0);
}

#[test]
fn a_seats_session_shows_what_it_read_and_takes_private_words() {
    let theme = theme_for(Look::Pictures);
    let lines = story::luna_session().lines(120, 38, &story::faces(), &theme);
    assert_eq!(lines.len(), 38);
    assert_rows_from("seat_session", &screen_rows(&lines), 0);
}

#[test]
fn the_note_keepers_session_reads_every_seat_by_its_letter() {
    let theme = theme_for(Look::Pictures);
    let lines = story::notes_session().lines(120, 38, &story::faces_one_model_twice(), &theme);
    assert_rows_from("what_a_seat_reads", &screen_rows(&lines), 0);
}

#[test]
fn a_private_message_sits_in_the_seats_own_session() {
    let theme = theme_for(Look::Pictures);
    let lines = story::private_session().lines(120, 38, &story::faces_one_model_twice(), &theme);
    assert_rows_from("private_message", &screen_rows(&lines), 0);
}

#[test]
fn the_reply_row_names_who_you_answer_and_cuts_the_quote() {
    let theme = theme_for(Look::Pictures);
    let bar = crate::council::screens::reply::ReplyBar {
        message: argonauts_core::base::names::MessageId::numbered(4),
        face: Some(story::faces().face(&flash())),
        quote:
            "And the balancer's probe calls /health with no key, so a plain 401 takes a node out."
                .to_owned(),
    };
    let (line, shapes) = bar.line(120, &theme);
    assert_rows_from("you_reply", &[text(&line)], 27);
    assert_eq!(shapes.len(), 1);

    // It sits right above the input's rule.
    let bottom = crate::bottom::BottomState {
        replying: Some(bar),
        ..crate::bottom::BottomState::default()
    };
    let ctx = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
    let rows = screen_rows(&bottom.render_live(&ctx, 20, false).lines);
    let at = rows
        .iter()
        .position(|row| row.starts_with("  Replying to"))
        .expect("the reply row");
    assert!(rows[at + 1].starts_with("───"), "{rows:?}");
}

/// With `ARGONAUTS_PICTURE_DUMP` set, writes the bubbles under the talk's screen as a PNG of the
/// screen's size, to put beside the screen as the terminal shows it.
#[test]
fn the_talks_bubbles_can_be_dumped_to_look_at() {
    let Some(dir) = std::env::var_os("ARGONAUTS_PICTURE_DUMP") else {
        return;
    };
    let theme = Theme::dark();
    let faces = story::faces();
    let ctx = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
    let mut spacing = Spacing::default();
    let mut row = 0usize;
    let mut placed: Vec<(usize, super::super::base::draw::picture::Picture)> = Vec::new();
    for item in talk(true, false) {
        let cell = TalkCell::new(item, Look::Pictures, faces.clone(), &mut spacing);
        let rows = cell.lines(&ctx).len();
        placed.extend(cell.picture(&ctx, (18, 42)).map(|picture| (row, picture)));
        row += rows;
    }
    let (_, typing_shapes) =
        Look::Pictures.typing(&[qwen()], 1, &reason(QWEN_WRITES), 120, &faces, &theme);
    let typing_row = row + 1;
    placed.extend(
        super::super::base::draw::picture::draw(&typing_shapes, (18, 42), theme.brightness)
            .map(|picture| (typing_row, picture)),
    );
    let top = typing_row - 23;
    let (cw, ch) = (18usize, 42usize);
    let mut screen = crate::base::mascot::png::Rgba::transparent(120 * cw, 38 * ch);
    for pixel in screen.pixels.chunks_mut(4) {
        pixel.copy_from_slice(&[0x22, 0x24, 0x36, 0xff]);
    }
    for (at, picture) in placed {
        let image = crate::base::mascot::png::decode(&picture.png).expect("a png");
        let x0 = usize::from(picture.col) * cw;
        let Some(y_row) = (at + usize::from(picture.row)).checked_sub(top) else {
            continue;
        };
        let y0 = y_row * ch;
        for y in 0..image.height {
            for x in 0..image.width {
                let (sx, sy) = (x0 + x, y0 + y);
                if sx >= screen.width || sy >= screen.height {
                    continue;
                }
                let i = (y * image.width + x) * 4;
                let a = f32::from(image.pixels[i + 3]) / 255.0;
                let j = (sy * screen.width + sx) * 4;
                for c in 0..3 {
                    let mixed = f32::from(image.pixels[i + c]) * a
                        + f32::from(screen.pixels[j + c]) * (1.0 - a);
                    screen.pixels[j + c] = mixed.round() as u8;
                }
            }
        }
    }
    let dir = std::path::PathBuf::from(dir);
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(
        dir.join("talk-bubbles.png"),
        crate::base::mascot::png::encode(&screen),
    )
    .expect("the picture written");
}

/// The working line `verb` draws `seconds` into the work, with its tokens, their limit and
/// its details.
fn working_rows(
    verb: &str,
    seconds: u64,
    (tokens, limit): (u64, Option<u64>),
    details: &str,
    (tip, note): (Option<&'static str>, Option<String>),
    width: u16,
) -> Vec<String> {
    use std::time::Duration;

    use crate::bottom::WorkingLine;

    let start = Instant::now();
    let mut line = WorkingLine::new(verb, start);
    line.tokens_down = tokens;
    line.tokens_limit = limit;
    line.details = vec![details.to_owned()];
    line.tip = tip;
    line.note = note;
    let theme = Theme::dark();
    let clock = Clock::new(start, start + Duration::from_secs(seconds));
    let ctx = RenderCtx::new(width, &theme, clock);
    let mut rows: Vec<String> = line.lines(&ctx).iter().map(text).collect();
    // The spinner turns with the clock; the screens show it at its ✻.
    if let Some(first) = rows.first_mut()
        && let Some(spinner) = first.chars().next()
    {
        first.replace_range(..spinner.len_utf8(), "✻");
    }
    rows
}

/// While the seats talk the working line parleys against the talk's token limit and counts the
/// seats that agree, with the tip under it as the council starts; once they work it rows and
/// counts the tasks done.
#[test]
fn the_working_line_parleys_then_rows() {
    assert_rows_from(
        "sent",
        &working_rows(
            TALKING,
            14,
            (3_200, Some(200_000)),
            "0/4 seats agree",
            (Some(TIP), None),
            120,
        ),
        25,
    );
    assert_rows_from(
        "blind_check",
        &working_rows(
            WORKING,
            552,
            (1_100_000, None),
            "1/3 tasks",
            (None, None),
            120,
        ),
        25,
    );
    let talking = |seconds, tokens, agree: &str| {
        working_rows(
            TALKING,
            seconds,
            (tokens, Some(200_000)),
            agree,
            (None, None),
            120,
        )
    };
    assert_rows_from(
        "first_answers",
        &talking(108, 34_500, "0/4 seats agree"),
        26,
    );
    assert_rows_from(
        "everyone_agreed",
        &talking(280, 82_900, "4/4 seats agree"),
        26,
    );
    let rowing = |seconds, tokens, done: &str, width| {
        working_rows(WORKING, seconds, (tokens, None), done, (None, None), width)
    };
    assert_rows_from(
        "same_maker_check",
        &rowing(545, 1_100_000, "1/3 tasks", 120),
        25,
    );
    for screen in ["work_starts", "task_picked", "transcript_task_picked"] {
        assert_rows_from(screen, &rowing(662, 1_200_000, "0/4 tasks", 120), 26);
    }
    assert_rows_from("work_nests", &rowing(846, 1_600_000, "1/4 tasks", 120), 21);
    assert_rows_from(
        "work_at_80_columns",
        &rowing(846, 1_600_000, "1/4 tasks", 80),
        21,
    );
}

/// The points still open, as the talk's points list them, each by its sides or else its words.
pub(crate) fn still_open(points: &[(&str, &[&str])]) -> Option<String> {
    use argonauts_core::runs::events::council::{
        CouncilEventKind, Point, PointState, Seating, Side,
    };

    let seating = Seating {
        seats: Vec::new(),
        notes: None,
    };
    let mut council = crate::council::Conversation::new(&seating, story::faces());
    let mut listed: Vec<Point> = points
        .iter()
        .zip(1..)
        .map(|((words, sides), number)| Point {
            number,
            words: (*words).to_owned(),
            state: PointState::Open,
            raised_by: None,
            sides: sides
                .iter()
                .map(|side| Side {
                    words: (*side).to_owned(),
                    backers: Vec::new(),
                    reason: String::new(),
                })
                .collect(),
        })
        .collect();
    listed.push(Point {
        number: 9,
        words: "Retry-After on every 429".to_owned(),
        state: PointState::Agreed,
        raised_by: None,
        sides: Vec::new(),
    });
    council.on(&CouncilEventKind::Points { points: listed }, None);
    council.still_open_line()
}

/// Under the working line while the seats talk, the points still open take the tip's row.
#[test]
fn the_points_still_open_sit_under_the_working_line() {
    const BUCKET: (&str, &[&str]) = (
        "A token bucket per key",
        &["token bucket", "window counter"],
    );
    const STORE: (&str, &[&str]) = ("Where counts live", &["Redis", "memory"]);
    const NO_KEY: (&str, &[&str]) = ("calls with no key", &[]);
    let talk = |seconds, tokens, agree: &str, open: &[(&str, &[&str])], width| {
        working_rows(
            TALKING,
            seconds,
            (tokens, Some(200_000)),
            agree,
            (Some(TIP), still_open(open)),
            width,
        )
    };
    assert_rows_from(
        "proposal",
        &talk(
            150,
            46_000,
            "2/4 seats agree",
            &[BUCKET, STORE, NO_KEY],
            120,
        ),
        25,
    );
    for screen in ["talk", "transcript", "blocks"] {
        assert_rows_from(
            screen,
            &talk(200, 61_200, "2/4 seats agree", &[STORE, NO_KEY], 120),
            25,
        );
    }
    assert_rows_from(
        "talk_at_80_columns",
        &talk(200, 61_200, "2/4 seats agree", &[STORE, NO_KEY], 80),
        25,
    );
    for screen in ["two_seats_one_model", "two_seats_one_model_transcript"] {
        assert_rows_from(
            screen,
            &talk(170, 52_400, "2/4 seats agree", &[STORE, NO_KEY], 120),
            25,
        );
    }
    assert_rows_from(
        "you_reply",
        &talk(206, 62_000, "2/4 seats agree", &[STORE, NO_KEY], 120),
        24,
    );
    assert_rows_from(
        "moved_in_private",
        &talk(242, 70_300, "3/4 seats agree", &[NO_KEY], 120),
        25,
    );
    assert_rows_from(
        "you_mid_talk",
        &talk(211, 63_400, "0/4 seats agree", &[STORE, NO_KEY], 120),
        25,
    );
    assert_eq!(still_open(&[]), None, "nothing open, no row");
}

/// What the input and the bottom line under it show at one moment of a council: whether its
/// work runs, what you typed, and the keys a picked message or task takes.
struct Under<'a> {
    working: bool,
    typed: Option<&'a str>,
    picking: Option<&'a str>,
    width: u16,
}

impl Under<'_> {
    /// The input's rule, its row and its closing rule, then the bottom line, as the app draws
    /// them under a council whose run has its row under them.
    fn rows(&self) -> Vec<String> {
        use crate::bottom::{WorkflowRow, WorkflowRowState, WorkingLine};

        let theme = Theme::dark();
        let mut bottom = crate::bottom::BottomState::default();
        bottom.council.sits = true;
        bottom.composer.label = Some("council".to_owned());
        if self.working {
            bottom.working = Some(WorkingLine::new(TALKING, Instant::now()));
        }
        if let Some(typed) = self.typed {
            bottom.composer.set_text(typed);
            bottom.enter_sends = Some(crate::bridge::status::GUIDES_THE_RUN.to_owned());
        }
        bottom.council.picking = self.picking.map(str::to_owned);
        bottom.workflows.rows = vec![WorkflowRow::new(
            "run-limit",
            "Rate limit the API",
            WorkflowRowState::Running,
        )];
        let ctx = RenderCtx::new(self.width, &theme, Clock::still(Instant::now()));
        let rows = screen_rows(&bottom.render_live(&ctx, 30, false).lines);
        let at = rows
            .iter()
            .position(|row| row.ends_with(" council ─"))
            .unwrap_or_else(|| panic!("the input's rule in {rows:#?}"));
        rows[at..at + 4].to_vec()
    }
}

/// While a council talks, the input says how to write to it under the rule its workflow
/// names, and the bottom line says what Esc does while the council works, what a picked
/// message or task takes, or that Enter sends what you typed.
#[test]
fn the_input_says_how_to_write_to_the_council() {
    let at_work = Under {
        working: true,
        typed: None,
        picking: None,
        width: 120,
    };
    for screen in [
        "sent",
        "first_answers",
        "proposal",
        "talk",
        "blocks",
        "transcript",
        "two_seats_one_model",
        "two_seats_one_model_transcript",
        "moved_in_private",
        "everyone_agreed",
        "you_mid_talk",
        "work_starts",
    ] {
        assert_rows_from(screen, &at_work.rows(), 28);
    }
    let narrow = Under {
        width: 80,
        ..at_work
    };
    assert_rows_from("talk_at_80_columns", &narrow.rows(), 28);
    let waiting = Under {
        working: false,
        ..at_work
    };
    assert_rows_from("approve_with_edits", &waiting.rows(), 28);
    let typed = "Calls with no key: don't break them yet. Warn them for 30 days, then 401. \
                 /health stays open.";
    let typing = Under {
        typed: Some(typed),
        ..at_work
    };
    assert_rows_from("you_reply", &typing.rows(), 28);

    // A picked task's keys follow the mode on the bottom line, in fewer words where the line
    // is narrow.
    let session =
        crate::bridge::council::task_keys(&argonauts_core::runs::events::task::Way::OwnSession);
    let agents =
        crate::bridge::council::task_keys(&argonauts_core::runs::events::task::Way::Agents {
            count: 3,
        });
    for (screen, keys, rule, width) in [
        ("blind_check", &session, 27, 120),
        ("same_maker_check", &session, 27, 120),
        ("work_nests", &agents, 23, 120),
        ("work_at_80_columns", &agents, 23, 80),
        ("task_picked", &agents, 28, 120),
        ("transcript_task_picked", &agents, 28, 120),
    ] {
        let under = Under {
            picking: Some(keys),
            width,
            ..at_work
        };
        assert_rows_from(screen, &under.rows(), rule);
    }
}

/// A council task's workflow view as the app docks it under the council on a 120 × 38 screen:
/// Build done, Review under way. Its head names the way, the task, whose seat it is and in
/// which council, and its keys go back to the council. The frame around the phases and agents
/// fits what it holds, keeps its margin at the right and counts agents.
#[test]
fn a_tasks_workflow_says_whose_task_it_is_in_the_council() {
    let rows = task_workflow_docked();
    assert!(
        rows[1].starts_with(" Maker-checker  Redis bucket · ")
            && rows[1].ends_with("Opus 5.5's task in the council Rate limit the API"),
        "{}",
        rows[1]
    );
    let about = "A worker builds, a reviewer sends it back until it holds";
    assert!(
        rows[2].starts_with(&format!(" {about}")) && rows[2].ends_with("1/2 agents · 6m40s"),
        "{}",
        rows[2]
    );
    let keys = format!(" · {} · s save", crate::council::work::task_header::BACK);
    assert!(
        rows.last().is_some_and(|row| row.ends_with(&keys)),
        "{rows:#?}"
    );
    let docked = std::fs::read_to_string(format!(
        "{}/tests/golden/council/task_workflow_docked.txt",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let docked: Vec<&str> = docked.lines().map(str::trim_end).collect();
    assert_eq!(rows, docked);
}

/// The rows of the council task's workflow view, docked as the app docks it on a 120 × 38
/// screen, logos as their pictures write them.
fn task_workflow_docked() -> Vec<String> {
    use argonauts_core::app::views::{
        AgentRow, PhaseCount, PhaseRow, RowStatus, WorkflowRow, WorkflowView as WorkflowData,
    };
    use argonauts_core::base::names::SessionKey;
    use argonauts_core::workflows::workflow::StandardPhase;
    use argonauts_core::{RunId, Spend};
    use ratatui::buffer::Buffer;
    use ratatui::layout::{Rect, Size};

    use crate::base::kit::logo::golden::{pictured, with_stand_ins};
    use crate::frame::view::dock;
    use crate::frame::view::{View, ViewCtx};
    use crate::views::workflow::WorkflowView;

    let agent = |session: &str, label: &str, worker: &str, status, tokens, elapsed_ms| AgentRow {
        session: SessionKey::parse(session).unwrap(),
        label: label.into(),
        worker: worker.parse().unwrap(),
        status,
        tokens,
        spent: Spend::default(),
        elapsed_ms,
        subagents: 0,
        context: None,
        fallback_for: None,
        answered: false,
        tries: Vec::new(),
        note: None,
        inner: None,
        left_out: false,
        not_needed: false,
        fallback_why: None,
    };
    let phase = |name: &str, standard, status, rows: Vec<AgentRow>| PhaseRow {
        name: name.into(),
        detail: String::new(),
        standard,
        status,
        try_number: None,
        count: PhaseCount::of(
            rows.iter()
                .filter(|row| row.status == RowStatus::Done)
                .count() as u32,
            rows.len() as u32,
        ),
        rows: rows.into_iter().map(WorkflowRow::Agent).collect(),
        note: None,
    };
    let review = agent(
        "s-002",
        "Review · Redis bucket",
        "codex/gpt-5.6-luna",
        RowStatus::Running,
        232_000,
        200_000,
    );
    let build = agent(
        "s-001",
        "Build · Redis bucket",
        "claude-code/claude-opus-5-5",
        RowStatus::Done,
        120_000,
        200_000,
    );
    let task = story::task_header();
    let data = WorkflowData {
        run: RunId::new("run-01").unwrap(),
        id: task.workflow.clone(),
        name: "maker-checker".into(),
        description: "A worker builds, a reviewer sends it back until it holds".into(),
        style: None,
        status_lines: Vec::new(),
        agents_done: 1,
        agents_total: 2,
        elapsed_ms: 400_000,
        spent: Spend::default(),
        phases: vec![
            phase("Build", StandardPhase::Do, RowStatus::Done, vec![build]),
            phase(
                "Review",
                StandardPhase::Check,
                RowStatus::Running,
                vec![review],
            ),
        ],
        notes: Vec::new(),
        required: Vec::new(),
        goals: Vec::new(),
        worker: None,
        state: Default::default(),
    };
    let mut view = WorkflowView::new(data).for_council_task(task, story::faces());
    // The mark at work holds its first frame, as it does where motion is reduced.
    let mut theme = pictured(&Theme::dark());
    theme.reduced_motion = true;
    let state = crate::frame::state::AppState::default();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    let screen = Size::new(120, 38);
    let height = view
        .dock_height(screen, &ctx)
        .unwrap_or_else(|| dock::height(screen.height))
        .min(screen.height - 2);
    let area = Rect::new(0, 0, screen.width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    crate::base::stability::buffer_text(&with_stand_ins(&buf))
        .lines()
        .map(|row| row.trim_end().to_owned())
        .collect()
}

/// Every logo the talk draws is the one of the seat it stands for: on each bubble's tail, in
/// each quote and mention, and in each tapback, as the talk's screen shows them; and a model
/// that sits twice wears its logo with each seat's number.
#[test]
fn each_logo_in_the_talk_is_its_own_seats() {
    let drawn = |logos: Vec<Vec<String>>| -> Vec<String> {
        logos
            .into_iter()
            .filter(|row| !row.is_empty())
            .map(|row| row.join(" "))
            .collect()
    };
    let talk = drawn(printed_logos(
        &talk(true, false),
        Look::Pictures,
        120,
        &story::faces(),
    ));
    assert_eq!(
        talk,
        [
            "claudecode antigravity ollama codex",
            "claudecode codex antigravity ollama",
            "claudecode antigravity codex ollama",
            "claudecode codex",
            "claudecode",
            "claudecode antigravity ollama",
            "codex",
            "codex",
            "codex antigravity ollama",
            "antigravity",
            "claudecode",
            "codex",
            "ollama",
            "claudecode codex",
            "antigravity",
        ]
    );
    let faces = story::faces_one_model_twice();
    let twice = drawn(printed_logos(
        &one_model_twice_talk(true),
        Look::Pictures,
        120,
        &faces,
    ));
    assert_eq!(
        twice,
        [
            "claudecode 1",
            "claudecode 2",
            "claudecode 1",
            "claudecode 1 codex antigravity",
            "claudecode 2",
            "claudecode 2",
            "claudecode 2 codex antigravity",
            "codex",
            "claudecode 1",
        ]
    );
}
