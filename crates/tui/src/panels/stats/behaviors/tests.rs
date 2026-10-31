use std::time::Instant;

use argonauts_core::app::ViewRequest;
use argonauts_core::app::overview::OverviewPeriod;
use argonauts_core::behaviors::behavior_map::{Grouping, Scope};
use crossterm::event::KeyCode;
use unicode_width::UnicodeWidthStr;

use super::sample::Sample;
use super::{At, BehaviorsTab, GroupColours, Level, MapKey, Stop};
use crate::base::kit::Kit;
use crate::base::theme::{Glyphs, Theme};
use crate::frame::view::map::MapPoint;
use crate::world::Pointed;

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

/// The cell the tab is drawn at here, 18 × 42 px, where the map is 64 × 13 in its usual room.
const DRAWN_AT: (u16, u16) = (18, 42);

/// The tab's rows in a 120-column window, the tab one cell in.
fn shown(tab: &BehaviorsTab, room: usize, pictures: bool) -> Vec<String> {
    shown_at(tab, 119, room, pictures)
}

fn shown_at(tab: &BehaviorsTab, width: usize, room: usize, pictures: bool) -> Vec<String> {
    shown_drawn(tab, width, room, pictures, pictures)
}

/// The tab's rows `width` wide in `room` rows, the world drawn as pictures or in braille, and
/// logos drawn as pictures, written as their stand-ins `◧◨`, or as colour dots.
fn shown_drawn(
    tab: &BehaviorsTab,
    width: usize,
    room: usize,
    pictures: bool,
    logo_pictures: bool,
) -> Vec<String> {
    let mut tab = tab.clone();
    tab.picture_placed = pictures;
    tab.cell = Some(DRAWN_AT);
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme.mascot_pictures = pictures;
    theme.logo_pictures = logo_pictures;
    let kit = Kit::new(&theme);
    let map = tab.map().unwrap();
    let colours = GroupColours::new(&kit, map, &[], &Default::default());
    let lines: Vec<_> = (tab
        .blocks(&kit, &colours, &Default::default(), width, room)
        .iter())
    .flat_map(|block| block.lines.iter().cloned())
    .collect();
    crate::base::kit::logo::golden::lines_with_stand_ins(&lines)
        .iter()
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
    assert!(lines[0].starts_with(" 1 Map · 2 Planet · 3 Universe   "));
    assert!(
        lines[0].ends_with(" Labels · Summaries"),
        "the switch between the maps at the levels' right: {lines:#?}"
    );
    assert!(lines.iter().any(|l| l.ends_with("Every model")));
    assert!(lines.iter().any(|l| l.ends_with("◧◨ Haiku 5.5   18")));
    assert!(lines.contains(&" Point at anything on the map to see what it is.".to_owned()));
    let headline = " Label map · 28 actions in 19 runs across 6 projects";
    let at = lines
        .iter()
        .position(|l| l == headline)
        .expect("the headline");
    assert_eq!(lines[at + 1], " Most common", "{lines:#?}");
    assert!(
        lines[at + 2].starts_with(" Read a source file "),
        "{lines:#?}"
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
    let said: Vec<&String> = lines.iter().filter(|l| l.contains("too few to")).collect();
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
    assert!(lines.iter().any(|l| l.ends_with("○  ◧◨ Haiku 5.5   18")));
    assert!(
        !lines.iter().any(|l| l.contains("Much more")),
        "the land stays untinted where nothing can be compared"
    );
}

#[test]
fn summaries_are_offered_on_the_map_and_give_their_rows_to_the_list() {
    let mut map = sample().map;
    map.offer_summaries = true;
    let mut tab = BehaviorsTab::default();
    tab.accept(&map);
    press(&mut tab, &[KeyCode::Right]);
    let offered = |tab: &BehaviorsTab| {
        shown(tab, 40, true)
            .iter()
            .any(|line| line.contains("✻  Summaries: "))
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
        lines.contains(&" Lit: read a source file · 17 actions, 12 of them Haiku 5.5's".to_owned())
    );
    assert!(lines.iter().any(|l| l.starts_with(" ❯ Read a source file")));
    press(&mut tab, &[KeyCode::Up, KeyCode::Up]);
    assert_eq!(tab.at, At::Map);
    press(&mut tab, &[KeyCode::Down, KeyCode::Esc]);
    assert_eq!(tab.at, At::Map);
    assert!(
        tab.claims(KeyCode::Up, true),
        "↑ on the map goes up to the groupings"
    );
    press(&mut tab, &[KeyCode::Up]);
    assert_eq!(tab.stop(), Stop::Groupings);
    assert!(
        !tab.claims(KeyCode::Up, true),
        "↑ on the groupings is the panel's"
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
        lines.contains(&" ◧◨ 6 Luna · read git history · 5 times".to_owned()),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with(" ◉ Read git history · ◧◨ 6 Luna · build:per-api-key")),
        "the ringed action said in the pointer's place: {lines:#?}"
    );
    press(&mut tab, &[KeyCode::Down, KeyCode::Down, KeyCode::Enter]);
    assert_eq!(tab.at, At::Card { row: 0, action: 2 });
    let card = shown(&tab, 40, true);
    assert_eq!(
        card[0],
        " build:per-api-key · ◧◨ GPT 6 Luna on Codex · builder · in the run's worktree"
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

/// Tab steps the groupings, the panel's `r` and Shift+R step the period, and `s` switches
/// between every project and this one, each asking for a new map from the top. `p` and `r` are
/// the panel's, and `g` is the panel's Home, never a grouping.
#[test]
fn tab_s_and_the_period_ask_for_a_new_map_from_the_top() {
    let mut tab = opened();
    press(&mut tab, &[KeyCode::Right, KeyCode::Down]);
    let asked = |key| match key {
        Some(MapKey::Ask(ViewRequest::Behaviors(request))) => request,
        other => panic!("{other:?}"),
    };
    let request = asked(press(&mut tab, &[KeyCode::Tab]));
    assert_eq!(request.grouping, Grouping::Cli);
    assert_eq!(tab.at, At::Map);
    let back = asked(press(&mut tab, &[KeyCode::BackTab]));
    assert_eq!(back.grouping, Grouping::Model);
    assert!(!tab.claims(KeyCode::Char('g'), true), "g is Home");
    assert_eq!(asked(tab.step_period(1)).period, OverviewPeriod::Week);
    assert_eq!(asked(tab.step_period(-1)).period, OverviewPeriod::All);
    assert_eq!(asked(tab.step_period(-1)).period, OverviewPeriod::Month);
    assert_eq!(
        asked(press(&mut tab, &[KeyCode::Char('s')])).scope,
        Scope::ThisProject
    );
    assert!(tab.keys(118).contains(&"s all projects".to_owned()));
    for key in ['p', 'r', 'R', 'g', 'G'] {
        assert!(!tab.claims(KeyCode::Char(key), true), "{key}");
    }
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
    assert!(roomy.iter().any(|l| l.starts_with("   Read a source file")));
    // In a panel cut short of the rows it could have, the levels, a blank, the map, a blank,
    // the pointer's two rows and the headline leave the list two rows: its heading gives way
    // so both its behaviors show.
    tab.size_map_for(40);
    let tight = shown(&tab, 1 + 1 + 13 + 1 + 2 + 1 + 2, true);
    for behavior in ["Edited a source file", "Read a source file"] {
        assert!(
            tight
                .iter()
                .any(|l| l.starts_with(&format!("   {behavior}"))),
            "{tight:#?}"
        );
    }
    assert!(
        !tight.iter().any(|l| l.contains("Distinctive to")),
        "{tight:#?}"
    );
    let roomier = shown(&tab, 1 + 1 + 13 + 1 + 2 + 1 + 3, true);
    assert!(
        roomier.iter().any(|l| l.contains("Distinctive to")),
        "{roomier:#?}"
    );
}

#[test]
fn the_mouse_rings_the_nearest_dot_and_a_click_opens_its_card() {
    let mut tab = opened();
    assert!(tab.point_at(Some((0.61, 0.8))));
    let lit = tab.hovered().unwrap();
    assert_eq!(tab.map().unwrap().actions[lit].behavior, "read git history");
    assert!(!tab.point_at(Some((0.61, 0.8))), "the same dot");
    // The world says what the dot is in its own words; braille as its map always has, with
    // the hint under it.
    let lines = shown(&tab, 40, true);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with(" ◌ Read git history · ◧◨ 6 Luna · 5 of its 7 · everyone 5 · ")),
        "{lines:#?}"
    );
    let braille = shown(&tab, 40, false);
    assert!(
        braille
            .iter()
            .any(|l| l.starts_with(" ◉ Read git history · GPT 6 Luna"))
    );
    let hint =
        "   The map stays as it was; move away and the ring goes; click opens its card".to_owned();
    assert!(braille.contains(&hint));
    let mut compared = tab.clone();
    press(&mut compared, &[KeyCode::Right]);
    compared.point_at(Some((0.61, 0.8)));
    for pictures in [true, false] {
        assert_eq!(
            shown(&compared, 40, pictures).len(),
            shown(&opened_compared(), 40, pictures).len(),
            "as many rows as without the mouse"
        );
    }
    assert!(tab.click());
    assert!(matches!(tab.at, At::Card { row: 0, .. }));
    assert_eq!(tab.compared(), Some(1), "the dot's own group");
    tab.at = At::Map;
    tab.point_at(Some((0.2, 0.7)));
    assert!(tab.point_at(None));
    assert_eq!(tab.hovered(), None);
}

/// Without pictures the map is braille with its legend beside it: what is compared, what the
/// shading says, and every group with its count and its whole name; under it on a screen too
/// narrow to hold it there.
#[test]
fn without_pictures_the_map_is_braille_with_its_legend_beside_it() {
    let mut tab = opened();
    press(&mut tab, &[KeyCode::Right, KeyCode::Right]);
    let lines = shown(&tab, 40, false);
    let map: Vec<&String> = lines[0..13].iter().collect();
    assert!(
        map.iter()
            .any(|l| l.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)))
    );
    let beside = |row: usize| lines[row].chars().skip(67).collect::<String>();
    assert!(beside(0).starts_with("Comparing GPT 6 Luna"), "{lines:#?}");
    assert!(beside(0).ends_with("Actions"));
    assert_eq!(beside(1), "With the other models");
    assert_eq!(beside(2), "   Behind the dots: GPT 6 Luna does this more");
    assert_eq!(beside(3), "   Behind the dots: GPT 6 Luna does this less");
    assert!(beside(6).starts_with("● GPT 6 Luna") && beside(6).ends_with(" 7"));
    // Too narrow for the legend beside the map's share of the width.
    let narrow = shown_at(&tab, 56, 40, false);
    let legend = narrow
        .iter()
        .position(|line| line.starts_with(" ⣿ Haiku 5.5"))
        .unwrap_or_else(|| panic!("{narrow:#?}"));
    assert!(narrow[legend].starts_with(" ⣿ Haiku 5.5  ⣿ GPT 6 Luna  ⣿ Gemini 3.8 Flash"));
    assert!(
        narrow[legend + 1].starts_with(" Red behind the dots:"),
        "{narrow:#?}"
    );
}

#[test]
fn the_keys_say_what_each_place_takes_each_arrow_followed_by_two_spaces() {
    let mut tab = opened();
    assert_eq!(
        tab.keys(160).join(" · "),
        "r to cycle dates · s this project · d summaries · tab grouping · ←  →  compare a model · \
         ↓  into the list, which lights each behavior · esc back"
    );
    assert_eq!(
        tab.keys(118).join(" · "),
        "r to cycle dates · s this project · d summaries · tab grouping · ←  →  compare · ↓  the \
         list · esc back",
        "the short words where the long ones don't fit"
    );
    press(&mut tab, &[KeyCode::Right, KeyCode::Down]);
    assert_eq!(
        tab.keys(160).join(" · "),
        "r to cycle dates · s this project · d summaries · ↑  ↓  behavior, lit on the map · enter \
         its actions · ←  →  model · esc back"
    );
    press(&mut tab, &[KeyCode::Enter]);
    assert_eq!(
        tab.keys(118).join(" · "),
        "r to cycle dates · s this project · d summaries · ↑  ↓  example · enter opens it · esc \
         back"
    );
    press(&mut tab, &[KeyCode::Enter]);
    assert_eq!(
        tab.keys(118).join(" · "),
        "r to cycle dates · s this project · ←  →  example · o opens the run · c copies the action \
         · esc back",
        "a card hides the map, so d waits"
    );
}

#[test]
fn d_switches_between_the_label_map_and_kept_summaries() {
    use argonauts_core::behaviors::behavior_map::Placing;
    use argonauts_core::settings::behaviors::BehaviorMap as Which;

    let mut labels = sample().map;
    labels.offer_summaries = false;
    let mut tab = BehaviorsTab::default();
    tab.accept(&labels);
    let asked = press(&mut tab, &[KeyCode::Char('d')]);
    let Some(MapKey::Ask(ViewRequest::Behaviors(request))) = asked else {
        panic!("{asked:?}")
    };
    assert_eq!(request.map, Some(Which::Summaries));
    let mut summaries = sample().map;
    summaries.request = request;
    summaries.placing = Placing::Summaries {
        by: "Qwen3 Coder 30B on Ollama".to_owned(),
    };
    summaries.by_words = true;
    tab.accept(&summaries);
    let lines = shown(&tab, 40, false);
    assert!(
        lines.iter().any(|line| line.ends_with(
            "Summaries · 28 summarized by Qwen3 Coder 30B on Ollama · placed by word vectors"
        )),
        "{lines:#?}"
    );
    let back = press(&mut tab, &[KeyCode::Char('d')]);
    assert!(
        matches!(back, Some(MapKey::Ask(ViewRequest::Behaviors(request))) if request.map == Some(Which::Labels)),
        "{back:?}"
    );
}

/// The headline names the map on screen and what it's drawn from, Summaries with how many of
/// the actions they hold and how many newer wait; the key line says what `d` switches to.
#[test]
fn the_headline_and_d_follow_the_map_on_screen() {
    use argonauts_core::behaviors::behavior_map::Placing;

    let mut map = sample().map;
    map.offer_summaries = true;
    let mut tab = BehaviorsTab::default();
    tab.accept(&map);
    assert!(tab.own_keys().contains(&"d set up summaries".to_owned()));
    map.offer_summaries = false;
    tab.accept(&map);
    assert!(tab.own_keys().contains(&"d summaries".to_owned()));
    let lines = shown(&tab, 40, true);
    assert!(
        lines.contains(&" Label map · 28 actions in 19 runs across 6 projects".to_owned()),
        "{lines:#?}"
    );
    map.placing = Placing::Summaries {
        by: "Qwen3 Coder 30B (64k)".to_owned(),
    };
    map.unsummarized = 5;
    tab.accept(&map);
    assert!(tab.own_keys().contains(&"d labels or update".to_owned()));
    let lines = shown(&tab, 40, true);
    assert!(
        lines.contains(
            &" Summaries · 28 of 33 summarized by Qwen3 Coder 30B (64k) · 5 newer not yet"
                .to_owned()
        ),
        "{lines:#?}"
    );
    assert!(lines[0].ends_with(" Labels · Summaries"), "{lines:#?}");
    map.unsummarized = 0;
    tab.accept(&map);
    assert!(tab.own_keys().contains(&"d labels".to_owned()));
    let lines = shown(&tab, 40, true);
    assert!(
        lines.contains(&" Summaries · 28 summarized by Qwen3 Coder 30B (64k)".to_owned()),
        "{lines:#?}"
    );
}

/// In the list the lit behavior is said where the pointer's line was, so the list keeps its
/// rows: the headline, then the list's heading and its rows.
#[test]
fn the_lit_behavior_is_said_in_the_pointers_place() {
    let mut tab = opened();
    press(&mut tab, &[KeyCode::Right, KeyCode::Down, KeyCode::Down]);
    let lines = shown(&tab, 40, true);
    let lit = lines
        .iter()
        .position(|l| l.starts_with(" Lit: read a source file"))
        .expect("the lit line");
    assert!(lines[lit - 1].is_empty(), "under the map: {lines:#?}");
    assert!(lines[lit + 2].starts_with(" Label map · "), "{lines:#?}");
    assert!(lines[lit + 3].starts_with(" Distinctive to "), "{lines:#?}");
    assert!(
        !lines.iter().any(|l| l.starts_with(" Point at anything")),
        "{lines:#?}"
    );
}

/// Summaries list each summary alone, its label left for the pointer's line and Enter's; a
/// summary too long for the row ends in `…` with a blank after it.
#[test]
fn summaries_list_each_summary_alone_cut_to_the_row() {
    use argonauts_core::behaviors::behavior_map::Placing;

    let mut map = sample().map;
    map.placing = Placing::Summaries {
        by: "Qwen3 Coder 30B (64k)".to_owned(),
    };
    let long =
        "Reading every file under the project to learn how the spend card measures its columns";
    for row in &mut map.comparisons[0].rows {
        row.label = Some("read a source file".to_owned());
    }
    map.comparisons[0].rows[0].behavior = long.to_owned();
    let mut tab = BehaviorsTab::default();
    tab.accept(&map);
    press(&mut tab, &[KeyCode::Right, KeyCode::Down]);
    let width = 60;
    let lines = shown_at(&tab, width, 40, true);
    let row = lines
        .iter()
        .find(|l| l.starts_with(" ❯ "))
        .expect("the cursor's row");
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("label:") && l.starts_with("   ")),
        "{lines:#?}"
    );
    assert!(row.ends_with('…'), "{row:?}");
    assert!(row.width() < width, "a blank after the cut: {row:?}");
}

#[test]
fn x_stops_an_update_the_app_reports() {
    let mut tab = opened();
    assert!(!tab.claims(KeyCode::Char('x'), false));
    let update = crate::frame::state::DeepUpdate {
        running: Some(argonauts_core::behaviors::deep_map::DeepProgress {
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
    let sample =
        super::sample::Sample::new(argonauts_core::behaviors::behavior_map::MapRequest::default())
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
            answer.contains(" 34  (51.5%)   others 135  (100.0%)"),
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
        .find(|line| line.contains("Comparing ◧◨ 6 Luna"))
        .unwrap();
    assert!(
        beside.starts_with(&" ".repeat(60)),
        "beside the map: {beside:?}"
    );
    // Too narrow for the legend beside the map's share of the width.
    let narrow = shown_at(&tab, 56, 60, true);
    let legend = narrow
        .iter()
        .position(|line| line.contains("Comparing"))
        .expect("the legend under the map");
    // The levels row and a blank above the map, four rows under it.
    let map_rows = super::Fit::for_room(56, 60 - 2 - 4, 4, DRAWN_AT).rows;
    assert!(
        legend > map_rows,
        "no legend squeezed beside the map: {narrow:#?}"
    );
    assert!(
        narrow[legend].starts_with(" Comparing ◧◨ 6 Luna"),
        "{narrow:#?}"
    );
    // The lit line fits too, cut with `…` rather than in the middle of a name.
    let lit = narrow
        .iter()
        .find(|line| line.starts_with(" Lit:"))
        .unwrap();
    assert!(lit.width() <= 56, "{lit}");
}

#[test]
fn with_no_action_the_tab_says_what_shows_more_and_offers_nothing() {
    use argonauts_core::behaviors::behavior_map::MapRequest;
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
    let headline = |tab: &BehaviorsTab| {
        let lines = shown(tab, 40, true);
        lines
            .iter()
            .find(|line| line.starts_with(" No actions"))
            .cloned()
            .unwrap_or_else(|| panic!("{lines:#?}"))
    };
    let mut tab = empty(MapRequest::default());
    let lines = shown(&tab, 40, true);
    assert_eq!(
        headline(&tab),
        " No actions recorded yet: the map fills in as your agents work"
    );
    assert!(
        !lines.iter().any(|line| line.contains("Most common")),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("Pick a model")),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("Summaries map")),
        "{lines:#?}"
    );
    assert_eq!(
        tab.keys(118).join(" · "),
        "r to cycle dates · s this project · esc back"
    );
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
        headline(&week),
        " No actions in the last 7 days · r changes the period"
    );
    let here = empty(MapRequest {
        scope: Scope::ThisProject,
        ..MapRequest::default()
    });
    assert_eq!(
        headline(&here),
        " No actions recorded in council yet · s shows every project"
    );
}

#[test]
fn a_group_named_for_its_cli_keeps_its_whole_name_and_the_compared_one_always_shows() {
    let mut sample =
        super::sample::Sample::new(argonauts_core::behaviors::behavior_map::MapRequest::default());
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
    let lines = shown(&tab, 40, false);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Behind the dots: Claude Code does this more")),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("● Claude Code")),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line.trim() == "↑  2 more above"),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("and ")),
        "{lines:#?}"
    );
}

/// Ten models, as many as the legend has never had room for beside the map.
fn ten_models() -> BehaviorsTab {
    let mut sample =
        super::sample::Sample::new(argonauts_core::behaviors::behavior_map::MapRequest::default());
    for at in 0..10 {
        sample = sample.model(&format!("Model {at}"), "codex", &format!("model-{at}"));
    }
    let mut map = sample.map;
    for group in 0..10 {
        map.actions.push(super::sample::action(
            group,
            "read a source file",
            (0.5, 0.5),
        ));
        map.groups[group].actions = 10 - group;
    }
    let mut tab = BehaviorsTab::default();
    tab.accept(&map);
    tab
}

/// The braille legend's rows after its heading: each group shown and the marks of those above
/// and below.
fn legend_rows(tab: &BehaviorsTab) -> Vec<String> {
    shown(tab, 40, false)
        .iter()
        .map(|line| line.chars().skip(67).collect::<String>().trim().to_owned())
        .filter(|line| {
            line.starts_with('●')
                || line.starts_with('○')
                || line.ends_with(" more above")
                || line.ends_with(" more below")
        })
        .collect()
}

/// Beside the braille map the legend lists every group: those its rows hold, then how many more
/// are below, and as ← → step through every one, it scrolls to keep the compared one in view
/// with how many are above.
#[test]
fn the_braille_legend_lists_every_group_and_scrolls_to_the_compared_one() {
    let mut tab = ten_models();
    let rows = legend_rows(&tab);
    assert_eq!(
        rows.len(),
        10,
        "every one fits with nothing compared: {rows:#?}"
    );
    assert!(rows[0].starts_with("● Model 0"), "{rows:#?}");
    assert!(rows[9].starts_with("● Model 9"), "{rows:#?}");
    for group in 0..10 {
        press(&mut tab, &[KeyCode::Right]);
        assert_eq!(tab.compared(), Some(group));
        let rows = legend_rows(&tab);
        assert!(
            rows.iter()
                .any(|row| row.starts_with(&format!("● Model {group} "))),
            "{group}: {rows:#?}"
        );
        let count = |row: &String| {
            row.strip_suffix(" more above")
                .or_else(|| row.strip_suffix(" more below"))
                .map(|n| n.trim_start_matches(['↑', '↓', ' ']))
                .and_then(|n| n.parse::<usize>().ok())
        };
        let marked: usize = rows.iter().filter_map(count).sum();
        let listed = rows.iter().filter(|row| count(row).is_none()).count();
        assert_eq!(marked + listed, 10, "{group}: {rows:#?}");
        assert!(rows.len() <= 8, "{group}: {rows:#?}");
    }
    let rows = legend_rows(&tab);
    assert_eq!(rows[0], "↑  3 more above", "{rows:#?}");
    assert!(rows[7].starts_with("● Model 9"), "{rows:#?}");
    // Round again to every group alike, and back with ← to the last one.
    press(&mut tab, &[KeyCode::Right]);
    assert_eq!(tab.compared(), None);
    press(&mut tab, &[KeyCode::Left]);
    assert_eq!(tab.compared(), Some(9));
}

/// Under a braille map every group shows too: those a row can't hold go on to the next, each
/// with its mark.
#[test]
fn the_braille_legend_lists_every_group_on_as_many_rows_as_it_takes() {
    let tab = ten_models();
    let lines = shown_at(&tab, 56, 40, false);
    let legend: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("⣿ Model"))
        .collect();
    assert_eq!(legend.len(), 2, "{lines:#?}");
    for group in 0..10 {
        assert!(
            legend
                .iter()
                .any(|line| line.contains(&format!("⣿ Model {group}"))),
            "{group}: {legend:#?}"
        );
    }
    assert!(legend.iter().all(|line| line.width() <= 80));
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
fn while_the_summaries_questions_show_the_keys_are_theirs() {
    let mut map = sample().map;
    map.offer_summaries = true;
    let mut tab = BehaviorsTab::default();
    tab.accept(&map);
    press(&mut tab, &[KeyCode::Char('d')]);
    tab.accept_offer(&crate::panels::stats::deep_map::sample::page_offer());
    assert_eq!(
        tab.keys(118).join(" · "),
        "↑  ↓  choose · enter answer · esc back"
    );
}

/// Where the list has two rows, the heading gives way to the cursor's row and the mark that
/// counts what it hides; from three rows up the heading stands over both.
#[test]
fn a_list_in_two_rows_keeps_the_cursor_and_its_mark_over_its_heading() {
    let names: Vec<String> = (0..9).map(|n| format!("behavior number {n}")).collect();
    let mut sample =
        super::sample::Sample::new(argonauts_core::behaviors::behavior_map::MapRequest::default())
            .model("Haiku 5.5", "claude-code", "claude-haiku-5-5")
            .model("GPT 6 Luna", "codex", "gpt-6-luna");
    for (at, name) in names.iter().enumerate() {
        sample = sample.actions(0, name, 3 + at, (0.1 * at as f32, 0.5));
    }
    let rows: Vec<(&str, usize, usize)> = names
        .iter()
        .enumerate()
        .map(|(at, name)| (name.as_str(), 3 + at, 0))
        .collect();
    let sample = sample.rows(0, &rows);
    let mut tab = BehaviorsTab::default();
    tab.accept(&sample.map);
    press(&mut tab, &[KeyCode::Right, KeyCode::Down, KeyCode::Down]);
    let theme = Theme::dark();
    let kit = Kit::new(&theme);
    let map = tab.map().unwrap();
    let colours = GroupColours::new(&kit, map, &[], &Default::default());
    let names = Default::default();
    let look = tab.look(&kit, map, &colours, &names, 119);
    let listed = |room: usize| -> Vec<String> {
        tab.listed(&look, room)
            .lines
            .iter()
            .map(|line| line.to_string().trim_end().to_owned())
            .collect()
    };
    let mark = " ↑  1 more above · ↓  7 more below";
    let two = listed(2);
    assert_eq!(two.len(), 2, "{two:#?}");
    assert!(two[0].starts_with(" ❯ Behavior number 1 "), "{two:#?}");
    assert_eq!(two[1], mark, "{two:#?}");
    let three = listed(3);
    assert_eq!(three.len(), 3, "{three:#?}");
    assert!(three[0].starts_with(" Distinctive to "), "{three:#?}");
    assert!(three[1].starts_with(" ❯ Behavior number 1 "), "{three:#?}");
    assert_eq!(three[2], mark, "{three:#?}");
}

#[test]
fn the_list_scrolls_past_the_rows_it_shows_and_says_how_many_more() {
    let names: Vec<String> = (0..9).map(|n| format!("behavior number {n}")).collect();
    let mut sample =
        super::sample::Sample::new(argonauts_core::behaviors::behavior_map::MapRequest::default())
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
    // The levels, a blank, the map, a blank, the pointer's two rows and the headline leave the
    // list its heading and six rows.
    let room = 1 + 1 + 13 + 1 + 2 + 1 + 1 + 6;
    let lines = shown(&tab, room, true);
    assert!(
        lines.iter().any(|line| line == " ↓  4 more below"),
        "five rows and the mark: {lines:#?}"
    );
    press(&mut tab, &[KeyCode::Down; 8]);
    assert_eq!(tab.at, At::List(8));
    let lines = shown(&tab, room, true);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with(" ❯ Behavior number 8")),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line == " ↑  4 more above"),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with(" Lit: behavior number 8")),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("Behavior number 2 ")),
        "scrolled past"
    );
    press(&mut tab, &[KeyCode::Up; 8]);
    let lines = shown(&tab, 60, true);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with(" ❯ Behavior number 0")),
        "{lines:#?}"
    );
}

/// The tab on the map of [`opened`], with three makers' models in its universe, its keys on the
/// picture and motion held still unless a test moves it.
fn in_the_world() -> BehaviorsTab {
    let mut tab = BehaviorsTab::default();
    tab.accept(&sample().world(super::sample::three_makers()).map);
    // Motion off, so a level is reached at once.
    tab.moment.still = true;
    tab.moment.skips_frames = true;
    tab.picture_placed = true;
    tab
}

#[test]
fn the_digits_and_minus_and_plus_go_between_the_map_the_planet_and_the_universe() {
    let mut tab = in_the_world();
    assert_eq!(tab.level(), Level::Map);
    press(&mut tab, &[KeyCode::Char('2')]);
    assert_eq!(tab.level(), Level::Planet);
    press(&mut tab, &[KeyCode::Char('3')]);
    assert_eq!(tab.level(), Level::Universe);
    press(&mut tab, &[KeyCode::Char('1')]);
    assert_eq!(tab.level(), Level::Map);
    press(&mut tab, &[KeyCode::Char('-')]);
    assert_eq!(tab.level(), Level::Planet);
    press(&mut tab, &[KeyCode::Char('-'), KeyCode::Char('-')]);
    assert_eq!(
        tab.level(),
        Level::Universe,
        "the universe is the furthest out"
    );
    press(&mut tab, &[KeyCode::Char('+')]);
    assert_eq!(
        tab.level(),
        Level::Planet,
        "nothing picked: in to the planet"
    );
    press(&mut tab, &[KeyCode::Char('=')]);
    assert_eq!(tab.level(), Level::Map, "= is + without Shift");
    press(&mut tab, &[KeyCode::Char('+')]);
    assert_eq!(tab.level(), Level::Map, "the map is the furthest in");
}

#[test]
fn in_the_universe_the_arrows_pick_each_galaxy_then_your_planet_and_enter_goes_in() {
    let mut tab = in_the_world();
    press(&mut tab, &[KeyCode::Char('3'), KeyCode::Right]);
    assert_eq!(
        tab.picked(),
        Some(Pointed::Galaxy(0)),
        "the busiest maker first"
    );
    assert_eq!(tab.universe().makers[0].key, "anthropic");
    press(&mut tab, &[KeyCode::Right, KeyCode::Right, KeyCode::Right]);
    assert_eq!(
        tab.picked(),
        Some(Pointed::Home),
        "your planet is a pick too"
    );
    press(&mut tab, &[KeyCode::Right]);
    assert_eq!(tab.picked(), Some(Pointed::Galaxy(0)), "round again");
    press(&mut tab, &[KeyCode::Left]);
    assert_eq!(tab.picked(), Some(Pointed::Home));
    press(&mut tab, &[KeyCode::Enter]);
    assert_eq!(
        tab.level(),
        Level::Planet,
        "enter on your planet opens the planet"
    );

    press(
        &mut tab,
        &[KeyCode::Char('3'), KeyCode::Right, KeyCode::Enter],
    );
    assert_eq!(tab.level(), Level::Galaxy(0));
    assert_eq!(tab.picked(), None);
    press(&mut tab, &[KeyCode::Right, KeyCode::Right]);
    assert_eq!(
        tab.picked(),
        Some(Pointed::Star(1)),
        "Anthropic's second model"
    );
    press(&mut tab, &[KeyCode::Left, KeyCode::Char('+')]);
    assert_eq!(tab.level(), Level::System(0), "+ goes into the star picked");
    let bodies: Vec<_> = tab.universe().picks(tab.level());
    assert_eq!(bodies.first(), Some(&Pointed::Star(0)), "its star first");
    let belt = tab
        .universe()
        .bodies
        .iter()
        .position(|body| *body == super::universe::Body::Belt { star: 0 })
        .expect("Haiku has more behaviors than planets");
    assert_eq!(
        bodies.last(),
        Some(&Pointed::Body(belt as u32)),
        "its belt last"
    );
    press(&mut tab, &[KeyCode::End]);
    assert_eq!(
        tab.picked(),
        Some(Pointed::Body(belt as u32)),
        "End picks the belt"
    );
}

#[test]
fn in_a_system_enter_keeps_the_body_picked_and_esc_comes_back_out_a_level_at_a_time() {
    let mut tab = in_the_world();
    press(
        &mut tab,
        &[KeyCode::Char('3'), KeyCode::Right, KeyCode::Enter],
    );
    press(&mut tab, &[KeyCode::Right, KeyCode::Enter]);
    assert_eq!(tab.level(), Level::System(0));
    press(&mut tab, &[KeyCode::Right, KeyCode::Right, KeyCode::Enter]);
    assert_eq!(
        tab.level(),
        Level::System(0),
        "the system is the deepest level"
    );
    assert_eq!(
        tab.picked(),
        Some(Pointed::Body(0)),
        "its first planet stays picked"
    );
    assert!(tab.claims(KeyCode::Esc, true));
    press(&mut tab, &[KeyCode::Esc]);
    assert_eq!(tab.level(), Level::Galaxy(0));
    assert_eq!(
        tab.picked(),
        Some(Pointed::Star(0)),
        "the star it came from"
    );
    press(&mut tab, &[KeyCode::Esc]);
    assert_eq!(tab.level(), Level::Universe);
    assert_eq!(
        tab.picked(),
        Some(Pointed::Galaxy(0)),
        "the galaxy it came from"
    );
    press(&mut tab, &[KeyCode::Esc]);
    assert_eq!(tab.level(), Level::Map, "then back to the map");
    assert!(
        !tab.claims(KeyCode::Esc, true),
        "and then Esc closes the panel"
    );

    press(
        &mut tab,
        &[
            KeyCode::Char('3'),
            KeyCode::Right,
            KeyCode::Enter,
            KeyCode::Char('3'),
        ],
    );
    assert_eq!(
        tab.level(),
        Level::Universe,
        "3 goes from a galaxy to the universe"
    );
}

/// A model drops its maker's word only after its maker's own logo: the line under a galaxy
/// whose logos draw as pictures says `6 Luna`, while a colour dot in the logo's place, braille,
/// which draws no logo, and a screen reader, which hears none, keep `GPT 6 Luna`.
#[test]
fn a_model_keeps_its_makers_word_where_no_logo_stands_before_it() {
    let mut tab = in_the_world();
    press(
        &mut tab,
        &[
            KeyCode::Char('3'),
            KeyCode::Right,
            KeyCode::Right,
            KeyCode::Enter,
            KeyCode::Right,
        ],
    );
    assert_eq!(tab.picked(), Some(Pointed::Star(2)), "OpenAI's one model");
    let star_line = |pictures: bool, logo_pictures: bool| {
        let rows = shown_drawn(&tab, 118, 30, pictures, logo_pictures);
        rows.iter()
            .find(|line| line.contains("6 Luna · "))
            .cloned()
            .unwrap_or_else(|| rows.join("\n"))
    };
    let with_logos = star_line(true, true);
    assert!(
        with_logos.contains(" 6 Luna · a protostar") && !with_logos.contains("GPT"),
        "{with_logos}"
    );
    let with_dots = star_line(true, false);
    assert!(
        with_dots.contains("●  GPT 6 Luna · a protostar"),
        "{with_dots}"
    );
    let braille = star_line(false, false);
    assert!(braille.contains("GPT 6 Luna · a protostar"), "{braille}");
    let heard = tab.spoken_here(&Default::default());
    assert!(
        heard
            .iter()
            .any(|line| line.starts_with("GPT 6 Luna · a protostar")),
        "{heard:#?}"
    );
}

#[test]
fn enter_with_nothing_picked_says_what_to_pick_until_the_next_key() {
    let mut tab = in_the_world();
    press(&mut tab, &[KeyCode::Char('3'), KeyCode::Enter]);
    assert_eq!(tab.level(), Level::Universe);
    assert_eq!(
        tab.notice(),
        Some("Pick a galaxy with ←  →  first, or point at one.")
    );
    press(&mut tab, &[KeyCode::Right]);
    assert_eq!(tab.notice(), None);
}

#[test]
fn tab_steps_the_groupings_on_every_level_and_in_the_universe_says_why_the_sky_stays() {
    let mut tab = in_the_world();
    press(&mut tab, &[KeyCode::Char('2')]);
    let asked = |key| match key {
        Some(MapKey::Ask(ViewRequest::Behaviors(request))) => request,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        asked(press(&mut tab, &[KeyCode::Tab])).grouping,
        Grouping::Cli
    );
    assert_eq!(tab.level(), Level::Planet, "a new grouping keeps the level");
    assert_eq!(tab.notice(), None);
    press(&mut tab, &[KeyCode::Char('3')]);
    assert_eq!(
        asked(press(&mut tab, &[KeyCode::Tab])).grouping,
        Grouping::Role
    );
    assert_eq!(
        tab.notice(),
        Some("The universe is drawn by maker and model; by role colours the map and the planet.")
    );
}

#[test]
fn the_groupings_row_takes_the_arrows_and_down_goes_into_the_picture() {
    let mut tab = in_the_world();
    tab.arrive_from_above();
    assert!(tab.on_first(), "↑ from the groupings goes to the sub-tabs");
    assert!(!tab.claims(KeyCode::Up, true));
    assert!(
        !tab.claims(KeyCode::Esc, true),
        "Esc closes from the groupings"
    );
    let request = match press(&mut tab, &[KeyCode::Right]) {
        Some(MapKey::Ask(ViewRequest::Behaviors(request))) => request,
        other => panic!("{other:?}"),
    };
    assert_eq!(request.grouping, Grouping::Cli);
    assert_eq!(tab.compared(), None, "the arrows compare nothing there");
    assert_eq!(
        tab.keys(118).join(" · "),
        "r to cycle dates · tab · ←  →  groupings · s this project · d summaries · ↓  into the \
         map · esc closes"
    );
    press(&mut tab, &[KeyCode::Down]);
    assert_eq!(tab.stop(), Stop::Picture);
    press(&mut tab, &[KeyCode::Right]);
    assert_eq!(tab.compared(), Some(0), "on the picture → compares");
    press(&mut tab, &[KeyCode::Home]);
    assert_eq!(tab.stop(), Stop::Picture, "Home stays on the picture");
}

#[test]
fn each_level_names_its_own_keys() {
    let mut tab = in_the_world();
    let keys = |tab: &BehaviorsTab| tab.keys(118).join(" · ");
    assert_eq!(
        keys(&tab),
        "r to cycle dates · s this project · d summaries · tab groupings · ←  →  compare · t top \
         view · esc back"
    );
    press(&mut tab, &[KeyCode::Char('t')]);
    assert!(tab.flat());
    assert!(keys(&tab).contains("t stand up"), "{}", keys(&tab));
    press(&mut tab, &[KeyCode::Char('2')]);
    assert_eq!(
        keys(&tab),
        "r to cycle dates · tab groupings · ←  →  compare · drag to turn · n night side · esc back"
    );
    press(&mut tab, &[KeyCode::Char('n')]);
    assert!(tab.night() && keys(&tab).contains("n day side"));
    press(&mut tab, &[KeyCode::Char('3')]);
    assert_eq!(
        keys(&tab),
        "r to cycle dates · ←  →  pick a galaxy · enter go in · scroll in or out · esc back to the map"
    );
    press(&mut tab, &[KeyCode::Right, KeyCode::Enter]);
    assert_eq!(
        keys(&tab),
        "r to cycle dates · ←  →  pick a star · enter its system · scroll in or out · esc out to \
         the universe"
    );
    press(&mut tab, &[KeyCode::Right, KeyCode::Enter]);
    assert_eq!(
        keys(&tab),
        "r to cycle dates · ←  →  pick a body · scroll out · esc out to the galaxy"
    );
    tab.moment.vim = true;
    assert!(keys(&tab).contains("h  l  pick a body"), "{}", keys(&tab));
    press(&mut tab, &[KeyCode::Char('m')]);
    assert!(tab.nature_hidden());
}

#[test]
fn going_to_another_level_zooms_there_unless_motion_is_off() {
    use std::time::Duration;

    let mut tab = in_the_world();
    tab.moment.still = false;
    tab.moment.skips_frames = false;
    let start = tab.moment.now;
    press(&mut tab, &[KeyCode::Char('2')]);
    let zoom = tab.zoom(start).expect("a zoom");
    assert_eq!((zoom.from, zoom.to), (Level::Map, Level::Planet));
    assert_eq!(tab.zoom(start + Duration::from_secs(2)), None, "arrived");
    // Reduced motion still zooms, what moves in it held still.
    tab.moment.still = true;
    press(&mut tab, &[KeyCode::Char('3')]);
    let zoom = tab.zoom(start).expect("reduced motion zooms");
    assert_eq!((zoom.from, zoom.to), (Level::Planet, Level::Universe));
    tab.moment.skips_frames = true;
    press(&mut tab, &[KeyCode::Char('1')]);
    assert_eq!(tab.zoom(start), None, "motion off jumps");
    assert_eq!(tab.level(), Level::Map);
}

#[test]
fn leaving_the_dots_closes_the_list_and_a_new_map_keeps_the_level() {
    let mut tab = in_the_world();
    press(&mut tab, &[KeyCode::Right, KeyCode::Down]);
    assert_eq!(tab.at, At::List(0));
    press(&mut tab, &[KeyCode::Char('2')]);
    assert_eq!(
        tab.at,
        At::List(0),
        "the planet shows the dots, and the list"
    );
    press(&mut tab, &[KeyCode::Char('3')]);
    assert_eq!(tab.at, At::Map);
    press(&mut tab, &[KeyCode::Right, KeyCode::Enter, KeyCode::Right]);
    assert_eq!(tab.level(), Level::Galaxy(0));
    tab.accept(&sample().world(super::sample::three_makers()).map);
    assert_eq!(tab.level(), Level::Galaxy(0));
    assert_eq!(tab.picked(), Some(Pointed::Star(0)));
    // A map with no facts has no galaxy to stay in.
    tab.accept(&sample().map);
    assert_eq!(tab.level(), Level::Universe);
    assert_eq!(tab.picked(), None);
}

#[test]
fn the_wheel_goes_in_toward_what_the_pointer_rests_on_and_out() {
    let mut tab = in_the_world();
    assert!(tab.wheel(false));
    assert_eq!(tab.level(), Level::Planet);
    assert!(tab.wheel(false));
    assert_eq!(tab.level(), Level::Universe);
    assert!(!tab.wheel(false), "nothing further out");
    tab.pointed = Some(Pointed::Galaxy(1));
    assert!(tab.wheel(true));
    assert_eq!(
        tab.level(),
        Level::Galaxy(1),
        "into the galaxy under the pointer"
    );
    assert!(tab.wheel(false));
    assert_eq!(tab.level(), Level::Universe);
    assert!(tab.wheel(true), "the galaxy it came out of stays picked");
    assert_eq!(tab.level(), Level::Galaxy(1));
    tab.wheel(false);
    tab.picked = None;
    assert!(tab.wheel(true), "from open sky, in to the planet");
    assert_eq!(tab.level(), Level::Planet);
}

/// Beside the world's map every group, in a list that scrolls in the rows the map leaves and
/// keeps the compared one in view, with how many it passed above and how many are below; never
/// "and N more". The counts line up three blanks after the widest name.
#[test]
fn beside_the_world_every_group_in_a_list_that_keeps_the_compared_one_in_view() {
    let mut tab = ten_models();
    let rows = |tab: &BehaviorsTab| -> Vec<String> {
        shown(tab, 40, true)
            .iter()
            .map(|line| {
                line.chars()
                    .skip(67)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .filter(|line| {
                line.starts_with('●')
                    || line.starts_with('○')
                    || line.ends_with(" more above")
                    || line.ends_with(" more below")
            })
            .collect()
    };
    let open = rows(&tab);
    assert_eq!(
        open.len(),
        10,
        "every one fits with nothing compared: {open:#?}"
    );
    assert_eq!(open[0], "●  ◧◨ Model 0   10");
    assert_eq!(open[9], "●  ◧◨ Model 9    1");
    for group in 0..10 {
        press(&mut tab, &[KeyCode::Right]);
        assert_eq!(tab.compared(), Some(group));
        let shown = rows(&tab);
        assert!(
            shown
                .iter()
                .any(|row| row.starts_with(&format!("●  ◧◨ Model {group} "))),
            "{group}: {shown:#?}"
        );
        assert!(!shown.iter().any(|row| row.contains("And ")), "{shown:#?}");
    }
    let last = rows(&tab);
    assert!(
        last[0].starts_with("↑  ") && last[0].ends_with(" more above"),
        "{last:#?}"
    );
    assert_eq!(last.last().map(String::as_str), Some("●  ◧◨ Model 9    1"));
}

/// `behaviors.view = "braille"` draws the braille map even where the screen places pictures,
/// and the world is drawn only where pictures are placed.
#[test]
fn the_view_setting_can_ask_for_braille_where_the_screen_places_pictures() {
    use argonauts_core::settings::behaviors::BehaviorView;

    let mut tab = opened();
    assert!(!tab.draws_world(), "no pictures placed");
    tab.picture_placed = true;
    assert!(tab.draws_world());
    let mut map = sample().map;
    map.view = BehaviorView::Braille;
    tab.accept(&map);
    assert_eq!(tab.view(), BehaviorView::Braille);
    assert!(!tab.draws_world());
    let lines = shown(&tab, 40, true);
    assert!(
        lines[..13]
            .iter()
            .any(|line| line.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c))),
        "{lines:#?}"
    );
    assert!(
        lines[0].ends_with("Actions"),
        "the braille legend: {lines:#?}"
    );
    assert!(!lines.iter().any(|line| line.contains("1 Map · 2 Planet")));
    map.view = BehaviorView::World;
    tab.accept(&map);
    assert!(tab.draws_world());
}

/// For a screen reader the world is plain lines: the map and its groups as before, then the
/// land by its climates, the planet, and each maker with its models; where the keys are, the
/// level shown and what ← → picked, in the pointer line's words, so stepping body by body says
/// each one.
#[test]
fn a_screen_reader_hears_the_world_and_what_the_arrows_step_to() {
    use argonauts_core::behaviors::behavior_world::{BehaviorFacts, CrewFacts};

    let mut world = super::sample::three_makers();
    world.behaviors = vec![
        BehaviorFacts {
            behavior: "read a source file".to_owned(),
            count: 17,
            last_used: Some(0),
            days_in_last_7: 5,
            ..BehaviorFacts::default()
        },
        BehaviorFacts {
            behavior: "read git history".to_owned(),
            count: 5,
            last_used: Some(16),
            ..BehaviorFacts::default()
        },
    ];
    world.crew = CrewFacts {
        idle_days: 4,
        quiet_days: 3,
        ..CrewFacts::default()
    };
    let mut tab = BehaviorsTab::default();
    tab.accept(&sample().world(world).map);
    tab.moment.still = true;
    let names = Default::default();
    let lines = tab.spoken_here(&names);
    assert!(
        lines.contains(
            &"Map: 28 actions on 2 behaviors; 45 behaviors on the list never used.".to_owned()
        ),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with(
            "Planet: a gas giant, too young for land; 99% of the map is sea; 4 idle days; 3 \
             quiet days;"
        )),
        "{lines:#?}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("  "))
            .filter(|line| line.contains(" in the last 7 days."))
            .count(),
        3,
        "a line for each maker: {lines:#?}"
    );
    assert_eq!(lines.last().map(String::as_str), Some("Shown: Map"));
    press(&mut tab, &[KeyCode::Char('3'), KeyCode::Right]);
    let lines = tab.spoken_here(&names);
    let at = lines
        .iter()
        .position(|line| line == "Shown: Universe")
        .expect("the level");
    assert!(
        lines[at + 1].starts_with("Anthropic · 32 actions, 76% of yours · 2 models"),
        "{lines:#?}"
    );
    press(&mut tab, &[KeyCode::Right]);
    let lines = tab.spoken_here(&names);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("OpenAI · 7 actions")),
        "the next galaxy says its own line: {lines:#?}"
    );
}

/// The pointer finds what each level drew where it was drawn: on the universe's picture a
/// galaxy where it stands, whose line then shows under the picture, and a click goes into it;
/// in braille the same galaxy among the cells.
#[test]
fn the_pointer_finds_what_each_level_drew_and_a_click_goes_into_it() {
    let mut tab = in_the_world();
    press(&mut tab, &[KeyCode::Char('3')]);
    let theme = Theme::dark();
    let kit = Kit::new(&theme);
    let colours = GroupColours::new(&kit, tab.map().unwrap(), &[], &Default::default());
    let scene = tab
        .scene(
            &colours,
            &theme,
            &Default::default(),
            (18, 42),
            tab.moment.now,
        )
        .expect("the universe drawn");
    let (x, y) = scene
        .hits
        .marks()
        .iter()
        .find_map(|mark| match (mark.pointed, &mark.shape) {
            (Pointed::Galaxy(1), crate::world::Shape::Disc { x, y, .. }) => Some((*x, *y)),
            _ => None,
        })
        .expect("OpenAI's galaxy");
    let on_galaxy = MapPoint {
        x,
        y,
        width: 1152.0,
        height: 546.0,
    };
    assert!(tab.point(Some(on_galaxy)));
    assert_eq!(tab.pointed(), Some(Pointed::Galaxy(1)));
    let lines = shown(&tab, 40, true);
    assert!(
        lines.iter().any(|line| line.contains("OpenAI · 7 actions")),
        "{lines:#?}"
    );
    assert!(tab.point(Some(MapPoint {
        x: 3.0,
        y: 3.0,
        width: 1152.0,
        height: 546.0
    })));
    assert!(
        matches!(tab.pointed(), Some(Pointed::Sky(_))),
        "the sky: {:?}",
        tab.pointed()
    );
    assert!(tab.point(Some(on_galaxy)));
    assert!(tab.click());
    assert_eq!(tab.level(), Level::Galaxy(1));

    let places = super::places::places(tab.universe(), Level::Universe);
    let galaxy = places
        .iter()
        .find(|p| p.pointed == Pointed::Galaxy(1))
        .expect("OpenAI's galaxy in braille");
    let at = |width: f32, height: f32| MapPoint {
        x: galaxy.at.0 as f32 * width,
        y: galaxy.at.1 as f32 * height,
        width,
        height,
    };
    // Without pictures the screen reports cells, and the braille draws the same places.
    let mut braille = in_the_world();
    braille.picture_placed = false;
    press(&mut braille, &[KeyCode::Char('3')]);
    assert!(braille.point(Some(at(64.0, 13.0))));
    assert_eq!(braille.pointed(), Some(Pointed::Galaxy(1)));
    assert!(braille.point(None));
    assert_eq!(braille.pointed(), None);
}

/// On the planet the pointer finds a dot on its near side, and the scene the screen places
/// carries what the pointer finds with its picture, moving only while a zoom has frames.
#[test]
fn the_planet_finds_its_near_dots_and_the_scene_carries_its_hit_map() {
    use std::time::Duration;

    let mut tab = in_the_world();
    press(&mut tab, &[KeyCode::Char('2')]);
    let theme = Theme::dark();
    let kit = Kit::new(&theme);
    let colours = GroupColours::new(&kit, tab.map().unwrap(), &[], &Default::default());
    let now = tab.moment.now;
    let scene = tab
        .scene(&colours, &theme, &Default::default(), (18, 42), now)
        .expect("a scene");
    let (at, x, y) = scene
        .hits
        .marks()
        .iter()
        .find_map(|mark| match (mark.pointed, &mark.shape) {
            (Pointed::Action(at), crate::world::Shape::Point { x, y }) => {
                Some((at as usize, *x, *y))
            }
            _ => None,
        })
        .expect("a dot on the near side");
    let found = tab.point(Some(MapPoint {
        x,
        y,
        width: 1152.0,
        height: 546.0,
    }));
    assert!(found);
    assert_eq!(tab.hovered(), Some(at));

    assert_eq!((scene.width(), scene.height()), (1152, 546));
    assert_eq!(scene.layers().len(), 1);
    assert_eq!(scene.hits.find(x, y), Some(Pointed::Action(at as u32)));
    assert!(!scene.moves, "motion held still");
    tab.moment.still = false;
    tab.moment.skips_frames = false;
    press(&mut tab, &[KeyCode::Char('3')]);
    assert!(
        tab.scene(&colours, &theme, &Default::default(), (18, 42), now)
            .unwrap()
            .moves
    );
    // Once the zoom is done the universe moves only with its own motion, held still here.
    tab.moment.still = true;
    let later = now + Duration::from_secs(3);
    assert!(
        !tab.scene(&colours, &theme, &Default::default(), (18, 42), later)
            .unwrap()
            .moves
    );
}

/// A drag turns the planet a step at a time, round again past the last, and only the planet.
#[test]
fn the_planet_turns_a_step_at_a_time_and_only_on_the_planet() {
    let mut tab = in_the_world();
    assert!(!tab.turn_planet(1), "the map doesn't turn");
    press(&mut tab, &[KeyCode::Char('2')]);
    let facing = tab.planet_view();
    assert!(tab.turn_planet(1));
    assert_eq!(tab.planet_view().step(), facing.step() + 1);
    assert!(tab.turn_planet(-2));
    assert_eq!(tab.planet_view().step(), crate::world::View::STEPS - 1);
}

/// Over the world's picture as the screen reports it: `x` and `y` in its pixels, 1152 × 546.
fn over_picture(x: f32, y: f32) -> MapPoint {
    MapPoint {
        x,
        y,
        width: 1152.0,
        height: 546.0,
    }
}

/// A drag across the planet turns it a radian for each of its radii the pointer goes, to the
/// nearest of the terminal's steps, keeping the part of a step left over for the drag's next
/// move. On the map a drag turns nothing.
#[test]
fn a_drag_turns_the_planet_as_far_as_the_pointer_went() {
    use crate::world::View;

    let mut tab = in_the_world();
    tab.point(Some(over_picture(500.0, 273.0)));
    assert!(
        !tab.drag(over_picture(600.0, 273.0)),
        "the map doesn't turn"
    );
    press(&mut tab, &[KeyCode::Char('2')]);
    let facing = tab.planet_view().step();
    tab.point(Some(over_picture(500.0, 273.0)));
    // 100 px right on a planet 240 px round turns it 100/240 of a radian west: 2.4 steps.
    assert!(tab.drag(over_picture(600.0, 273.0)));
    assert_eq!(
        tab.planet_view().step(),
        (facing + View::STEPS - 2) % View::STEPS
    );
    // Another 100 px: 4.8 steps in all, so the fifth.
    assert!(tab.drag(over_picture(700.0, 273.0)));
    assert_eq!(
        tab.planet_view().step(),
        (facing + View::STEPS - 5) % View::STEPS
    );
    assert!(
        !tab.drag(over_picture(705.0, 273.0)),
        "less than half a step"
    );
    assert!(tab.drag(over_picture(500.0, 273.0)), "and back");
    assert_eq!(tab.planet_view().step(), facing);
}

/// The wheel zooms a level a turn, but the turns that follow within a moment are the same
/// swipe, so one swipe of a trackpad goes one level. It zooms toward what the pointer is on.
#[test]
fn one_swipe_of_the_wheel_goes_one_level() {
    use std::time::Duration;

    let mut tab = in_the_world();
    let start = Instant::now();
    let at = |ms: u64| start + Duration::from_millis(ms);
    let middle = over_picture(576.0, 273.0);
    assert!(tab.scroll(middle, false, at(0)));
    assert_eq!(tab.level(), Level::Planet);
    for ms in [40, 120, 300] {
        tab.scroll(middle, false, at(ms));
        assert_eq!(tab.level(), Level::Planet, "the same swipe, {ms} ms on");
    }
    assert!(tab.scroll(middle, false, at(300 + 400)));
    assert_eq!(tab.level(), Level::Universe);
    assert!(
        tab.zoom(at(700)).is_none(),
        "held still, the zoom is done at once"
    );
    assert!(tab.scroll(middle, true, at(1200)));
    assert_eq!(
        tab.level(),
        Level::Planet,
        "from open sky, in to the planet"
    );
}

/// A zoom the wheel starts is timed from the wheel's turn, not from the last frame drawn.
#[test]
fn a_zoom_the_wheel_starts_runs_from_the_turn() {
    use std::time::Duration;

    let mut tab = in_the_world();
    tab.moment.still = false;
    tab.moment.skips_frames = false;
    let long_after = tab.moment.now + Duration::from_secs(60);
    assert!(tab.scroll(over_picture(576.0, 273.0), false, long_after));
    assert!(tab.zoom(long_after).is_some(), "it zooms from the turn on");
}

/// ? opens the list of every key and the next key closes it: Esc and ? only close it, and any
/// other key does what it does as well. `m` is listed there and nowhere else.
#[test]
fn question_mark_lists_every_key_until_the_next() {
    let mut tab = in_the_world();
    assert!(tab.claims(KeyCode::Char('?'), true));
    assert!(
        !tab.claims(KeyCode::Char('?'), false),
        "not from the sub-tabs"
    );
    assert!(!tab.claims(KeyCode::Esc, true), "nothing to close yet");
    assert_eq!(press(&mut tab, &[KeyCode::Char('?')]), Some(MapKey::Moved));
    assert!(tab.shortcuts_open());
    assert!(tab.claims(KeyCode::Esc, true));
    let hints = tab.shortcuts();
    assert!(hints.contains(&"m to hide nature".to_owned()), "{hints:#?}");
    assert!(
        hints.contains(&"t to lay the map flat".to_owned()),
        "{hints:#?}"
    );
    assert!(
        !tab.keys(200).iter().any(|key| key.starts_with("m ")),
        "the key line leaves m to ?"
    );
    press(&mut tab, &[KeyCode::Esc]);
    assert!(!tab.shortcuts_open());
    assert_eq!(tab.level(), Level::Map, "Esc only closed the list");

    press(&mut tab, &[KeyCode::Char('?'), KeyCode::Char('?')]);
    assert!(!tab.shortcuts_open(), "? again closes it");
    press(&mut tab, &[KeyCode::Char('?'), KeyCode::Char('m')]);
    assert!(!tab.shortcuts_open());
    assert!(tab.nature_hidden(), "m closed the list and hid nature");
    press(&mut tab, &[KeyCode::Char('?')]);
    assert!(tab.shortcuts().contains(&"m to show nature".to_owned()));
    press(&mut tab, &[KeyCode::Char('2')]);
    assert_eq!(tab.level(), Level::Planet, "2 closed the list and went out");
    assert!(!tab.shortcuts_open());
    press(&mut tab, &[KeyCode::Char('?')]);
    tab.arrive_from_above();
    assert!(
        !tab.shortcuts_open(),
        "coming back from the tabs, the list is closed"
    );

    tab.picture_placed = false;
    let braille = tab.shortcuts();
    assert!(braille.contains(&"drag to turn".to_owned()));
    assert!(
        !braille
            .iter()
            .any(|hint| ["m ", "n "].iter().any(|key| hint.starts_with(key))),
        "braille has no nature or night side to toggle: {braille:#?}"
    );
}

/// Each level lists its own keys: the planet its drag and night side, the cosmos what ← →
/// pick and what Enter goes into; with vim keys, h and l.
#[test]
fn each_level_lists_its_own_keys() {
    let mut tab = in_the_world();
    press(&mut tab, &[KeyCode::Char('2')]);
    let planet = tab.shortcuts();
    for hint in ["drag to turn", "n for the night side", "m to hide nature"] {
        assert!(planet.contains(&hint.to_owned()), "{hint}: {planet:#?}");
    }
    assert!(!planet.iter().any(|hint| hint.starts_with("t ")));
    press(&mut tab, &[KeyCode::Char('3')]);
    let universe = tab.shortcuts();
    assert!(universe.contains(&"←  →  to pick a galaxy".to_owned()));
    assert!(universe.contains(&"enter to go in".to_owned()));
    assert!(!universe.iter().any(|hint| hint.starts_with("m ")));
    tab.moment.vim = true;
    assert!(
        tab.shortcuts()
            .contains(&"h  l  to pick a galaxy".to_owned())
    );
    for hint in tab.shortcuts() {
        assert!(
            hint.chars().next().is_some_and(|c| !c.is_uppercase()),
            "a key hint stays lowercase: {hint}"
        );
    }
}

/// Every line the world writes starts with a capital: legends, hover lines, the summary and
/// notices, on every level, drawn as pictures or in braille. A line may start with a count, a
/// mark or the braille map itself before its words.
#[test]
fn every_line_the_world_writes_starts_with_a_capital() {
    let first_word = |line: &str| {
        line.split_whitespace()
            .find(|token| token.chars().any(char::is_alphanumeric))
            .map(str::to_owned)
    };
    let check = |tab: &BehaviorsTab, pictures: bool, case: &str| {
        for line in shown(tab, 40, pictures) {
            let Some(word) = first_word(&line) else {
                continue;
            };
            let first = word.chars().find(|c| c.is_alphanumeric()).unwrap();
            assert!(
                first.is_ascii_digit() || first.is_uppercase(),
                "{case}: {line:?}"
            );
        }
    };
    for pictures in [true, false] {
        let mut tab = in_the_world();
        let steps: [(&[KeyCode], &str); 8] = [
            (&[], "the map"),
            (&[KeyCode::Right, KeyCode::Right], "comparing"),
            (&[KeyCode::Down], "the list"),
            (&[KeyCode::Esc, KeyCode::Char('2')], "the planet"),
            (&[KeyCode::Char('3'), KeyCode::Right], "the universe"),
            (&[KeyCode::Tab], "a notice"),
            (
                &[KeyCode::Right, KeyCode::Enter, KeyCode::Right],
                "a galaxy",
            ),
            (
                &[KeyCode::Enter, KeyCode::Right, KeyCode::Right],
                "a system",
            ),
        ];
        for (keys, case) in steps {
            press(&mut tab, keys);
            if case == "a notice" {
                // A new grouping asks for a new map; the same one comes back.
                tab.accept(&{
                    let mut map = sample().world(super::sample::three_makers()).map;
                    map.request = tab.request;
                    map
                });
            }
            check(&tab, pictures, case);
        }
    }
}

/// The planet's turn holds from the moment the pointer rests on its picture to the moment it
/// leaves, however many moves come between.
#[test]
fn the_planets_turn_holds_while_the_pointer_rests_on_it() {
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let after = |seconds: u64| start + Duration::from_secs(seconds);
    let mut hold = super::Hold::default();
    hold.rest(true, start);
    hold.rest(true, after(1));
    assert_eq!(hold.held(after(2)), Duration::from_secs(2));
    hold.rest(false, after(3));
    hold.rest(false, after(5));
    assert_eq!(hold.held(after(9)), Duration::from_secs(3));
}
