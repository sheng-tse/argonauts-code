use super::*;

#[test]
fn windows_are_named_in_words_never_by_their_keys() {
    assert_eq!(window_words("session", None), "5-hour");
    assert_eq!(window_words("week", None), "week");
    assert_eq!(window_words("Fable week", None), "Fable week");
    assert_eq!(window_words("weekly", None), "week");
    assert_eq!(window_words("Gemini 5h", None), "Gemini 5-hour");
    assert_eq!(
        window_words("Claude, GPT", Some("5-hour")),
        "Claude, GPT 5-hour"
    );
    assert_eq!(fitted_words("Claude, GPT 5-hour", 16), "Claude, GPT 5h");
    assert_eq!(fitted_words("GPT 6 Astra week", 16), "GPT 6 Astra week");
}

#[test]
fn a_readings_key_reads_as_words_too() {
    assert_eq!(key_words("five_hour"), "5-hour");
    assert_eq!(key_words("seven_day"), "week");
    assert_eq!(key_words("weekly"), "week");
    assert_eq!(key_words("seven_day_opus"), "Opus week");
    assert_eq!(key_words("model:Fable"), "Fable");
}

mod picture {
    use argonauts_core::base::names::AgentId;
    use argonauts_core::models::limits::PlanUsageSource;
    use argonauts_core::models::plan_usage::{LiveUsage, LiveWindow};
    use argonauts_core::runs::api::{PlanUsage, UsageSummary};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use time::OffsetDateTime;
    use time::macros::datetime;

    use crate::base::kit::{Form, Frame, Kit};
    use crate::panels::cards::{UsageCard, UsageCards};
    use crate::testing::pictures::golden::{Picture, Region, assert_matches, theme};

    /// The picture's clock: Thursday evening, as its 5-hour and Codex and agy weeks read.
    const NOW: OffsetDateTime = datetime!(2026-10-08 20:12 UTC);

    fn window(key: &str, label: &str, resets: Option<OffsetDateTime>, used: f64) -> LiveWindow {
        let mut window = LiveWindow::new(key, label, label).used(Some(used));
        window.resets_at = resets;
        window
    }

    fn plan(agent: &str, name: &str, tier: &str, windows: Vec<LiveWindow>) -> PlanUsage {
        let mut live = LiveUsage::new(AgentId(agent.into()), PlanUsageSource::AppServer, NOW);
        live.plan = Some(tier.into());
        live.windows = windows;
        PlanUsage {
            agent: agent.into(),
            name: name.into(),
            plan: Some(tier.into()),
            live: Some(live),
            ..PlanUsage::default()
        }
    }

    /// The picture's three CLIs and their windows.
    pub fn usage() -> UsageSummary {
        let tue = Some(datetime!(2026-10-13 22:00 UTC));
        let codex_week = Some(datetime!(2026-10-14 11:22 UTC));
        let agy_week = Some(datetime!(2026-10-14 22:52 UTC));
        let mut other_group = window("claude and gpt models:five_hour", "Claude, GPT", None, 0.0);
        other_group.note = Some("5-hour".into());
        let mut other_week = window(
            "claude and gpt models:weekly",
            "Claude, GPT week",
            agy_week,
            0.0,
        );
        other_week.page_only = true;
        UsageSummary {
            plans: vec![
                plan(
                    "claude-code",
                    "Claude Code",
                    "Max",
                    vec![
                        window(
                            "five_hour",
                            "session",
                            Some(datetime!(2026-10-08 22:40 UTC)),
                            0.0,
                        ),
                        window("seven_day", "week", tue, 0.36),
                        window("model:Fable", "Fable week", tue, 0.0),
                    ],
                ),
                plan(
                    "codex",
                    "Codex",
                    "Plus",
                    vec![
                        window("five_hour", "5-hour", None, 0.0),
                        window("weekly", "weekly", codex_week, 0.01),
                        window("gpt-6-astra:weekly", "GPT 6 Astra weekly", codex_week, 0.0),
                    ],
                ),
                plan(
                    "antigravity-cli",
                    "agy",
                    "Pro",
                    vec![
                        window("five_hour", "Gemini 5h", None, 0.0),
                        window("weekly", "Gemini week", agy_week, 0.0),
                        other_group,
                        other_week,
                    ],
                ),
            ],
            ..UsageSummary::default()
        }
    }

    /// The Limits tab's plan usage card at the picture's width, in its colours. The picture's
    /// Claude Code weeks say `resets in 4d 1h` where its other rows put the clock a day
    /// earlier: on that clock the same reset reads `5d 2h`, the only cells that are different.
    /// The same card where the terminal draws logos: each CLI's logo before its name, and the
    /// names' column two cells wider to hold it.
    #[test]
    fn the_limits_card_puts_each_clis_logo_before_its_name() {
        use crate::base::kit::logo::golden::{count, pictured, with_stand_ins};
        use crate::testing::pictures::golden::keep;

        let theme = pictured(&theme());
        let kit = Kit::new(&theme);
        let usage = usage();
        let names = argonauts_core::look::display_names::DisplayNames::default();
        let cards = UsageCards {
            usage: &usage,
            names: &names,
            now: NOW,
            left: false,
        };
        let drawn = cards.draw(&kit, UsageCard::PlanUsage, Form::Full, Frame::view(120));
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, drawn.lines.len() as u16));
        for (y, line) in drawn.lines.iter().enumerate() {
            buf.set_line(0, y as u16, line, 120);
        }
        assert_eq!(count(&buf), 3);
        let shown = with_stand_ins(&buf);
        keep("limits_logos", &shown);
        let picture = Picture::load("limits_logos");
        assert_matches(&picture, &shown, &Region::whole(&picture, (0, 0)), &theme);
    }

    #[test]
    fn the_limits_card_matches_its_picture() {
        let picture = Picture::load("limits_after");
        let theme = theme();
        let kit = Kit::new(&theme);
        let usage = usage();
        let names = argonauts_core::look::display_names::DisplayNames::default();
        let cards = UsageCards {
            usage: &usage,
            names: &names,
            now: NOW,
            left: false,
        };
        let drawn = cards.draw(&kit, UsageCard::PlanUsage, Form::Full, Frame::view(120));
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, drawn.lines.len() as u16));
        for (y, line) in drawn.lines.iter().enumerate() {
            buf.set_line(0, y as u16, line, 120);
        }
        let whole = Region::whole(&picture, (1, 0));
        assert_matches(&picture, &buf, &whole, &theme);
    }
}
