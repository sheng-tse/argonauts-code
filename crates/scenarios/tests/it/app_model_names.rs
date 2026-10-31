//! A model set by its CLI's alias is named by the model the alias runs, with its version, in
//! every place the screen names it: `opus` reads `Opus 5.5`, never `Opus`.

use std::fs;
use std::io::Write as _;

use anyhow::Context as _;
use argonauts_scenarios::{Scenario, WAIT, as_set_up};
use argonauts_testkit::approved::{self, Drawing, Entry, Manifest, Step};

use crate::app_support::*;
use crate::drawings_support::bring_with;

/// The sizes the screen is held to, as `cols × rows`.
const SIZES: [(u16, u16); 2] = [(120, 38), (177, 40)];

/// Claude Code set to its alias `opus`, which the catalog says runs Opus 5.5.
fn opus_config() -> String {
    format!("{GLOBAL_CONFIG}\n[agents.claude-code]\nmodel = \"opus\"\n")
}

/// The rows that name `Opus` without the version the alias runs.
fn bare_opus(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| {
            line.match_indices("Opus").any(|(at, word)| {
                let rest = &line[at + word.len()..];
                !rest
                    .strip_prefix(' ')
                    .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
            })
        })
        .collect()
}

#[test]
fn app_an_alias_reads_as_its_model_with_the_version() -> anyhow::Result<()> {
    for (cols, rows) in SIZES {
        let scenario = scenario()?;
        std::fs::write(scenario.repo.global_config(), as_set_up(&opus_config()))?;
        let mut pty = scenario.launch_sized(&[], rows, cols)?;
        wait_ready(&scenario, &pty)?;
        wait_start_through(&scenario)?;
        let screen = pty.wait_for_visible("Opus 5\\.5", WAIT)?;
        let header = screen
            .visible
            .iter()
            .find(|row| row.contains("Claude Code ·"))
            .expect("the header's model line");
        assert!(header.contains("Opus 5.5"), "{cols}x{rows}: {header}");
        assert_eq!(
            bare_opus(&screen.visible_text()),
            Vec::<&str>::new(),
            "{cols}x{rows}"
        );

        start_captain(&scenario, &mut pty, requests::SLOW_TASK)?;
        wait_worker_started(&scenario)?;
        let screen = pty.wait_for_visible("on Claude Code", WAIT)?;
        assert_eq!(
            bare_opus(&screen.visible_text()),
            Vec::<&str>::new(),
            "{cols}x{rows}"
        );
    }
    Ok(())
}

/// The approved drawing `file`'s row, with its home and fake CLIs.
fn drawn(manifest: &Manifest, file: &str) -> anyhow::Result<Entry> {
    manifest
        .drawings
        .iter()
        .find(|entry| entry.file == file)
        .cloned()
        .with_context(|| format!("no {file}"))
}

/// `entry`'s drawing as it would be at `cols` × `rows`.
fn drawing_at(
    dir: &std::path::Path,
    entry: &Entry,
    cols: u16,
    rows: u16,
) -> anyhow::Result<Drawing> {
    let mut drawing = Drawing::load(&dir.join(&entry.file))?;
    (drawing.cols, drawing.rows) = (cols, rows);
    Ok(drawing)
}

/// Seats the Council's first seat, set to `claude-code/claude-opus-5-5` in the home, by the
/// alias `claude-code/opus` instead.
fn seat_the_alias(scenario: &Scenario) -> anyhow::Result<()> {
    let config = scenario.repo.home().join("config.toml");
    let text = fs::read_to_string(&config)?;
    let full = "\"claude-code/claude-opus-5-5\"";
    anyhow::ensure!(text.contains(full), "{} seats no {full}", config.display());
    fs::write(&config, text.replace(full, "\"claude-code/opus\""))?;
    Ok(())
}

/// A Council whose first seat is set by the alias `claude-code/opus` names that seat `Opus 5.5`
/// in its row under the input, and nowhere `Opus` alone.
#[test]
fn app_a_seat_set_by_an_alias_reads_as_its_model_with_the_version() -> anyhow::Result<()> {
    let dir = approved::dir();
    let manifest = Manifest::load(&dir)?;
    let entry = drawn(&manifest, "council/sent@120x38.txt")?;
    for (cols, rows) in SIZES {
        let drawing = drawing_at(&dir, &entry, cols, rows)?;
        let at = bring_with(&entry, &dir, &drawing, seat_the_alias)?;
        let screen = at
            .pty
            .wait_for_visible("○ +\\S+ Opus 5\\.5 +Writing its first answer", WAIT)?
            .visible_text();
        assert_eq!(bare_opus(&screen), Vec::<&str>::new(), "{cols}x{rows}");
    }
    Ok(())
}

/// Usage recorded for Claude Code's alias `opus` reads `Opus 5.5` in Overview › Models: its
/// Favorite and its model's own rows.
#[test]
fn app_overview_names_an_alias_by_its_model_with_the_version() -> anyhow::Result<()> {
    let dir = approved::dir();
    let manifest = Manifest::load(&dir)?;
    let mut entry = drawn(&manifest, "behaviors/label-map@120x38.txt")?;
    entry.steps = vec![
        Step::Wait("shift\\+tab to cycle".to_owned()),
        Step::Send("/overview\r".to_owned()),
        Step::Wait("Yes, import it".to_owned()),
        Step::Press("2".to_owned()),
        Step::Wait("Overview    Models    Behaviors".to_owned()),
        Step::Press("Down".to_owned()),
        Step::Press("Tab".to_owned()),
        Step::Wait("★ Favorite: ".to_owned()),
    ];
    for (cols, rows) in SIZES {
        let drawing = drawing_at(&dir, &entry, cols, rows)?;
        let at = bring_with(&entry, &dir, &drawing, |scenario| {
            let home = scenario.repo.home();
            let mut config = fs::OpenOptions::new()
                .append(true)
                .open(home.join("config.toml"))?;
            writeln!(config, "\n[agents.claude-code]\nmodel = \"opus\"")?;
            // The most tokens of any model in the home's records, so it is the favorite.
            let mut usage = fs::OpenOptions::new()
                .append(true)
                .open(home.join("usage/2026-10.jsonl"))?;
            writeln!(
                usage,
                "{{\"day\": \"2026-10-19\", \"repo\": \"/Users/you/code/tasks-api\", \
                 \"agent\": \"claude-code\", \"model\": \"opus\", \"agent_id\": \"claude-code\", \
                 \"input\": 90000000, \"cached\": 0, \"output\": 400000, \"cost\": 0.0, \
                 \"estimated\": true, \"partial\": false, \"hour\": 15, \"took\": 60, \
                 \"billing\": \"plan\"}}"
            )?;
            Ok(())
        })?;
        let screen = at
            .pty
            .wait_for_visible("Opus 5\\.5 \\(", WAIT)?
            .visible_text();
        let row = screen.lines().find(|line| line.contains("★ Favorite: "));
        assert!(
            row.is_some_and(|row| row.contains("Favorite: Opus 5.5 ")),
            "{cols}x{rows}: {row:?}\n{screen}"
        );
        assert_eq!(bare_opus(&screen), Vec::<&str>::new(), "{cols}x{rows}");
    }
    Ok(())
}
