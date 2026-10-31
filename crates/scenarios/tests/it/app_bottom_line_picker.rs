//! Config › Mods › bottom line › CLIs has a fifth choice, the ones you pick, whose enter opens
//! the same picker `/bottom-line` opens. The list it saves shows under the row, and choosing one
//! of the automatic choices again takes it out of the file.

use std::path::{Path, PathBuf};

use argonauts_scenarios::WAIT;
use argonauts_testkit::{Key, Pty};

use crate::app_support::*;

/// The sizes the screen is held to, as `cols × rows`.
const SIZES: [(u16, u16); 2] = [(120, 38), (177, 40)];

/// Steps through the CLIs row's choices: the four the line picks itself, then the fifth.
const TO_THE_FIFTH: usize = 4;

/// The bottom line's file, wherever the page saves it.
const LINE_FILE: &str = "bottom-line.toml";

/// A list on screen: which rows belong to it, how the pointed one starts, and the column, in
/// characters, where each row's name starts.
struct List {
    rows: fn(&str) -> bool,
    pointed: &'static str,
    name_from: usize,
}

/// The list of mods, the left column of its panel, so a row starts at the frame's left edge.
const MODS: List = List {
    rows: |row| row.starts_with("│ ❯ ◆ ") || row.starts_with("│   ◆ "),
    pointed: "│ ❯ ◆ ",
    name_from: 6,
};

/// A mod page's settings, under headings that sit one column further left.
const SETTINGS: List = List {
    rows: |row| row.starts_with(" ❯ ") || (row.starts_with("   ") && !row[3..].starts_with(' ')),
    pointed: " ❯ ",
    name_from: 3,
};

/// Moves the pointer along `list` to the row that starts with `name`.
fn point_at(pty: &mut Pty, list: &List, name: &str) -> anyhow::Result<()> {
    let screen = pty.screen();
    let rows: Vec<&str> = screen
        .visible
        .iter()
        .map(String::as_str)
        .filter(|row| (list.rows)(row))
        .collect();
    let at = rows.iter().position(|row| row.starts_with(list.pointed));
    let named = format!("{name} ");
    let to = rows.iter().position(|row| {
        let from: String = row.chars().skip(list.name_from).collect();
        from.starts_with(&named)
    });
    let (Some(at), Some(to)) = (at, to) else {
        anyhow::bail!("no row named {name}:\n{}", screen.visible_text());
    };
    let key = if to < at { Key::Up } else { Key::Down };
    for _ in 0..at.abs_diff(to) {
        pty.press(key)?;
    }
    pty.wait_for_visible(&format!("{}{name} ", list.pointed), WAIT)?;
    Ok(())
}

/// Opens Config › Mods › bottom line with the pointer on its CLIs row.
fn open_the_clis_row(pty: &mut Pty) -> anyhow::Result<()> {
    submit(pty, "/mods")?;
    pty.wait_for_visible("Config › Mods", WAIT)?;
    point_at(pty, &MODS, "bottom line")?;
    pty.press(Key::Enter)?;
    pty.wait_for_visible("Config › Mods › bottom line", WAIT)?;
    point_at(pty, &SETTINGS, "CLIs")
}

/// Every copy of the bottom line's file under `dir`.
fn line_files(dir: &Path, found: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            line_files(&path, found)?;
        } else if path.file_name().is_some_and(|name| name == LINE_FILE) {
            found.push(path);
        }
    }
    Ok(())
}

/// What the bottom line's files hold, together.
fn saved_text(scenario: &AppScenario) -> anyhow::Result<String> {
    let mut files = Vec::new();
    line_files(scenario.repo.home(), &mut files)?;
    line_files(scenario.repo.root(), &mut files)?;
    let mut text = String::new();
    for file in files {
        text.push_str(&std::fs::read_to_string(file)?);
    }
    Ok(text)
}

/// Whether `text` sets a list of CLIs, rather than naming one in a comment.
fn holds_a_list(text: &str) -> bool {
    text.lines()
        .any(|line| line.trim_start().starts_with("clis"))
}

#[test]
fn app_the_ones_you_pick_saves_a_list_and_an_automatic_choice_drops_it() -> anyhow::Result<()> {
    for (cols, rows) in SIZES {
        let scenario = scenario()?;
        let mut pty = scenario.launch_sized(&[], rows, cols)?;
        wait_ready(&scenario, &pty)?;
        open_the_clis_row(&mut pty)?;
        for _ in 0..TO_THE_FIFTH {
            pty.press(Key::Right)?;
        }
        pty.wait_for_visible("enter picks them and their order", WAIT)?;

        pty.press(Key::Enter)?;
        let picker = pty.wait_for_visible("space shows or hides", WAIT)?;
        assert!(
            picker
                .visible
                .iter()
                .any(|row| row.trim() == "The bottom line"),
            "{cols}x{rows}: /bottom-line's picker is open:\n{}",
            picker.visible_text()
        );
        // The CLI in use always shows, so the second row is the first one the picker changes.
        pty.press(Key::Down)?;
        pty.press(Key::Char(' '))?;
        pty.press(Key::Enter)?;
        wait_until("the picked list saved", || {
            Ok(holds_a_list(&saved_text(&scenario)?).then_some(()))
        })?;

        open_the_clis_row(&mut pty)?;
        let listed = pty.wait_for_visible("· in this order · enter changes them", WAIT)?;
        assert!(
            listed.visible_text().contains("the ones you pick"),
            "{cols}x{rows}: the fifth choice stands with the list under it:\n{}",
            listed.visible_text()
        );

        pty.press(Key::Left)?;
        pty.press(Key::Char('s'))?;
        let text = wait_until("the list taken out of the file", || {
            let text = saved_text(&scenario)?;
            Ok((!holds_a_list(&text)).then_some(text))
        })?;
        assert!(
            text.lines()
                .any(|line| line.trim_start().starts_with("show")),
            "{cols}x{rows}: the automatic choice is saved in the list's place:\n{text}"
        );
    }
    Ok(())
}
