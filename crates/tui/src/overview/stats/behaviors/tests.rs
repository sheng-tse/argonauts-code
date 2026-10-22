use argonauts_core::app::ViewRequest;
use argonauts_core::behavior_map::{Grouping, Scope};
use argonauts_core::overview::OverviewPeriod;
use crossterm::event::KeyCode;
use unicode_width::UnicodeWidthStr;

use super::sample::Sample;
use super::{At, BehaviorsTab, GroupColours, MapKey};
use crate::kit::Kit;
use crate::theme::{Glyphs, Theme};

fn sample() -> Sample {
    super::sample::three_models()
}

fn opened() -> BehaviorsTab {
    let mut tab = BehaviorsTab::default();
    tab.accept(&sample().map);
    tab
}

fn opened_compared() -> BehaviorsTab {
    let mut tab = opened();
    press(&mut tab, &[KeyCode::Right]);
    tab
}

fn press(tab: &mut BehaviorsTab, keys: &[KeyCode]) -> Option<MapKey> {
    let mut last = None;
    for key in keys {
        last = tab.key(*key);
    }
    last
}

fn shown(tab: &BehaviorsTab, room: usize, pictures: bool) -> Vec<String> {
    shown_at(tab, 118, room, pictures)
}

fn shown_at(tab: &BehaviorsTab, width: usize, room: usize, pictures: bool) -> Vec<String> {
    let mut tab = tab.clone();
    tab.picture_placed = pictures;
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme.mascot_pictures = pictures;
    let kit = Kit::new(&theme);
    let map = tab.map().unwrap();
    let colours = GroupColours::new(&kit, map, &[], &Default::default());
    tab.blocks(&kit, &colours, width, room)
        .iter()
        .flat_map(|block| block.lines.iter())
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[test]
fn it_opens_on_every_groups_dots_with_nothing_compared_or_lit() {
    let tab = opened();
    assert_eq!(tab.at, At::Map);
    assert_eq!(tab.compared(), None);
    assert!(tab.lit().is_empty());
    let lines = shown(&tab, 40, true);
    assert_eq!(
        lines[0],
        " What your agents did, from 28 actions in 19 runs across 6 projects · by model · Argonauts' labels"
    );
    assert!(lines.iter().any(|l| l.ends_with("Every model")));
    assert!(lines.iter().any(|l| l.contains("● Haiku 5.5          18")));
    assert!(
        lines.iter().any(
            |l| l == " Pick a model with ←  →  to see what it does more or less than the rest."
        )
    );
    assert!(!lines.iter().any(|l| l.contains("Distinctive")));
}

#[test]
fn left_and_right_go_round_every_group_then_each_compared() {
    let mut tab = opened();
    press(&mut tab, &[KeyCode::Right]);
    assert_eq!(tab.compared(), Some(0));
    press(&mut tab, &[KeyCode::Right, KeyCode::Right]);
    assert_eq!(tab.compared(), Some(2));
    press(&mut tab, &[KeyCode::Right]);
    assert_eq!(tab.compared(), None, "round to every group alike");
    press(&mut tab, &[KeyCode::Left]);
    assert_eq!(tab.compared(), Some(2));
    let lines = shown(&tab, 40, true);
    let why = "3 actions are too few to draw where it acts: it takes 5.";
    let said: Vec<&String> = lines.iter().filter(|l| l.contains("too few")).collect();
    assert_eq!(said.len(), 1, "said once: {lines:#?}");
    assert_eq!(
        said[0].trim_end(),
        format!(" {why}"),
        "set in as the list's rows are"
    );
    assert!(
        !lines.iter().any(|l| l.contains("Distinctive to")),
        "no heading over an empty list"
    );
    assert!(lines.iter().any(|l| l.ends_with("○ Haiku 5.5          18")));
}

#[test]
fn the_deeper_map_is_offered_on_the_map_and_gives_its_rows_to_the_list() {
    let mut map = sample().map;
    map.offer_deeper = true;
    let mut tab = BehaviorsTab::default();
    tab.accept(&map);
    press(&mut tab, &[KeyCode::Right]);
    let offered = |tab: &BehaviorsTab| {
        shown(tab, 40, true)
            .iter()
            .any(|line| line.contains("✻ Deeper map"))
    };
    assert!(offered(&tab), "on the map");
    press(&mut tab, &[KeyCode::Down]);
    assert_eq!(tab.at, At::List(0));
    assert!(!offered(&tab), "in the list");
    press(&mut tab, &[KeyCode::Esc]);
    assert!(offered(&tab), "back on the map");
}

#[test]
fn down_goes_into_the_list_and_lights_a_behavior_and_up_or_esc_come_back_out() {
    let mut tab = opened();
    press(&mut tab, &[KeyCode::Down]);
    assert_eq!(tab.at, At::Map, "nothing to list with nothing compared");
    press(&mut tab, &[KeyCode::Right, KeyCode::Down]);
    assert_eq!(tab.at, At::List(0));
    assert_eq!(tab.lit().len(), 6, "every action under the behavior");
    press(&mut tab, &[KeyCode::Down]);
    assert_eq!(tab.at, At::List(1));
    assert_eq!(
        tab.lit().len(),
        17,
        "Haiku's twelve, Luna's two and Gemini's three"
    );
    let lines = shown(&tab, 40, true);
    assert!(
        lines.contains(&" lit: read a source file · 17 actions, 12 of them Haiku 5.5's".to_owned())
    );
    assert!(lines.iter().any(|l| l.starts_with(" ❯ read a source file")));
    press(&mut tab, &[KeyCode::Up, KeyCode::Up]);
    assert_eq!(tab.at, At::Map);
    press(&mut tab, &[KeyCode::Down, KeyCode::Esc]);
    assert_eq!(tab.at, At::Map);
    assert!(
        !tab.claims(KeyCode::Up, true),
        "↑ on the map is the panel's"
    );
}

#[test]
fn enter_lists_a_behaviors_actions_and_opens_each_ones_card() {
    let mut tab = opened();
    press(
        &mut tab,
        &[
            KeyCode::Right,
            KeyCode::Right,
            KeyCode::Down,
            KeyCode::Enter,
        ],
    );
    assert_eq!(tab.at, At::Actions { row: 0, action: 0 });
    assert!(tab.ringed().is_some());
    let lines = shown(&tab, 40, true);
    assert!(
        lines.contains(&" GPT 6 Luna · read git history · 5 times".to_owned()),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with(" ◉ read git history · GPT 6 Luna · build:per-api-key"))
    );
    press(&mut tab, &[KeyCode::Down, KeyCode::Down, KeyCode::Enter]);
    assert_eq!(tab.at, At::Card { row: 0, action: 2 });
    let card = shown(&tab, 40, true);
    assert_eq!(
        card[0],
        " build:per-api-key · GPT 6 Luna on Codex · builder · in the run's worktree"
    );
    assert!(card[1].starts_with(" run-20261020-123036-4e50 · council project · spec-first · "));
    assert!(card[1].ends_with(" · its action 3 of 6"));
    assert!(card.contains(&"   read git history: git status".to_owned()));
    assert!(card.contains(
        &"   GPT 6 Luna read git history in 71.4% of its actions (5 of 7); the other models in 0.0% (0 of 21)."
            .to_owned()
    ));
    assert!(
        !card.iter().any(|l| l.contains("What the run shows")),
        "the run says nothing"
    );
    press(&mut tab, &[KeyCode::Right]);
    assert_eq!(tab.at, At::Card { row: 0, action: 3 });
    press(
        &mut tab,
        &[KeyCode::Left, KeyCode::Left, KeyCode::Left, KeyCode::Left],
    );
    assert_eq!(
        tab.at,
        At::Card { row: 0, action: 4 },
        "round from the first"
    );
    assert_eq!(
        press(&mut tab, &[KeyCode::Char('c')]),
        Some(MapKey::Copy("git status".to_owned()))
    );
    match press(&mut tab, &[KeyCode::Char('o')]) {
        Some(MapKey::Ask(ViewRequest::Workflow { run, .. })) => {
            assert_eq!(run.as_str(), "run-20261020-123036-4e50");
        }
        other => panic!("{other:?}"),
    }
    press(&mut tab, &[KeyCode::Esc]);
    assert_eq!(tab.at, At::Actions { row: 0, action: 4 });
    press(&mut tab, &[KeyCode::Esc]);
    assert_eq!(tab.at, At::List(0));
}

#[test]
fn g_p_and_r_ask_for_a_new_map_from_the_top() {
    let mut tab = opened();
    press(&mut tab, &[KeyCode::Right, KeyCode::Down]);
    let asked = |key| match key {
        Some(MapKey::Ask(ViewRequest::Behaviors(request))) => request,
        other => panic!("{other:?}"),
    };
    let request = asked(press(&mut tab, &[KeyCode::Char('g')]));
    assert_eq!(request.grouping, Grouping::Cli);
    assert_eq!(tab.at, At::Map);
    assert_eq!(
        asked(press(&mut tab, &[KeyCode::Char('p')])).period,
        OverviewPeriod::Week
    );
    assert_eq!(
        asked(press(&mut tab, &[KeyCode::Char('r')])).scope,
        Scope::ThisProject
    );
    // A map for another request is an old answer.
    let mut old = sample().map;
    old.groups.clear();
    tab.accept(&old);
    assert_eq!(tab.map().unwrap().groups.len(), 3);
}

#[test]
fn a_new_map_keeps_the_comparison_on_the_group_of_the_same_name() {
    let mut tab = opened();
    press(&mut tab, &[KeyCode::Right, KeyCode::Right]);
    let mut again = sample().map;
    again.groups.swap(0, 1);
    again.comparisons.swap(0, 1);
    tab.accept(&again);
    assert_eq!(tab.compared(), Some(0));
}

#[test]
fn the_list_shows_the_rows_there_is_room_for() {
    let mut tab = opened();
    press(&mut tab, &[KeyCode::Right]);
    let roomy = shown(&tab, 40, true);
    assert!(roomy.iter().any(|l| l.starts_with("   read a source file")));
    // The line, a blank, the map, a blank and the heading leave one row.
    let tight = shown(&tab, 1 + 1 + 13 + 1 + 1 + 1, true);
    assert!(
        tight
            .iter()
            .any(|l| l.starts_with("   edited a source file"))
    );
    assert!(!tight.iter().any(|l| l.starts_with("   read a source file")));
}

#[test]
fn the_mouse_rings_the_nearest_dot_and_a_click_opens_its_card() {
    let mut tab = opened();
    assert!(tab.point_at(Some((0.61, 0.8))));
    let lit = tab.hovered().unwrap();
    assert_eq!(tab.map().unwrap().actions[lit].behavior, "read git history");
    assert!(!tab.point_at(Some((0.61, 0.8))), "the same dot");
    let lines = shown(&tab, 40, true);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with(" ◉ read git history · GPT 6 Luna"))
    );
    let hint =
        "   the map stays as it was; move away and the ring goes; click opens its card".to_owned();
    // With nothing compared the one row under the map goes to the dot's own line, so the
    // panel keeps its height; a list leaves room for the hint too.
    assert!(!lines.contains(&hint));
    let mut compared = tab.clone();
    press(&mut compared, &[KeyCode::Right]);
    compared.point_at(Some((0.61, 0.8)));
    assert!(shown(&compared, 40, true).contains(&hint));
    assert_eq!(
        shown(&compared, 40, true).len(),
        shown(&opened_compared(), 40, true).len(),
        "as many rows as without the mouse"
    );
    assert!(tab.click());
    assert!(matches!(tab.at, At::Card { row: 0, .. }));
    assert_eq!(tab.compared(), Some(1), "the dot's own group");
    tab.at = At::Map;
    tab.point_at(Some((0.2, 0.7)));
    assert!(tab.point_at(None));
    assert_eq!(tab.hovered(), None);
}

#[test]
fn without_pictures_the_map_is_braille_with_its_legend_under_it() {
    let mut tab = opened();
    press(&mut tab, &[KeyCode::Right, KeyCode::Right]);
    let lines = shown(&tab, 40, false);
    let map: Vec<&String> = lines[1..14].iter().collect();
    assert!(
        map.iter()
            .any(|l| l.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)))
    );
    assert!(lines[15].starts_with(" ⣿ Haiku 5.5  ⣿ GPT 6 Luna  ⣿ Gemini 3.8 Flash"));
    assert_eq!(
        lines[16],
        " red behind the dots:    GPT 6 Luna does this more · blue: less"
    );
}

#[test]
fn the_keys_say_what_each_place_takes_each_arrow_followed_by_two_spaces() {
    let mut tab = opened();
    assert_eq!(
        tab.keys(118).join(" · "),
        "←  →  compare a model · ↓  into the list, which lights each behavior · g group · p period · esc back"
    );
    assert_eq!(
        tab.keys(80).join(" · "),
        "←  →  compare · ↓  the list · g group · p period · esc back",
        "the short words where the long ones don't fit"
    );
    press(&mut tab, &[KeyCode::Right, KeyCode::Down]);
    assert_eq!(
        tab.keys(118).join(" · "),
        "←  →  model · ↑  ↓  behavior, lit on the map · enter its actions · g group · p period · esc back"
    );
    press(&mut tab, &[KeyCode::Enter]);
    assert_eq!(
        tab.keys(118).join(" · "),
        "↑  ↓  example · enter opens it · esc back"
    );
    press(&mut tab, &[KeyCode::Enter]);
    assert_eq!(
        tab.keys(118).join(" · "),
        "←  →  example · o opens the run · c copies the action · esc back"
    );
}

mod picture {
    use super::super::picture::{Scene, draw, png, size, to_place};
    use super::*;
    use crate::mascot::png::decode;

    fn pixel(image: &crate::mascot::png::Rgba, (x, y): (f32, f32)) -> (u8, u8, u8) {
        let at = (y as usize * image.width + x as usize) * 4;
        let p = &image.pixels[at..at + 3];
        (p[0], p[1], p[2])
    }

    fn scene<'a>(
        map: &'a argonauts_core::behavior_map::BehaviorMap,
        colours: &'a GroupColours,
        compared: Option<usize>,
        lit: &'a [usize],
    ) -> Scene<'a> {
        Scene {
            map,
            colours,
            compared,
            lit,
            ringed: None,
            hovered: None,
        }
    }

    #[test]
    fn the_picture_fills_the_maps_cells_at_the_cell_size() {
        assert_eq!(size((9, 21)), (576, 273));
        assert_eq!(size((18, 42)), (1152, 546));
        let map = sample().map;
        let theme = Theme::dark();
        let colours = GroupColours::new(&Kit::new(&theme), &map, &[], &Default::default());
        let bytes = png(&scene(&map, &colours, None, &[]), (18, 42));
        let image = decode(&bytes).expect("a PNG");
        assert_eq!((image.width, image.height), (1152, 546));
        assert_eq!(
            pixel(&image, (2.0, 2.0)),
            (31, 32, 47),
            "the map's own background"
        );
        // Pixels and places go both ways.
        let (x, y) = to_place(576.0, 273.0, (1152.0, 546.0));
        assert!((x - 0.5).abs() < 0.01 && (y - 0.5).abs() < 0.01);
    }

    /// As the header's mascot goes out: the kitty protocol's escapes over the map's 64 by 13
    /// cells, or iTerm2's inline image of the same size, each carrying the very PNG drawn.
    #[test]
    fn the_picture_goes_out_over_the_maps_cells_as_the_mascots_do() {
        use super::super::picture::escapes;
        use crate::mascot::Graphics;
        use crate::mascot::graphics::pictures_in;
        let map = sample().map;
        let theme = Theme::dark();
        let colours = GroupColours::new(&Kit::new(&theme), &map, &[], &Default::default());
        let shown = scene(&map, &colours, Some(1), &[]);
        for cell in [(9, 21), (18, 42)] {
            let picture = png(&shown, cell);
            let kitty = escapes(Graphics::Kitty, &shown, cell, 7);
            assert!(
                String::from_utf8_lossy(&kitty)
                    .starts_with("\x1b_Ga=T,f=100,q=2,C=1,c=64,r=13,i=7,")
            );
            assert_eq!(pictures_in(&kitty), std::slice::from_ref(&picture));
            let iterm = escapes(Graphics::Iterm, &shown, cell, 7);
            assert!(String::from_utf8_lossy(&iterm).contains(";width=64;height=13:"));
            assert_eq!(pictures_in(&iterm), [picture]);
        }
    }

    #[test]
    fn each_dot_is_its_groups_colour_and_red_shows_where_the_compared_group_acts_more() {
        let map = sample().more_at(1, (0.6, 0.8)).map;
        let theme = Theme::dark();
        let colours = GroupColours::new(&Kit::new(&theme), &map, &[], &Default::default());
        let plain = draw(&scene(&map, &colours, None, &[]), (18, 42));
        let first = &map.actions[17];
        let at = |x: f32, y: f32| {
            let margin = 12.0 * 1152.0 / 720.0;
            (
                margin + x * (1152.0 - 2.0 * margin),
                546.0 - margin - y * (546.0 - 2.0 * margin),
            )
        };
        assert_eq!(
            pixel(&plain, at(first.x, first.y)),
            (0xe0, 0x8a, 0x66),
            "Haiku 5.5's own"
        );
        let compared = draw(&scene(&map, &colours, Some(1), &[]), (18, 42));
        let (r, g, b) = pixel(&compared, at(0.63, 0.83));
        assert!(
            r > g && r > b && r > 60,
            "red behind Luna's git history: {r} {g} {b}"
        );
        let (r, g, b) = pixel(&plain, at(0.63, 0.83));
        assert_eq!(
            (r, g, b),
            (31, 32, 47),
            "nothing behind the dots when nothing is compared"
        );
        // Lit, the behavior's dots stay full and every other dot fades.
        let lit: Vec<usize> = (18..23).collect();
        let shown = draw(&scene(&map, &colours, Some(1), &lit), (18, 42));
        let (fr, _, _) = pixel(&shown, at(first.x, first.y));
        assert!(fr < 0xe0 / 2, "faded: {fr}");
        let luna = &map.actions[22];
        assert_eq!(pixel(&shown, at(luna.x, luna.y)), (0x6a, 0xa6, 0xff));
    }
}

/// The mouse in the middle of a braille cell stands for a place that the cell draws, and on
/// the picture the middle stands for the map's middle, where the margins are even.
#[test]
fn the_mouse_reads_as_a_place_on_the_map_as_each_layout_draws_it() {
    use crate::views::map::MapPoint;

    let point = |x: f32, y: f32, width: f32, height: f32| MapPoint {
        x,
        y,
        width,
        height,
    };
    let (x, y) = BehaviorsTab::place_of(point(10.5, 3.5, 64.0, 13.0), false);
    assert_eq!(
        ((x * 128.0) as usize / 2, ((1.0 - y) * 52.0) as usize / 4),
        (10, 3)
    );
    let (x, y) = BehaviorsTab::place_of(point(320.0, 130.0, 640.0, 260.0), true);
    assert!((x - 0.5).abs() < 1e-4 && (y - 0.5).abs() < 1e-4, "{x} {y}");
    // The margins leave the picture's corners just off the map.
    let (x, y) = BehaviorsTab::place_of(point(0.0, 260.0, 640.0, 260.0), true);
    assert!(x < 0.0 && y < 0.0, "{x} {y}");
}

#[test]
fn d_switches_between_the_label_map_and_a_kept_deeper_one() {
    use argonauts_core::behavior_map::Placing;
    use argonauts_core::settings::behaviors::BehaviorMap as Which;

    let mut labels = sample().map;
    labels.offer_deeper = false;
    let mut tab = BehaviorsTab::default();
    tab.accept(&labels);
    let asked = press(&mut tab, &[KeyCode::Char('d')]);
    let Some(MapKey::Ask(ViewRequest::Behaviors(request))) = asked else {
        panic!("{asked:?}")
    };
    assert_eq!(request.map, Some(Which::Deeper));
    let mut deeper = sample().map;
    deeper.request = request;
    deeper.placing = Placing::Summaries {
        by: "Qwen3 Coder 30B on Ollama".to_owned(),
    };
    deeper.by_words = true;
    tab.accept(&deeper);
    let line = shown(&tab, 40, false).remove(0);
    assert!(
        line.ends_with("by model · placed by word vectors"),
        "{line}"
    );
    let back = press(&mut tab, &[KeyCode::Char('d')]);
    assert!(
        matches!(back, Some(MapKey::Ask(ViewRequest::Behaviors(request))) if request.map == Some(Which::Labels)),
        "{back:?}"
    );
}

#[test]
fn x_stops_an_update_the_app_reports() {
    let mut tab = opened();
    assert!(!tab.claims(KeyCode::Char('x'), false));
    let update = crate::state::DeepUpdate {
        running: Some(argonauts_core::deep_map::DeepProgress {
            done: 3,
            total: 37,
            seconds_left: None,
            summarizer: "Qwen3 Coder 30B on Ollama".to_owned(),
        }),
        ..Default::default()
    };
    tab.deep.sync(&update);
    assert!(tab.claims(KeyCode::Char('x'), false));
    assert_eq!(
        press(&mut tab, &[KeyCode::Char('x')]),
        Some(MapKey::Send(argonauts_core::app::UiCommand::DeepStop))
    );
    let rows = shown(&tab, 40, false);
    assert!(
        rows[0].contains("summarizing with Qwen3 Coder 30B"),
        "{rows:?}"
    );
    assert!(rows[2].contains("3 of 37"), "{rows:?}");
}

/// Luna's list with counts too wide for a narrow screen's behavior column, as the real records
/// give it: 34 of 66 answers against the others' 135.
fn wide_counts() -> BehaviorsTab {
    let sample = super::sample::Sample::new(argonauts_core::behavior_map::MapRequest::default())
        .model("Haiku 5.5", "claude-code", "claude-haiku-5-5")
        .model("GPT 6 Luna", "codex", "gpt-6-luna")
        .actions(0, "returned its structured answer", 135, (0.8, 0.2))
        .actions(1, "returned its structured answer", 34, (0.8, 0.2))
        .actions(
            1,
            "checked git status or diff and read git history",
            32,
            (0.7, 0.8),
        )
        .rows(
            1,
            &[
                ("checked git status or diff and read git history", 32, 0),
                ("returned its structured answer", 34, 135),
            ],
        );
    let mut tab = BehaviorsTab::default();
    tab.accept(&sample.map);
    press(&mut tab, &[KeyCode::Right, KeyCode::Right, KeyCode::Down]);
    tab
}

#[test]
fn on_a_narrow_screen_the_counts_show_whole_and_the_behaviors_give_way() {
    let tab = wide_counts();
    for (width, pictures) in [(80, true), (80, false), (60, false)] {
        let lines = shown_at(&tab, width, 60, pictures);
        let answer = lines
            .iter()
            .find(|line| line.contains("(51.5%)"))
            .unwrap_or_else(|| panic!("{width}: {lines:#?}"));
        assert!(
            answer.contains(" 34 (51.5%)   others 135 (100.0%)"),
            "{answer}"
        );
        assert!(answer.width() <= width, "{width}: {answer:?}");
        let history = lines
            .iter()
            .find(|line| line.contains("others   0"))
            .unwrap();
        assert!(history.contains('…'), "the words give way: {history}");
        // Narrower than the map itself, only the map's own rows may run past the edge.
        if width >= 80 {
            for line in &lines {
                assert!(line.width() <= width, "{width}: {line:?}");
            }
        }
    }
}

#[test]
fn a_screen_too_narrow_for_the_legend_beside_the_picture_has_it_under_the_map() {
    let tab = wide_counts();
    let wide = shown_at(&tab, 118, 60, true);
    let beside = wide
        .iter()
        .find(|line| line.contains("Comparing GPT 6 Luna"))
        .unwrap();
    assert!(
        beside.starts_with(&" ".repeat(60)),
        "beside the map: {beside:?}"
    );
    let narrow = shown_at(&tab, 80, 60, true);
    assert!(
        !narrow.iter().any(|line| line.contains("Comparing")),
        "no legend squeezed beside the map: {narrow:#?}"
    );
    let legend = narrow
        .iter()
        .position(|line| line.contains("⣿ Haiku 5.5"))
        .expect("the legend under the map");
    assert!(legend > super::MAP_ROWS, "{narrow:#?}");
    assert!(
        narrow[legend + 1].contains("GPT 6 Luna does this more"),
        "{narrow:#?}"
    );
    // The lit line fits too, cut with `…` rather than in the middle of a name.
    let lit = narrow
        .iter()
        .find(|line| line.starts_with(" lit:"))
        .unwrap();
    assert!(lit.width() <= 80, "{lit}");
}

#[test]
fn with_no_action_the_tab_says_what_shows_more_and_offers_nothing() {
    use argonauts_core::behavior_map::MapRequest;
    let empty = |request: MapRequest| {
        let mut map = super::sample::Sample::new(request).map;
        map.runs = 0;
        map.projects.clear();
        map.here = Some("council".to_owned());
        let mut tab = BehaviorsTab {
            request,
            ..BehaviorsTab::default()
        };
        tab.accept(&map);
        tab
    };
    let mut tab = empty(MapRequest::default());
    let lines = shown(&tab, 40, true);
    assert_eq!(
        lines[0].trim_end(),
        " No actions recorded yet: the map fills in as your agents work"
    );
    assert!(
        !lines.iter().any(|line| line.contains("Pick a model")),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("Deeper map")),
        "{lines:#?}"
    );
    assert_eq!(tab.keys(118).join(" · "), "g group · p period · esc back");
    assert_eq!(
        press(&mut tab, &[KeyCode::Char('d')]),
        None,
        "nothing to summarize"
    );
    let week = empty(MapRequest {
        period: OverviewPeriod::Week,
        ..MapRequest::default()
    });
    assert_eq!(
        shown(&week, 40, true)[0].trim_end(),
        " No actions in the last 7 days · p changes the period"
    );
    let here = empty(MapRequest {
        scope: Scope::ThisProject,
        ..MapRequest::default()
    });
    assert_eq!(
        shown(&here, 40, true)[0].trim_end(),
        " No actions recorded in council yet · r shows every project"
    );
}

#[test]
fn a_group_named_for_its_cli_keeps_its_whole_name_and_the_compared_one_always_shows() {
    let mut sample =
        super::sample::Sample::new(argonauts_core::behavior_map::MapRequest::default());
    for at in 0..9 {
        sample = sample.model(&format!("Model {at}"), "codex", &format!("model-{at}"));
    }
    let mut map = sample.map;
    map.groups[8].name = "Claude Code".to_owned();
    map.groups[8].model = None;
    for group in 0..9 {
        map.actions.push(super::sample::action(
            group,
            "read a source file",
            (0.5, 0.5),
        ));
        map.groups[group].actions = 9 - group;
    }
    let mut tab = BehaviorsTab::default();
    tab.accept(&map);
    for _ in 0..9 {
        press(&mut tab, &[KeyCode::Right]);
    }
    assert_eq!(tab.compared(), Some(8));
    let lines = shown(&tab, 40, true);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("red: Claude Code does this more")),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("● Claude Code")),
        "{lines:#?}"
    );
    assert!(lines.iter().any(|line| line.contains("and ")), "{lines:#?}");
}

#[test]
fn a_new_map_for_the_same_request_keeps_the_card_open() {
    let mut tab = opened();
    press(
        &mut tab,
        &[
            KeyCode::Right,
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Enter,
        ],
    );
    assert!(matches!(tab.at, At::Card { .. }));
    let card = tab.at;
    tab.accept(&sample().map);
    assert_eq!(
        tab.at, card,
        "an update in the background leaves the card as it was"
    );
    let mut other = sample().map;
    other.request.grouping = Grouping::Cli;
    tab.request = other.request;
    tab.accept(&other);
    assert_eq!(tab.at, At::Map, "another request starts on the map");
}

#[test]
fn while_the_deeper_maps_questions_show_the_keys_are_theirs() {
    let mut map = sample().map;
    map.offer_deeper = true;
    let mut tab = BehaviorsTab::default();
    tab.accept(&map);
    press(&mut tab, &[KeyCode::Char('d')]);
    tab.accept_offer(&crate::questions::deep_map::sample::page_offer());
    assert_eq!(
        tab.keys(118).join(" · "),
        "↑  ↓  choose · enter answer · esc back"
    );
}

#[test]
fn the_list_scrolls_past_the_rows_it_shows_and_says_how_many_more() {
    let names: Vec<String> = (0..9).map(|n| format!("behavior number {n}")).collect();
    let mut sample =
        super::sample::Sample::new(argonauts_core::behavior_map::MapRequest::default())
            .model("Haiku 5.5", "claude-code", "claude-haiku-5-5")
            .model("GPT 6 Luna", "codex", "gpt-6-luna");
    for (at, name) in names.iter().enumerate() {
        sample = sample.actions(0, name, 3 + at, (0.1 * at as f32, 0.5));
    }
    sample = sample.actions(1, "read git history", 5, (0.9, 0.9));
    let rows: Vec<(&str, usize, usize)> = names
        .iter()
        .enumerate()
        .map(|(at, name)| (name.as_str(), 3 + at, 0))
        .collect();
    let sample = sample.rows(0, &rows);
    let mut tab = BehaviorsTab::default();
    tab.accept(&sample.map);
    press(&mut tab, &[KeyCode::Right, KeyCode::Down]);
    let lines = shown(&tab, 60, true);
    assert!(
        lines
            .iter()
            .any(|line| line.trim_end() == "   3 more below"),
        "{lines:#?}"
    );
    press(&mut tab, &[KeyCode::Down; 8]);
    assert_eq!(tab.at, At::List(8));
    let lines = shown(&tab, 60, true);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with(" ❯ behavior number 8")),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.trim_end() == "   3 more above"),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with(" lit: behavior number 8")),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("behavior number 2 ")),
        "scrolled past"
    );
    press(&mut tab, &[KeyCode::Up; 8]);
    let lines = shown(&tab, 60, true);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with(" ❯ behavior number 0")),
        "{lines:#?}"
    );
}
