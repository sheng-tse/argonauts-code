mod common;

use argonauts_tui::commands::command_items;
use argonauts_tui::model::{BottomState, MenuItem};
use argonauts_tui::stability::{self, samples};
use common::{assert_fits, ctx, text, theme};

const WIDTHS: [u16; 4] = [80, 100, 120, 200];

fn typed(text: &str) -> BottomState {
    let mut bottom = BottomState::new();
    bottom.composer.label = Some("peer-crew".into());
    bottom.menus.commands = command_items(None);
    bottom.menus.mentions = vec![
        MenuItem::new("claude-code", "opus · owner on card-002").with_kind("agent"),
        MenuItem::new(
            "codex",
            "2 sessions: owner on card-003, reviewer on card-002",
        )
        .with_kind("agent"),
        MenuItem::new("card-002", "Add the JSON writer").with_kind("card"),
        MenuItem::new("card-003", samples::CJK).with_kind("card"),
        MenuItem::new("owner", "claude-code/opus on card-002, codex on card-003").with_kind("role"),
        MenuItem::new("reviewer", "codex on card-002").with_kind("role"),
        MenuItem::new("1", "claude-code/opus on card-002").with_kind("follow"),
        MenuItem::new("2", "codex on card-003").with_kind("follow"),
        MenuItem::new("all", "Every agent in this run").with_kind("run"),
    ];
    bottom.composer.set_text(text);
    bottom
}

fn snapshot_widths(name: &str, bottom: &BottomState) {
    let theme = theme();
    for width in stability::WIDTHS {
        let view = bottom.render(&ctx(&theme, width, 0.0));
        stability::assert_lines(&view.lines, width);
        assert_fits(&view.lines, width);
        if WIDTHS.contains(&width) {
            insta::assert_snapshot!(format!("{name}_{width}"), text(&view.lines));
        }
    }
}

#[test]
fn slash_menu_under_input() {
    let bottom = typed("/");
    snapshot_widths("slash_menu_under_input", &bottom);

    let theme = theme();
    let lines = bottom.render(&ctx(&theme, 80, 0.0)).lines;
    let shown = text(&lines);
    let rows: Vec<&str> = shown.lines().collect();
    let rule = rows
        .iter()
        .rposition(|row| row.starts_with('─'))
        .expect("the input's bottom rule");
    // Category names sit over their commands, which are indented under them, until you type.
    // The commands sit where they do once you type, under what you typed.
    assert_eq!(rows[rule + 1], "Session", "{shown}");
    assert!(rows[rule + 2].starts_with("  /clear"), "{shown}");
    // The rows left show the menu's first commands in its order, here all of them Session's.
    let entries: Vec<&str> = rows
        .iter()
        .filter_map(|row| row.strip_prefix("  /"))
        .filter_map(|row| row.split_whitespace().next())
        .collect();
    let items = command_items(None);
    let first: Vec<&str> = items
        .iter()
        .take(12)
        .map(|item| item.name.as_str())
        .collect();
    assert_eq!(entries, first, "{shown}");
    let groups: Vec<&str> = items
        .iter()
        .filter_map(|item| item.group.as_deref())
        .collect();
    assert!(
        groups[..12].iter().all(|group| *group == "Session"),
        "{groups:?}"
    );
    assert!(groups.contains(&"Work"), "{groups:?}");
    for line in &lines {
        assert!(line.width() <= 80, "one row each: {line:?}");
    }
    // Like the mode line it covers, the menu leaves the last column empty.
    for row in rows[rule + 1..].iter() {
        assert!(row.trim_end().chars().count() < 80, "{row:?}");
    }

    let narrowed = text(&typed("/wo").render(&ctx(&theme, 80, 0.0)).lines);
    let names: Vec<&str> = narrowed
        .lines()
        .filter_map(|row| row.strip_prefix("  /"))
        .filter_map(|row| row.split_whitespace().next())
        .collect();
    assert_eq!(names, ["workflows"], "{narrowed}");
}

#[test]
fn slash_menu_with_the_main_agents_commands() {
    use argonauts_core::names::VendorId;
    use argonauts_core::vendor_commands::{
        CommandKind, CommandScope, CommandSource, VendorCommand, VendorCommandSet,
    };
    let command = |name: &str, args: &str, help: &str, source: CommandSource| VendorCommand {
        name: name.into(),
        aliases: Vec::new(),
        args: args.into(),
        help: help.into(),
        kind: CommandKind::Vendor,
        source,
    };
    let set = VendorCommandSet::new(
        VendorId("claude-code".into()),
        "Claude Code",
        &[],
        vec![
            command(
                "ship",
                "[branch]",
                "Ship the branch once the checks pass",
                CommandSource::Custom {
                    scope: CommandScope::Project,
                    path: "/repo/.claude/commands/ship.md".into(),
                },
            ),
            command(
                "shared-notes:share",
                "",
                "Share the notes with the team",
                CommandSource::Plugin {
                    plugin: "shared-notes".into(),
                },
            ),
        ],
    );
    let mut bottom = typed("/sh");
    bottom.menus.commands = command_items(Some(&set));
    snapshot_widths("slash_menu_with_the_main_agents_commands", &bottom);
}

#[test]
fn mention_list_under_input() {
    let bottom = typed("tell @c");
    snapshot_widths("mention_list_under_input", &bottom);

    let theme = theme();
    let shown = text(&bottom.render(&ctx(&theme, 120, 0.0)).lines);
    let names: Vec<&str> = shown
        .lines()
        .filter_map(|row| row.strip_prefix("  @"))
        .filter_map(|row| row.split_whitespace().next())
        .collect();
    assert_eq!(
        names,
        ["claude-code", "codex", "card-002", "card-003"],
        "{shown}"
    );
}
