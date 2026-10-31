use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use super::*;
use crate::pty::emulator::Emulator;
use crate::screen::{Colour, Placeholder, Rgb};

const ID: u32 = 7;

/// Pixels a cell, width then height.
const CELL: (u16, u16) = (10, 20);

fn taking() -> Emulator {
    Emulator::new(4, 20, 100).takes_pictures(CELL)
}

/// Picture `id` and a virtual placement two cells wide, as the app sends a logo.
fn sent(id: u32) -> String {
    format!(
        "\x1b_Ga=t,f=100,q=2,i={id},m=0;iVBORw0KGgo=\x1b\\\
         \x1b_Ga=p,U=1,i={id},c=2,r=1,q=2\x1b\\"
    )
}

/// Picture `id`'s two placeholder cells at the top left, in the colour that names it.
fn drawn(id: u32) -> String {
    let [_, red, green, blue] = id.to_be_bytes();
    format!(
        "\x1b[1;1H\x1b[38;2;{red};{green};{blue}m\
         \u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\u{0305}\u{030D}\x1b[0m"
    )
}

fn feed(emulator: &mut Emulator, parts: &[&str]) {
    for part in parts {
        emulator.process(part.as_bytes());
    }
}

fn shows(emulator: &Emulator) -> Option<bool> {
    let cells = emulator.cells();
    let picture = cells.get(0, 0)?.picture?;
    assert_eq!(picture.id, ID);
    Some(picture.shows)
}

fn blank_on(screen: &str, why: &str) -> Vec<String> {
    vec![format!(
        "placeholder for picture {ID} on the {screen}, {why} \
         (2 cells, the first at row 0, column 0)"
    )]
}

#[test]
fn a_picture_sent_on_the_screen_that_draws_it_shows() {
    let mut emulator = taking();
    feed(&mut emulator, &[&sent(ID), &drawn(ID)]);
    assert_eq!(shows(&emulator), Some(true));
    assert_eq!(
        emulator.cells().get(0, 1).and_then(|cell| cell.picture),
        Some(Placeholder {
            id: ID,
            shows: true
        })
    );
    assert!(emulator.picture_problems().is_empty());
}

#[test]
fn the_alternate_screen_holds_none_of_the_main_screens_pictures() {
    let mut emulator = taking();
    feed(&mut emulator, &[&sent(ID), "\x1b[?1049h", &drawn(ID)]);
    assert_eq!(shows(&emulator), Some(false));
    assert_eq!(
        emulator.picture_problems(),
        blank_on("alternate screen", "which holds none")
    );

    // Sent again on the screen that draws them, they show.
    feed(&mut emulator, &[&sent(ID)]);
    assert_eq!(shows(&emulator), Some(true));
    assert!(emulator.picture_problems().is_empty());

    // The main screen kept its own all along.
    feed(&mut emulator, &["\x1b[?1049l", &drawn(ID)]);
    assert_eq!(shows(&emulator), Some(true));
}

#[test]
fn entering_the_alternate_screen_empties_it_every_time() {
    let mut emulator = taking();
    feed(&mut emulator, &["\x1b[?1049h", &sent(ID), &drawn(ID)]);
    assert_eq!(shows(&emulator), Some(true));
    feed(&mut emulator, &["\x1b[?1049l\x1b[?1049h", &drawn(ID)]);
    assert_eq!(shows(&emulator), Some(false));

    // Even from the alternate screen itself.
    feed(&mut emulator, &[&sent(ID), "\x1b[?1049h", &drawn(ID)]);
    assert_eq!(shows(&emulator), Some(false));

    // A switch that doesn't clear keeps them.
    feed(
        &mut emulator,
        &[&sent(ID), "\x1b[?47l\x1b[?47h", &drawn(ID)],
    );
    assert_eq!(shows(&emulator), Some(true));
}

#[test]
fn clearing_a_screen_whole_empties_its_store_alone() {
    let mut emulator = taking();
    feed(
        &mut emulator,
        &[&sent(ID), "\x1b[?1049h", &sent(ID), "\x1b[2J", &drawn(ID)],
    );
    assert_eq!(
        emulator.picture_problems(),
        blank_on("alternate screen", "which holds none")
    );
    // Clearing below the cursor, as an app redraws, keeps them.
    feed(
        &mut emulator,
        &[&sent(ID), "\x1b[J\x1b[1J\x1b[3J", &drawn(ID)],
    );
    assert_eq!(shows(&emulator), Some(true));
    feed(&mut emulator, &["\x1b[?1049l", &drawn(ID)]);
    assert_eq!(shows(&emulator), Some(true));
}

#[test]
fn pictures_sent_on_the_alternate_screen_stay_off_the_main_one() {
    let mut emulator = taking();
    feed(&mut emulator, &[&sent(ID + 1), "\x1b[?1049h", &sent(ID)]);
    feed(&mut emulator, &["\x1b[?1049l", &drawn(ID)]);
    assert_eq!(
        emulator.picture_problems(),
        blank_on("main screen", "which doesn't hold it")
    );
}

#[test]
fn a_picture_shows_once_its_last_chunk_arrives() {
    let mut emulator = taking();
    let first = format!("\x1b_Ga=T,U=1,f=100,q=2,i={ID},c=2,r=1,m=1;iVBO\x1b\\");
    feed(&mut emulator, &[&first, "\x1b_Gm=1;Rw0K\x1b\\", &drawn(ID)]);
    assert_eq!(shows(&emulator), Some(false));
    feed(&mut emulator, &["\x1b_Gm=0;Ggo=\x1b\\"]);
    assert_eq!(shows(&emulator), Some(true));
}

#[test]
fn a_placement_that_finds_no_picture_stays_a_problem() {
    let mut emulator = taking();
    feed(
        &mut emulator,
        &["\x1b[?1049h\x1b_Ga=p,q=2,C=1,i=9,p=1\x1b\\"],
    );
    assert_eq!(
        emulator.picture_problems(),
        ["a placement of picture 9 on the alternate screen, which held none"]
    );
    assert!(emulator.misplaced_a_picture());
    feed(
        &mut emulator,
        &["\x1b_Ga=p,q=2,C=1,i=9,p=1\x1b\\", &sent(9)],
    );
    assert_eq!(
        emulator.picture_problems(),
        ["2 placements of picture 9 on the alternate screen, which held none"]
    );
}

#[test]
fn deleting_takes_off_the_placement_or_the_picture() {
    let mut emulator = taking();
    feed(&mut emulator, &[&sent(ID), &sent(ID + 1), &drawn(ID)]);
    feed(&mut emulator, &[&format!("\x1b_Ga=d,d=i,q=2,i={ID}\x1b\\")]);
    assert_eq!(
        emulator.picture_problems(),
        blank_on(
            "main screen",
            "which holds it with no placement for placeholders"
        )
    );
    feed(
        &mut emulator,
        &[&sent(ID), &format!("\x1b_Ga=d,d=I,q=2,i={ID}\x1b\\")],
    );
    assert_eq!(
        emulator.picture_problems(),
        blank_on("main screen", "which doesn't hold it")
    );
    feed(&mut emulator, &[&sent(ID), "\x1b_Ga=d,d=A,q=2\x1b\\"]);
    assert_eq!(
        emulator.picture_problems(),
        blank_on("main screen", "which holds none")
    );
}

#[test]
fn a_terminal_that_takes_no_pictures_shows_no_placeholder() {
    let mut emulator = Emulator::new(4, 20, 100);
    feed(&mut emulator, &[&sent(ID), &drawn(ID)]);
    assert_eq!(shows(&emulator), Some(false));
    assert_eq!(
        emulator.picture_problems(),
        blank_on("main screen", "in a terminal that takes no pictures")
    );
}

#[test]
fn a_synchronized_frame_draws_from_the_pictures_held_before_it() {
    let mut emulator = taking();
    let first = format!("\x1b[?2026h\x1b[?1049h{}{}\x1b[?2026l", sent(ID), drawn(ID));
    feed(&mut emulator, &[&first]);
    assert_eq!(shows(&emulator), Some(true));
    // The next frame frees the picture and sends it again; until it ends, the last one shows.
    let freed = format!("\x1b[?2026h\x1b_Ga=d,d=I,q=2,i={ID}\x1b\\");
    feed(&mut emulator, &[&freed]);
    assert_eq!(shows(&emulator), Some(true));
    feed(&mut emulator, &[&sent(ID), "\x1b[?2026l"]);
    assert_eq!(shows(&emulator), Some(true));
}

#[test]
fn a_full_reset_empties_both_stores() {
    let mut emulator = taking();
    feed(
        &mut emulator,
        &[&sent(ID), "\x1b[?1049h", &sent(ID), "\x1bc", &drawn(ID)],
    );
    assert_eq!(
        emulator.picture_problems(),
        blank_on("main screen", "which holds none")
    );
}

#[test]
fn bytes_split_anywhere_read_the_same() {
    let stream = format!(
        "{}\x1b_Xnot a picture\x1b\\\x1b[?1049h{}\x1b_Ga=t,i=3,m=1;AA\x1b\\{}\x1b_Gm=0;AA\x1b\\",
        sent(ID),
        drawn(ID),
        sent(ID)
    );
    let mut whole = taking();
    whole.process(stream.as_bytes());
    let mut bytewise = taking();
    for byte in stream.as_bytes() {
        bytewise.process(std::slice::from_ref(byte));
    }
    assert_eq!(whole.cells(), bytewise.cells());
    assert_eq!(shows(&whole), Some(true));
    assert!(whole.picture_problems().is_empty());
}

#[test]
fn a_command_cut_off_by_another_escape_is_dropped() {
    let mut emulator = taking();
    let cut = format!("\x1b_Ga=t,q=2,i={ID};iVBO\x1b[1m");
    feed(&mut emulator, &[&cut, &drawn(ID)]);
    assert_eq!(shows(&emulator), Some(false));
    feed(&mut emulator, &[&sent(ID)]);
    assert_eq!(shows(&emulator), Some(true));
}

#[test]
fn the_pictures_never_show_as_text() {
    let mut emulator = taking();
    feed(&mut emulator, &[&sent(ID), "\x1b_Xother\x1b\\", "after"]);
    assert_eq!(
        emulator.cells().get(0, 0).map(|cell| cell.text.as_str()),
        Some("a")
    );
}

#[test]
fn an_indexed_colour_names_a_picture_by_its_index() {
    let mut emulator = taking();
    feed(
        &mut emulator,
        &[&sent(42), "\x1b[38;5;42m\u{10EEEE}\u{0305}\u{0305}\x1b[0m"],
    );
    assert_eq!(
        emulator.cells().get(0, 0).and_then(|cell| cell.picture),
        Some(Placeholder {
            id: 42,
            shows: true
        })
    );
}

#[test]
fn cells_carry_the_colours_and_attributes_the_program_set() {
    let mut emulator = Emulator::new(2, 10, 100);
    emulator.process("\x1b[1;3;38;2;1;2;3;48;5;17mA\x1b[0;2;7m界\x1b[0m".as_bytes());
    let cells = emulator.cells();
    let first = cells.get(0, 0).cloned().unwrap_or_default();
    assert_eq!(
        (
            first.text.as_str(),
            first.ink,
            first.background,
            first.bold,
            first.italic
        ),
        (
            "A",
            Colour::Rgb(Rgb(1, 2, 3)),
            Colour::Indexed(17),
            true,
            true
        )
    );
    let wide = cells.get(0, 1).cloned().unwrap_or_default();
    assert!(
        wide.wide && wide.dim && wide.inverse && !wide.bold,
        "{wide:?}"
    );
    assert_eq!(wide.ink, Colour::Own);
    assert!(cells.get(0, 2).is_some_and(|cell| cell.is_covered()));
    assert_eq!(cells.get(2, 0), None);
    assert_eq!((cells.rows(), cells.cols()), (2, 10));
    assert!(!cells.alternate());
}

#[test]
fn a_command_of_chunk_keys_alone_continues_a_transmission() {
    let parse = |keys: &str| Command::parse(keys.as_bytes(), &[]);
    assert!(parse("m=1").continues);
    assert!(parse("m=0,q=2").continues);
    assert!(!parse("a=t,m=1").continues);
    assert!(!parse("").continues);
    let placed = parse("a=p,U=1,i=12,p=3,c=2,r=1,q=2");
    assert_eq!(
        (
            placed.action,
            placed.id,
            placed.placement,
            placed.virtual_placement
        ),
        (b'p', 12, 3, true)
    );
    assert_eq!(parse("i=4").action, b't');
    assert_eq!(parse("a=d").delete, b'a');
}

/// Picture `id` sent as a PNG `width` × `height` pixels, its file cut after the header's size.
fn sent_png(id: u32, (width, height): (u32, u32)) -> String {
    let mut file = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    file.extend(width.to_be_bytes());
    file.extend(height.to_be_bytes());
    let data = STANDARD.encode(&file);
    format!("\x1b_Ga=t,f=100,q=2,i={id},m=0;{data}\x1b\\")
}

/// The cells of `emulator`'s screen that show picture `id`, row then column.
fn showing(emulator: &Emulator, id: u32) -> Vec<(u16, u16)> {
    let cells = emulator.cells();
    (0..cells.rows())
        .flat_map(|row| (0..cells.cols()).map(move |col| (row, col)))
        .filter(|&(row, col)| cells.cell(row, col).picture == Some(Placeholder { id, shows: true }))
        .collect()
}

/// Rows `rows` by columns `cols`, row then column.
fn block(rows: std::ops::Range<u16>, cols: std::ops::Range<u16>) -> Vec<(u16, u16)> {
    rows.flat_map(|row| cols.clone().map(move |col| (row, col)))
        .collect()
}

#[test]
fn a_picture_placed_on_the_screen_shows_over_the_cells_from_the_cursor() {
    let mut emulator = taking();
    feed(
        &mut emulator,
        &[
            &sent_png(ID, (64, 64)),
            &format!("\x1b[2;4H\x1b_Ga=p,q=2,C=1,i={ID},p=1,c=4,r=2\x1b\\"),
        ],
    );
    assert_eq!(showing(&emulator, ID), block(1..3, 3..7));
    assert!(emulator.picture_problems().is_empty());
    assert_eq!(emulator.screen().cursor_position(), (1, 3));
}

#[test]
fn a_picture_at_its_own_pixels_covers_the_cells_they_reach() {
    let mut emulator = taking();
    feed(
        &mut emulator,
        &[
            &sent_png(ID, (25, 30)),
            &format!("\x1b[1;1H\x1b_Ga=p,q=2,C=1,i={ID},p=1,X=5\x1b\\"),
        ],
    );
    assert_eq!(showing(&emulator, ID), block(0..2, 0..3));
    feed(
        &mut emulator,
        &[&format!(
            "\x1b[1;1H\x1b_Ga=p,q=2,C=1,i={ID},p=1,X=5,x=10,y=10,w=10,h=10\x1b\\"
        )],
    );
    assert_eq!(showing(&emulator, ID), block(0..1, 0..2));
}

#[test]
fn a_placement_given_its_columns_alone_keeps_the_pictures_shape() {
    let mut emulator = taking();
    feed(
        &mut emulator,
        &[
            &sent_png(ID, (20, 40)),
            &format!("\x1b[1;1H\x1b_Ga=p,q=2,C=1,i={ID},p=1,c=4\x1b\\"),
        ],
    );
    assert_eq!(showing(&emulator, ID), block(0..4, 0..4));
}

#[test]
fn a_picture_under_the_text_shows_where_the_cells_are_blank() {
    let mut emulator = taking();
    feed(
        &mut emulator,
        &[
            &sent_png(ID, (64, 64)),
            "\x1b[1;1Hab",
            &format!("\x1b[1;1H\x1b_Ga=p,q=2,C=1,i={ID},p=1,c=4,r=1,z=-1\x1b\\"),
        ],
    );
    assert_eq!(showing(&emulator, ID), [(0, 2), (0, 3)]);
    feed(
        &mut emulator,
        &[&format!(
            "\x1b_Ga=p,q=2,C=1,i={ID},p=1,c=4,r=1,z=-1073741825\x1b\\\x1b[1;4H\x1b[44m \x1b[0m"
        )],
    );
    assert_eq!(showing(&emulator, ID), [(0, 2)]);
}

#[test]
fn the_highest_picture_shows_where_two_overlap() {
    let mut emulator = taking();
    feed(
        &mut emulator,
        &[
            &sent_png(ID, (64, 64)),
            &sent_png(ID + 1, (64, 64)),
            &format!(
                "\x1b[1;1H\x1b_Ga=p,q=2,C=1,i={},p=1,c=2,r=1,z=5\x1b\\",
                ID + 1
            ),
            &format!("\x1b_Ga=p,q=2,C=1,i={ID},p=1,c=4,r=1\x1b\\"),
        ],
    );
    assert_eq!(showing(&emulator, ID + 1), [(0, 0), (0, 1)]);
    assert_eq!(showing(&emulator, ID), [(0, 2), (0, 3)]);
}

#[test]
fn deleting_or_moving_a_placement_takes_it_off_its_cells() {
    let mut emulator = taking();
    let place = format!("\x1b[1;1H\x1b_Ga=p,q=2,C=1,i={ID},p=1,c=2,r=1\x1b\\");
    feed(&mut emulator, &[&sent_png(ID, (64, 64)), &place]);
    feed(
        &mut emulator,
        &[&format!(
            "\x1b[3;5H\x1b_Ga=p,q=2,C=1,i={ID},p=1,c=2,r=1\x1b\\"
        )],
    );
    assert_eq!(showing(&emulator, ID), [(2, 4), (2, 5)]);
    feed(
        &mut emulator,
        &[&format!("\x1b_Ga=d,d=i,q=2,i={ID},p=1\x1b\\")],
    );
    assert_eq!(showing(&emulator, ID), []);
    feed(&mut emulator, &[&place, "\x1b[2J"]);
    assert_eq!(showing(&emulator, ID), []);
}

#[test]
fn a_picture_placed_on_the_main_screen_goes_up_with_its_text() {
    let mut emulator = taking();
    feed(
        &mut emulator,
        &[
            &sent_png(ID, (64, 64)),
            &format!("\x1b[4;1H\x1b_Ga=p,q=2,C=1,i={ID},p=1,c=2,r=1\x1b\\"),
            "\n",
        ],
    );
    assert_eq!(showing(&emulator, ID), [(2, 0), (2, 1)]);
}

#[test]
fn a_picture_placed_on_one_screen_shows_on_it_alone() {
    let mut emulator = taking();
    feed(
        &mut emulator,
        &[
            "\x1b[?1049h",
            &sent_png(ID, (64, 64)),
            &format!("\x1b[1;1H\x1b_Ga=p,q=2,C=1,i={ID},p=1,c=2,r=1\x1b\\"),
        ],
    );
    assert_eq!(showing(&emulator, ID), [(0, 0), (0, 1)]);
    feed(&mut emulator, &["\x1b[?1049l"]);
    assert_eq!(showing(&emulator, ID), []);
}
