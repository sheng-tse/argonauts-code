use super::*;
use crate::overview::sample;
use crate::stability;
use crate::text::{plain_text, spans_width};
use crate::theme::{ColorLevel, Glyphs, Theme};

fn plain() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn rows(drawn: &Drawn) -> Vec<String> {
    plain_text(&drawn.lines)
        .lines()
        .map(|row| row.trim_end().to_owned())
        .collect()
}

#[test]
fn the_picked_session_shows_its_context_of_its_window_and_where_it_rotates() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let (view, names) = (sample::context(), sample::names());
    let cards = ContextCards {
        view: &view,
        names: &names,
        selected: 0,
        mark: "❯",
        breakdown: None,
        all: false,
    };
    let rows = rows(&cards.session(&kit, Frame::view(110)).unwrap());
    assert!(
        rows[1].starts_with(" │ ◆ context · main · chat · Opus 5.5 on Claude Code"),
        "{rows:#?}"
    );
    assert!(
        rows[1].ends_with("212k of 1M · rotates at 950k  [21%] │"),
        "{rows:#?}"
    );
    // The bar is two rows high on this tab, with the tick where the session rotates.
    assert_eq!(rows[2], rows[3], "{rows:#?}");
    assert!(rows[2].contains('│'), "{rows:#?}");
    assert!(rows[4].contains("▌ in context"), "{rows:#?}");
    assert!(rows[4].contains("▌ free"), "{rows:#?}");
    assert_eq!(rows.len(), 6, "{rows:#?}");

    let mut rotated = sample::context();
    rotated.rows[0].rotations = 2;
    let cards = ContextCards {
        view: &rotated,
        ..cards
    };
    let rows = self::rows(&cards.session(&kit, Frame::view(110)).unwrap());
    assert!(rows[5].contains("started afresh twice so far"), "{rows:#?}");

    let none = ContextCards {
        view: &ContextData { rows: Vec::new() },
        ..cards
    };
    assert!(none.session(&kit, Frame::view(110)).is_none());
}

#[test]
fn every_live_session_has_a_row_with_a_meter_and_the_picked_one_is_marked() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let (view, names) = (sample::context(), sample::names());
    let cards = ContextCards {
        view: &view,
        names: &names,
        selected: 1,
        mark: "❯",
        breakdown: None,
        all: false,
    };
    let rows = rows(&cards.sessions(&kit, Frame::view(120)));
    assert!(rows[1].ends_with("3 live · 291k in context │"), "{rows:#?}");
    for heading in ["Session", "Worker", "Context", "Used", "Calls", "Spend"] {
        assert!(rows[2].contains(heading), "{heading}: {rows:#?}");
    }
    assert!(rows[3].starts_with(" │   main · chat"), "{rows:#?}");
    assert!(
        rows[4].starts_with(" │ ❯ build:report-endpoint"),
        "{rows:#?}"
    );
    for part in [
        "Opus 5.5 on Claude Code",
        "━",
        "48.2k/200k",
        " 24%",
        "37",
        "~$0.62",
    ] {
        assert!(rows[4].contains(part), "{part}: {rows:#?}");
    }
    assert!(rows[5].contains("31k/272k"), "{rows:#?}");
    // Each meter carries the tick where its session starts afresh.
    assert!(rows[3].contains('┃'), "{rows:#?}");

    let none = ContextCards {
        view: &ContextData { rows: Vec::new() },
        ..cards
    };
    let empty = self::rows(&none.sessions(&kit, Frame::view(120)));
    assert!(empty[2].contains("No live sessions"), "{empty:#?}");
}

#[test]
fn both_cards_keep_inside_their_width_and_a_narrow_list_drops_the_model_then_the_meter() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let (view, names) = (sample::context(), sample::names());
    let cards = ContextCards {
        view: &view,
        names: &names,
        selected: 0,
        mark: "❯",
        breakdown: None,
        all: false,
    };
    for width in stability::WIDTHS.map(usize::from) {
        let frame = Frame::view(width);
        for drawn in [
            cards.session(&kit, frame).unwrap(),
            cards.sessions(&kit, frame),
        ] {
            for line in &drawn.lines {
                assert!(spans_width(&line.spans) <= width, "at {width}: {line}");
            }
        }
    }
    let narrow = rows(&cards.sessions(&kit, Frame::view(80)));
    assert!(!narrow[2].contains("Worker"), "{narrow:#?}");
    assert!(narrow[3].contains("212k/1M"), "{narrow:#?}");
    let tiny = rows(&cards.sessions(&kit, Frame::view(50)));
    assert!(!tiny[2].contains("Context"), "{tiny:#?}");
    assert!(tiny[3].contains("212k/1M"), "{tiny:#?}");
}

#[test]
fn a_long_list_shows_ten_sessions_and_keeps_the_picked_one_in_view() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let names = sample::names();
    let one = sample::context().rows.remove(1);
    let view = ContextData {
        rows: (0..14)
            .map(|n| ContextRow {
                label: format!("build:task-{n:02}"),
                ..one.clone()
            })
            .collect(),
    };
    let top = ContextCards {
        view: &view,
        names: &names,
        selected: 0,
        mark: "❯",
        breakdown: None,
        all: false,
    };
    let shown = rows(&top.sessions(&kit, Frame::view(120)));
    assert_eq!(shown.len(), 2 + 1 + 10 + 1 + 1, "{shown:#?}");
    assert!(shown[3].contains("❯ build:task-00"), "{shown:#?}");
    assert!(
        shown[13].contains("1 of 14 · ↑↓ shows the rest"),
        "{shown:#?}"
    );

    let last = ContextCards {
        selected: 13,
        ..top
    };
    let shown = rows(&last.sessions(&kit, Frame::view(120)));
    assert!(shown[3].contains("build:task-04"), "{shown:#?}");
    assert!(shown[12].contains("❯ build:task-13"), "{shown:#?}");
}

#[test]
fn the_conversations_own_session_shows_its_categories_and_what_fills_them() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let (view, names, breakdown) = (sample::context(), sample::names(), sample::breakdown());
    let cards = ContextCards {
        view: &view,
        names: &names,
        selected: 0,
        mark: "❯",
        breakdown: Some(&breakdown),
        all: false,
    };
    let shown = rows(&cards.session(&kit, Frame::view(110)).unwrap());
    assert!(
        shown[1].ends_with("212k of 1M · compacts at 950k  [21%] │"),
        "{shown:#?}"
    );
    let legend = shown[4..7].join("\n");
    for part in [
        "▌ system prompt  3.4k",
        "▌ tools           12k",
        "▌ mcp tools      2.6k",
        "▌ agents         1.1k",
        "▌ memory files   4.9k",
        "▌ skills         2.2k",
        "▌ messages       186k",
        "▌ free           788k",
    ] {
        assert!(legend.contains(part), "{part}: {shown:#?}");
    }
    // Under the legend, a column for each category that lists its items.
    let at = shown
        .iter()
        .position(|row| row.contains("Memory files · 2"))
        .unwrap_or_else(|| panic!("{shown:#?}"));
    for heading in ["MCP tools · 2", "Skills · 3", "Custom agents · 1"] {
        assert!(shown[at].contains(heading), "{heading}: {shown:#?}");
    }
    for part in [
        "├ CLAUDE.md",
        "2.1k",
        "├ github",
        "├ export-csv",
        "└ reviewer",
    ] {
        assert!(shown[at + 1].contains(part), "{part}: {shown:#?}");
    }
    assert!(shown[at + 2].contains("└ AGENTS.md"), "{shown:#?}");
    assert!(shown[at + 3].contains("└ release-notes"), "{shown:#?}");

    // A run's session has no breakdown of its own, so it shows used and free.
    let other = ContextCards {
        selected: 1,
        ..cards
    };
    let shown = rows(&other.session(&kit, Frame::view(110)).unwrap());
    assert!(shown.iter().any(|row| row.contains("▌ in context")));
    assert!(!shown.iter().any(|row| row.contains("Memory files")));

    // Narrow, the columns go under one another and every row keeps inside the card.
    for width in stability::WIDTHS.map(usize::from) {
        let drawn = cards.session(&kit, Frame::view(width)).unwrap();
        for line in &drawn.lines {
            assert!(spans_width(&line.spans) <= width, "at {width}: {line}");
        }
    }
}

#[test]
fn a_long_category_lists_its_first_items_and_counts_the_rest() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let mut breakdown = sample::breakdown();
    let skill = breakdown.skills[0].clone();
    breakdown.skills = (0..9)
        .map(|n| ContextItem {
            name: format!("skill-{n}"),
            ..skill.clone()
        })
        .collect();
    let shown = rows(&items(&kit, &breakdown, 100, false));
    assert_eq!(shown.len(), 1 + 5, "{shown:#?}");
    assert!(shown[0].contains("Skills · 9"), "{shown:#?}");
    assert!(shown[4].contains("├ skill-3"), "{shown:#?}");
    assert!(shown[5].contains("└ and 5 more"), "{shown:#?}");
}

#[test]
fn asked_for_all_every_item_shows_with_its_tokens_and_where_it_comes_from() {
    let theme = plain();
    let kit = Kit::new(&theme);
    let mut breakdown = sample::breakdown();
    let skill = breakdown.skills[0].clone();
    breakdown.skills = (0..9)
        .map(|n| ContextItem {
            name: format!("skill-{n}"),
            tokens: 100 * (n + 1),
            source: Some("project".to_owned()),
        })
        .collect();
    breakdown.mcp_tools[0].source = Some("github".to_owned());
    let shown = rows(&items(&kit, &breakdown, 100, true));
    let at = |what: &str| {
        shown
            .iter()
            .position(|row| row.contains(what))
            .unwrap_or_else(|| panic!("{what}: {shown:#?}"))
    };
    // A heading a category, with how many and what they come to, then every item.
    assert_eq!(shown[0], "Memory files · 2 · 4.9k");
    assert!(shown[1].starts_with("├ CLAUDE.md"), "{shown:#?}");
    assert!(shown[1].ends_with("2.1k"), "{shown:#?}");
    assert!(shown[2].starts_with("└ AGENTS.md"), "{shown:#?}");
    assert_eq!(shown[3], "", "a blank row parts the categories");
    assert!(
        shown[at("MCP tools · 2") + 1].ends_with("1.9k   github"),
        "{shown:#?}"
    );
    let skills = at("Skills · 9 · 4.5k");
    assert!(shown[skills + 9].starts_with("└ skill-8"), "{shown:#?}");
    assert!(shown[skills + 9].ends_with("900   project"), "{shown:#?}");
    assert!(!shown.iter().any(|row| row.contains("more")), "{shown:#?}");
    assert!(shown[at("Custom agents · 1") + 1].starts_with("└ reviewer"));
    assert_eq!(skill.name, "export-csv");

    for width in [20, 40, 60, 80, 200] {
        for row in rows(&items(&kit, &breakdown, width, true)) {
            assert!(crate::text::str_width(&row) <= width, "{width}: {row}");
        }
    }
}
