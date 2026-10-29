use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;
use crate::overview::sample;
use crate::render::Clock;
use crate::text::plain_text;
use crate::theme::{ColorLevel, Glyphs, Theme};

fn plain() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn band(mods: &[ModName]) -> Band {
    Band {
        mods: mods.to_vec(),
        usage: Some(Arc::new(sample::usage())),
        names: sample::names(),
        wall: sample::now(),
        // Each test draws many heights in one instant.
        settle: Duration::ZERO,
        ..Band::default()
    }
}

fn rows_in(band: &Band, width: u16, room: usize, question_up: bool) -> Vec<String> {
    let theme = plain();
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    plain_text(&band.lines(&ctx, room, question_up, false))
        .lines()
        .map(|row| row.trim_end().to_owned())
        .collect()
}

fn rows(band: &Band) -> Vec<String> {
    rows_in(band, 110, 40, false)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

const BOTH: [ModName; 2] = [ModName::PlanUsage, ModName::Spend];

#[test]
fn pinned_mods_stack_in_order_each_with_its_fold_toggle() {
    let rows = rows(&band(&BOTH));
    let at = |text: &str| rows.iter().position(|row| row.contains(text));
    let (plan, spend) = (at("◆ plan usage").unwrap(), at("◆ spend").unwrap());
    assert!(plan < spend, "{rows:#?}");
    assert!(rows[plan - 1].ends_with("╮ [-]"), "{rows:#?}");
    assert!(rows[spend - 1].ends_with("╮ [-]"), "{rows:#?}");
    assert!(rows.iter().any(|row| row.contains("Claude Code    Max")));
    // A row per plan window leaves the spend card its bar, not its rows.
    assert!(rows[spend + 1].contains("████"), "{rows:#?}");
    assert!(rows.len() <= 14, "{rows:#?}");
}

#[test]
fn nothing_pinned_or_a_band_turned_off_draws_nothing() {
    assert!(rows(&band(&[])).is_empty());
    let off = Band {
        shown: false,
        ..band(&BOTH)
    };
    assert!(rows(&off).is_empty());
    // A name no card here answers to is another mod's to draw.
    assert!(rows(&band(&[ModName::Narrator])).is_empty());
}

#[test]
fn a_folded_mod_is_one_line_with_its_number() {
    let folded = Band {
        folded: vec![ModName::Spend],
        ..band(&BOTH)
    };
    let rows = rows(&folded);
    let line = rows.last().unwrap();
    assert!(line.starts_with(" ▸ ◆ spend  ~$1.86 today"), "{rows:#?}");
    assert!(line.ends_with("[+]"), "{rows:#?}");
    assert!(rows.iter().any(|row| row.contains("Claude Code    Max")));
}

#[test]
fn short_of_rows_the_last_mod_steps_down_first_then_the_band_hides() {
    let band = band(&BOTH);
    let full = rows(&band).len();
    // One row short: spend loses its legend, and plan usage keeps every row.
    let stepped = rows_in(&band, 110, full - 1, false);
    assert!(stepped.len() < full, "{stepped:#?}");
    assert!(stepped.iter().any(|row| row.contains("agy            Pro")));
    assert!(!stepped.iter().any(|row| row.contains("on Claude Code")));

    let lines = rows_in(&band, 110, 2, false);
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(lines.iter().all(|row| row.ends_with("[+]")), "{lines:#?}");

    let hidden = rows_in(&band, 110, 1, false);
    assert_eq!(
        hidden,
        [" ▸ 2 mods hidden · Codex 5-hour 92% · ~$1.86 today · ctrl+x ctrl+a to show"]
    );
    assert!(rows_in(&band, 110, 0, false).is_empty());
}

#[test]
fn ctrl_x_ctrl_a_hides_the_band_to_one_line_that_keeps_each_number() {
    let mut band = band(&BOTH);
    band.toggle_hidden();
    assert_eq!(
        rows(&band),
        [" ▸ 2 mods hidden · Codex 5-hour 92% · ~$1.86 today · ctrl+x ctrl+a to show"]
    );
    assert!(!band.focus(), "a hidden band takes no keys");
    band.toggle_hidden();
    assert!(rows(&band).len() > 1);
}

#[test]
fn a_question_folds_the_band_and_a_narrow_screen_does_too() {
    let band = band(&BOTH);
    let under_question = rows_in(&band, 110, 40, true);
    assert_eq!(under_question.len(), 2, "{under_question:#?}");
    let narrow = rows_in(&band, 50, 40, false);
    assert_eq!(narrow.len(), 2, "{narrow:#?}");
    let hide = Band {
        when_question: WhenQuestion::Hide,
        ..band
    };
    assert!(rows_in(&hide, 110, 40, true).is_empty());
}

#[test]
fn the_keys_move_fold_open_unpin_and_reorder_the_mod_that_has_them() {
    let mut band = band(&BOTH);
    assert_eq!(band.handle_key(key(KeyCode::Tab)), BandEvent::Unhandled);
    assert!(band.focus() && band.has_focus());
    let (plan_usage, spend) = (ModName::PlanUsage, ModName::Spend);
    assert_eq!(
        band.handle_key(key(KeyCode::Char('-'))),
        BandEvent::Fold {
            name: plan_usage,
            open: false
        }
    );
    assert_eq!(
        band.handle_key(key(KeyCode::Enter)),
        BandEvent::Open(plan_usage)
    );
    assert_eq!(band.handle_key(key(KeyCode::Tab)), BandEvent::Moved);
    assert_eq!(
        band.handle_key(key(KeyCode::Char('['))),
        BandEvent::Move {
            name: spend,
            up: true
        }
    );
    assert_eq!(
        band.handle_key(key(KeyCode::Char(','))),
        BandEvent::Settings(plan_usage),
        "the keys follow the mod up"
    );
    assert_eq!(band.handle_key(key(KeyCode::Tab)), BandEvent::Moved);
    assert_eq!(band.handle_key(key(KeyCode::Tab)), BandEvent::Past);
    assert_eq!(
        band.handle_key(key(KeyCode::Char('p'))),
        BandEvent::Unpin(spend)
    );
    assert_eq!(band.handle_key(key(KeyCode::Esc)), BandEvent::Leave);
    assert!(!band.has_focus());

    // A folded mod opens again with the same key.
    let mut band = Band {
        folded: vec![plan_usage],
        ..band
    };
    band.focus();
    assert_eq!(
        band.handle_key(key(KeyCode::Char(' '))),
        BandEvent::Fold {
            name: plan_usage,
            open: true
        }
    );
}

#[test]
fn drawing_the_band_asks_for_its_numbers() {
    let theme = plain();
    let now = Instant::now();
    let ctx = RenderCtx::new(110, &theme, Clock::still(now));
    let waiting = Band {
        usage: None,
        ..band(&BOTH)
    };
    assert!(waiting.lines(&ctx, 40, false, false).is_empty());
    assert!(
        waiting.watch.due(now).is_some(),
        "nothing to draw yet, so it asks"
    );

    let unpinned = band(&[]);
    unpinned.lines(&ctx, 40, false, false);
    assert!(
        unpinned.watch.due(now).is_none(),
        "nothing pinned, nothing asked"
    );
}

#[test]
fn two_half_width_mods_share_a_row_on_a_wide_screen() {
    let pair = [ModName::Tokens, ModName::History];
    let halves = Band {
        halves: pair.to_vec(),
        ..band(&pair)
    };
    let wide = rows_in(&halves, 130, 40, false);
    let top = &wide[0];
    assert_eq!(top.matches("╮ [-]").count(), 2, "{wide:#?}");
    let titles = wide.iter().find(|row| row.contains("◆ tokens")).unwrap();
    assert!(titles.contains("◆ history"), "{wide:#?}");
    assert!(wide.iter().all(|row| crate::text::str_width(row) <= 130));

    // Narrower than `side_by_side_at`, or with full-width mods, they stack.
    let narrow = rows_in(&halves, 110, 40, false);
    let at = |text: &str| narrow.iter().position(|row| row.contains(text)).unwrap();
    assert!(at("◆ tokens") < at("◆ history"), "{narrow:#?}");
    let full = rows_in(&band(&pair), 130, 40, false);
    assert_eq!(full[0].matches("╮ [-]").count(), 1, "{full:#?}");

    // One of the two folded leaves the other its whole row.
    let folded = Band {
        folded: vec![ModName::History],
        ..halves
    };
    let rows = rows_in(&folded, 130, 40, false);
    assert_eq!(rows[0].matches("╮ [-]").count(), 1, "{rows:#?}");
    assert!(
        rows.last().unwrap().starts_with(" ▸ ◆ history"),
        "{rows:#?}"
    );
}

#[test]
fn a_folded_mod_names_its_own_key() {
    let folded = Band {
        folded: vec![ModName::PlanUsage],
        keys: vec![(ModName::PlanUsage, "ctrl+x q".to_owned())],
        ..band(&BOTH)
    };
    let rows = rows(&folded);
    let line = rows
        .iter()
        .find(|row| row.contains("◆ plan usage"))
        .unwrap();
    assert!(line.ends_with("ctrl+x q  [+]"), "{rows:#?}");
    // A mod with no key bound shows only its toggle.
    let spend = Band {
        folded: vec![ModName::Spend],
        ..folded
    };
    let rows = self::rows(&spend);
    let line = rows.iter().find(|row| row.contains("◆ spend")).unwrap();
    assert!(!line.contains("ctrl+x"), "{rows:#?}");
}

#[test]
fn a_change_of_height_waits_a_moment_then_lands_in_one_frame() {
    let theme = plain();
    let t0 = Instant::now();
    let draw = |band: &Band, at: Instant| {
        let ctx = RenderCtx::new(110, &theme, Clock::still(at));
        band.lines(&ctx, 40, false, false).len()
    };
    let mut band = Band {
        settle: Duration::from_millis(150),
        ..band(&BOTH)
    };
    // The first draw has nothing to hold on to.
    let open = draw(&band, t0);
    assert!(open > 2);
    assert_eq!(band.next_change(), None);

    band.folded = BOTH.to_vec();
    assert_eq!(draw(&band, t0 + Duration::from_millis(10)), open);
    assert_eq!(band.next_change(), Some(t0 + Duration::from_millis(160)));
    assert_eq!(draw(&band, t0 + Duration::from_millis(100)), open);
    assert_eq!(draw(&band, t0 + Duration::from_millis(160)), 2);
    assert_eq!(band.next_change(), None);

    // Rows that no longer fit give way at once, and so do rows of another width.
    band.folded.clear();
    assert_eq!(draw(&band, t0 + Duration::from_millis(400)), 2);
    let ctx = RenderCtx::new(110, &theme, Clock::still(t0 + Duration::from_millis(410)));
    assert_eq!(band.lines(&ctx, 1, false, false).len(), 1);
    let ctx = RenderCtx::new(100, &theme, Clock::still(t0 + Duration::from_millis(420)));
    assert_eq!(band.lines(&ctx, 40, false, false).len(), open);
}

/// A band with the run's cards and the context card fed, as the bridge feeds them.
fn following_a_run(mods: &[ModName]) -> Band {
    Band {
        run: Some(PinnedRun {
            summary: sample::status_with_limit(),
            title: Some("export-reports-csv".to_owned()),
        }),
        context: Some(crate::overview::conversation_context(&sample::counted())),
        ..band(mods)
    }
}

#[test]
fn the_runs_card_pins_without_its_goal_and_plan_and_folds_to_how_far_it_is() {
    let pinned = following_a_run(&[ModName::Run]);
    let rows = rows(&pinned);
    assert!(
        rows[1].starts_with(" │ ◆ run-03 · export-reports-csv"),
        "{rows:#?}"
    );
    assert!(rows[1].contains("1 of 4 merged [needs you]"), "{rows:#?}");
    assert!(rows[2].contains('█'), "{rows:#?}");
    assert!(rows[3].contains("▌ merged 1"), "{rows:#?}");
    assert!(rows[4].contains("task-001    CSV writer"), "{rows:#?}");
    assert!(
        !rows.iter().any(|row| row.contains("Goal  ")),
        "above the input rows are dear: {rows:#?}"
    );
    assert_eq!(rows.len(), 9, "{rows:#?}");

    let folded = Band {
        folded: vec![ModName::Run],
        ..pinned
    };
    let rows = self::rows(&folded);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert!(
        rows[0].starts_with(" ▸ ◆ run-03 · export-reports-csv  1 of 4 merged (needs you)"),
        "{rows:#?}"
    );
}

#[test]
fn the_runs_spend_and_what_needs_you_pin_as_cards_of_their_own() {
    let rows = rows(&following_a_run(&[ModName::RunLimits, ModName::Waiting]));
    let at = |text: &str| rows.iter().position(|row| row.contains(text));
    let spend = at("◆ spend · run-03").unwrap();
    assert!(rows[spend].contains("$1.20 of $3.00"), "{rows:#?}");
    assert!(rows[spend].contains("40%"), "{rows:#?}");
    assert!(rows[spend + 1].contains("spend"), "{rows:#?}");
    let waits = at("◆ needs you").unwrap();
    assert!(spend < waits, "{rows:#?}");
    assert!(rows[waits - 1].ends_with("╮ [-]"), "{rows:#?}");
    assert!(rows[waits].ends_with("[1] │"), "{rows:#?}");
    assert!(rows[waits + 1].contains("Review asks"), "{rows:#?}");

    let folded = Band {
        folded: vec![ModName::RunLimits, ModName::Waiting],
        ..following_a_run(&[ModName::RunLimits, ModName::Waiting])
    };
    let rows = self::rows(&folded);
    assert!(
        rows[0].starts_with(" ▸ ◆ spend · run-03  $1.20 of $3.00 (40%)"),
        "{rows:#?}"
    );
    assert!(
        rows[1].starts_with(" ▸ ◆ needs you  1 · task-004 Review asks which date format"),
        "{rows:#?}"
    );
}

#[test]
fn the_context_card_pins_by_category_and_folds_to_how_full_it_is() {
    let pinned = following_a_run(&[ModName::Context]);
    let rows = rows(&pinned);
    assert!(rows[1].starts_with(" │ ◆ context"), "{rows:#?}");
    assert!(rows[1].contains("of 1M"), "{rows:#?}");
    assert!(rows[1].contains("compacts at"), "{rows:#?}");
    assert!(
        rows.iter().any(|row| row.contains("▌ System prompt")),
        "{rows:#?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("▌ Messages")),
        "{rows:#?}"
    );

    let folded = Band {
        folded: vec![ModName::Context],
        ..pinned.clone()
    };
    let line = &self::rows(&folded)[0];
    assert!(line.starts_with(" ▸ ◆ context  21% · "), "{line}");
    assert!(line.contains("of 1M"), "{line}");

    // Hidden, the band still says each card's number.
    let hidden = Band {
        hidden: true,
        ..following_a_run(&[ModName::Context, ModName::Run, ModName::RunLimits])
    };
    let line = &self::rows(&hidden)[0];
    assert!(
        line.starts_with(" ▸ 3 mods hidden · context 21% · "),
        "{line}"
    );
    assert!(line.contains("1 of 4 merged (needs you)"), "{line}");
    assert!(line.contains("$1.20 of $3.00 (40%)"), "{line}");
}

#[test]
fn a_card_with_nothing_to_show_takes_no_row_and_no_key() {
    // No run is followed, so its cards have nothing to draw, and usage hasn't come.
    let mut waiting = Band {
        usage: None,
        ..band(&[ModName::Run, ModName::RunLimits, ModName::Spend])
    };
    assert!(rows(&waiting).is_empty());
    assert!(!waiting.shows());
    assert!(!waiting.focus(), "nothing is there to take the keys");

    // The run's cards draw before the usage numbers come, from what the screen holds.
    let run_only = Band {
        usage: None,
        ..following_a_run(&[ModName::Spend, ModName::Run])
    };
    let rows = rows(&run_only);
    assert!(rows[1].contains("◆ run-03"), "{rows:#?}");
    assert!(!rows.iter().any(|row| row.contains("today")), "{rows:#?}");
}

#[test]
fn a_run_that_ends_hands_the_keys_to_the_last_card_left() {
    let mut band = following_a_run(&[ModName::Spend, ModName::Run]);
    assert!(band.focus());
    assert_eq!(band.handle_key(key(KeyCode::Tab)), BandEvent::Moved);
    assert_eq!(
        band.handle_key(key(KeyCode::Enter)),
        BandEvent::Open(ModName::Run)
    );
    band.run = None;
    assert_eq!(
        band.handle_key(key(KeyCode::Enter)),
        BandEvent::Open(ModName::Spend),
        "the run's card left, so the keys are on the one above it"
    );
}

/// The stats card asks for its numbers by itself, for the period its file sets, even after
/// a tab brought numbers for another.
#[test]
fn the_stats_card_asks_for_the_period_its_file_sets() {
    use argonauts_core::api::UsagePeriod;
    use argonauts_core::app::ViewRequest;
    use argonauts_core::settings::stats::StatsPeriod;

    let theme = plain();
    let now = Instant::now();
    let ctx = RenderCtx::new(110, &theme, Clock::still(now));
    let waiting = Band {
        usage: None,
        ..band(&[ModName::Stats])
    };
    waiting.lines(&ctx, 40, false, false);
    assert!(waiting.watch.due(now).is_some(), "pinned alone, it asks");

    let mut usage = sample::usage();
    usage.period = UsagePeriod::Week;
    let mut stats = sample::crew_stats();
    stats.settings.period = StatsPeriod::Month;
    usage.stats = Some(stats);
    let after_the_tab = Band {
        usage: Some(Arc::new(usage)),
        ..band(&[ModName::Stats])
    };
    after_the_tab.lines(&ctx, 40, false, false);
    assert_eq!(
        after_the_tab.watch.due(now),
        Some(ViewRequest::Usage {
            period: UsagePeriod::Month
        })
    );
}

/// The bottom line's `stats` part draws the same numbers, so they are asked for while the
/// line lists it, with no card pinned and the band off.
#[test]
fn the_line_listing_stats_asks_for_its_numbers() {
    use argonauts_core::api::UsagePeriod;
    use argonauts_core::app::ViewRequest;
    use argonauts_core::settings::stats::StatsPeriod;

    let theme = plain();
    let now = Instant::now();
    let ctx = RenderCtx::new(110, &theme, Clock::still(now));
    let unlisted = Band {
        usage: None,
        shown: false,
        ..band(&[])
    };
    unlisted.lines(&ctx, 40, false, false);
    assert_eq!(unlisted.watch.due(now), None, "nothing draws them");

    let mut usage = sample::usage();
    let mut stats = sample::crew_stats();
    stats.settings.period = StatsPeriod::Week;
    usage.stats = Some(stats);
    let listed = Band {
        usage: Some(Arc::new(usage)),
        shown: false,
        line_stats: true,
        ..band(&[])
    };
    assert!(listed.lines(&ctx, 40, false, false).is_empty());
    assert_eq!(
        listed.watch.due(now),
        Some(ViewRequest::Usage {
            period: UsagePeriod::Week
        })
    );
}
