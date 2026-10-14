use std::sync::Arc;
use std::time::Instant;

use argonauts_core::api::UsagePeriod;
use argonauts_core::app::ViewData;
use argonauts_core::model_catalog::{CatalogEntry, ModelCatalog};
use argonauts_core::status_tab::{CliHealth, HealthRows, ModelStanding, Standing, StatusTabView};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::*;
use crate::kit::fit;
use crate::overview::golden::{self, Picture, Region};
use crate::overview::stats::draw;
use crate::overview::{OverviewView, Tab, UsageWatch, home, sample};
use crate::state::AppState;
use crate::views::{View, ViewCtx};

/// The CLIs' colors, and the two a CLI without one of its own takes.
const BASES: [Role; 5] = [
    Role::CliClaude,
    Role::CliCodex,
    Role::CliAgy,
    Role::AgentPurple,
    Role::AgentPink,
];

fn full_color_theme(brightness: Brightness) -> Theme {
    Theme::for_brightness(brightness).with_level(ColorLevel::TrueColor)
}

fn shade_rgb(kit: &Kit<'_>, base: Role, nth: usize) -> Rgb {
    match shade(kit, base, nth).fg {
        Some(Color::Rgb(r, g, b)) => (r, g, b),
        other => panic!("{base:?} shade {nth}: {other:?}"),
    }
}

/// A color in CIELAB, under D65.
fn lab((r, g, b): Rgb) -> [f64; 3] {
    let linear = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (linear(r), linear(g), linear(b));
    let f = |t: f64| {
        if t > 216.0 / 24389.0 {
            t.cbrt()
        } else {
            (24389.0 / 27.0 * t + 16.0) / 116.0
        }
    };
    let x = f((0.4124564 * r + 0.3575761 * g + 0.1804375 * b) / 0.95047);
    let y = f(0.2126729 * r + 0.7151522 * g + 0.0721750 * b);
    let z = f((0.0193339 * r + 0.1191920 * g + 0.9503041 * b) / 1.08883);
    [116.0 * y - 16.0, 500.0 * (x - y), 200.0 * (y - z)]
}

/// How far apart two colors are in CIELAB.
fn apart(a: Rgb, b: Rgb) -> f64 {
    let (a, b) = (lab(a), lab(b));
    (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f64>().sqrt()
}

#[test]
fn the_shades_of_one_color_stand_at_least_15_apart_on_either_screen() {
    for brightness in [Brightness::Dark, Brightness::Light] {
        let theme = full_color_theme(brightness);
        let kit = Kit::new(&theme);
        for base in BASES {
            let shades: Vec<Rgb> = (0..SHADES).map(|nth| shade_rgb(&kit, base, nth)).collect();
            for (at, a) in shades.iter().enumerate() {
                for b in &shades[at + 1..] {
                    let far = apart(*a, *b);
                    assert!(
                        far >= 15.0,
                        "{brightness:?} {base:?}: {a:?} and {b:?} are {far:.1} apart"
                    );
                }
            }
        }
    }
}

#[test]
fn the_first_shade_is_the_clis_own_and_the_fifth_model_starts_again() {
    for brightness in [Brightness::Dark, Brightness::Light] {
        let theme = full_color_theme(brightness);
        let kit = Kit::new(&theme);
        for base in BASES {
            assert_eq!(shade(&kit, base, 0), kit.fg(base));
            assert_eq!(shade(&kit, base, SHADES), kit.fg(base));
            assert_eq!(shade(&kit, base, SHADES + 1), shade(&kit, base, 1));
        }
    }
}

/// A color of your own has no fixed shades, so they are mixed from it.
#[test]
fn a_color_you_set_is_mixed_into_shades_of_its_own() {
    let mut theme = full_color_theme(Brightness::Dark);
    theme.set(
        Role::CliClaude,
        Style::new().fg(Color::Rgb(0x80, 0xc0, 0x40)),
    );
    let kit = Kit::new(&theme);
    assert_eq!(shade_rgb(&kit, Role::CliClaude, 0), (0x80, 0xc0, 0x40));
    let deep = shade_rgb(&kit, Role::CliClaude, 1);
    assert_eq!(deep, (0x5a, 0x86, 0x2d), "three tenths toward black");
    let pale = shade_rgb(&kit, Role::CliClaude, 3);
    assert!(pale.0 > 0xe0 && pale.1 > 0xf0 && pale.2 > 0xe0, "{pale:?}");
}

#[test]
fn fewer_colors_have_fewer_shades() {
    let sixteen = Theme::dark().with_level(ColorLevel::Ansi16);
    let kit = Kit::new(&sixteen);
    assert_eq!(shade(&kit, Role::CliClaude, 1), kit.fg(Role::CliClaude));
    let none = Theme::dark().with_level(ColorLevel::None);
    let kit = Kit::new(&none);
    assert_eq!(shade(&kit, Role::CliClaude, 2), kit.fg(Role::CliClaude));
    // With 256 colors a shade is the nearest of them to the one full color draws.
    let many = Theme::dark().with_level(ColorLevel::Ansi256);
    let kit = Kit::new(&many);
    let deep = shade(&kit, Role::CliClaude, 1).fg;
    assert_eq!(
        deep,
        Some(Color::Indexed(nearest_ansi256((0xb4, 0x47, 0x2a))))
    );
    assert_ne!(deep, kit.fg(Role::CliClaude).fg);
}

fn tokens(agent: &str, model: Option<&str>, tokens: u64) -> ModelTokens {
    ModelTokens {
        agent: agent.to_owned(),
        model: model.map(str::to_owned),
        tokens,
    }
}

/// Names that know `haiku` as Claude Code's alias for Haiku 5.5, which Claude Code also runs
/// when a record names no model.
fn names() -> DisplayNames {
    let mut haiku = CatalogEntry::new("claude-code/claude-haiku-5-5".parse().unwrap());
    haiku.aliases = Some(vec!["haiku".to_owned()]);
    DisplayNames {
        catalog: ModelCatalog {
            models: vec![haiku],
            ..ModelCatalog::default()
        },
        configured: [("claude-code".to_owned(), "haiku".to_owned())].into(),
        ..DisplayNames::default()
    }
}

#[test]
fn a_model_takes_its_place_by_its_tokens_in_all_among_its_clis_models() {
    let names = DisplayNames::default();
    let ranked = [
        tokens("codex", Some("gpt-6-astra"), 900),
        tokens("claude-code", Some("claude-opus-5"), 300),
        tokens("claude-code", Some("claude-opus-5-5"), 800),
        tokens("codex", Some("gpt-5.6-luna"), 100),
    ];
    let shades = ModelColors::new(&ranked, &names);
    assert_eq!(shades.place("claude-code", Some("claude-opus-5-5")), 0);
    assert_eq!(shades.place("claude-code", Some("claude-opus-5")), 1);
    assert_eq!(shades.place("codex", Some("gpt-6-astra")), 0);
    assert_eq!(shades.place("codex", Some("gpt-5.6-luna")), 1);
}

#[test]
fn a_model_recorded_under_three_names_is_one_model_with_one_color() {
    let names = names();
    let ranked = [
        tokens("claude-code", Some("claude-opus-5-5"), 500),
        tokens("claude-code", Some("haiku"), 300),
        tokens("claude-code", Some("claude-haiku-5-5"), 200),
        tokens("claude-code", None, 100),
    ];
    let shades = ModelColors::new(&ranked, &names);
    // 600 in all under its three names, which is more than Opus 5.5's 500.
    for spelled in [Some("haiku"), Some("claude-haiku-5-5"), None] {
        assert_eq!(shades.place("claude-code", spelled), 0, "{spelled:?}");
    }
    assert_eq!(shades.place("claude-code", Some("claude-opus-5-5")), 1);
}

#[test]
fn a_model_the_list_hasnt_met_comes_after_the_ones_it_has() {
    let names = DisplayNames::default();
    let ranked = [
        tokens("claude-code", Some("claude-opus-5-5"), 800),
        tokens("claude-code", Some("claude-opus-5"), 300),
    ];
    let shown = [
        ("claude-code", Some("claude-sonnet-5-5")),
        ("claude-code", Some("claude-opus-5")),
        ("codex", Some("gpt-5.6")),
    ];
    let shades = ModelColors::new(&ranked, &names).with(shown);
    assert_eq!(shades.place("claude-code", Some("claude-opus-5")), 1);
    assert_eq!(shades.place("claude-code", Some("claude-sonnet-5-5")), 2);
    assert_eq!(shades.place("codex", Some("gpt-5.6")), 0);
    // With no list at all a tab's own order stands, as it did before there was one.
    let alone = ModelColors::new(&[], &names).with(shown);
    assert_eq!(alone.place("claude-code", Some("claude-sonnet-5-5")), 0);
    assert_eq!(alone.place("claude-code", Some("claude-opus-5")), 1);
}

/// The picture of the shades: each model's dot in the Models legend, then four of Claude
/// Code's models as a legend's square and a thin bar, in the colors `model_shades.html`
/// gives by their values.
#[test]
fn the_shades_match_their_picture() {
    let picture = Picture::load("model_shades");
    let theme = golden::theme();
    let kit = Kit::new(&theme);
    let names = DisplayNames::default();
    let models = [
        ("claude-code", "claude-opus-5-5", 100),
        ("claude-code", "claude-opus-5", 37),
        ("claude-code", "claude-opus-4-8", 21),
        ("claude-code", "claude-fable-5-1", 3),
        ("codex", "gpt-6-astra", 90),
        ("codex", "gpt-5.6-luna", 40),
    ];
    let ranked: Vec<ModelTokens> = models
        .iter()
        .map(|(agent, model, sum)| tokens(agent, Some(model), *sum))
        .collect();
    let shades = ModelColors::new(&ranked, &names);

    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new(), Vec::new()];
    for (at, (agent, model, _)) in models.iter().enumerate() {
        if at > 0 {
            rows[0].push(Span::raw("   "));
        }
        rows[0].push(Span::styled("● ", shades.style(&kit, agent, Some(model))));
        rows[0].push(Span::raw(names.model(agent, Some(model)).unwrap()));
    }
    for (agent, model, sum) in &models[..4] {
        let style = shades.style(&kit, agent, Some(model));
        let mut row = draw::swatch(&kit, style);
        row.push(Span::raw(names.model(agent, Some(model)).unwrap()));
        let mut row = fit(row, 14);
        row.extend(draw::thin(&kit, *sum, 100, 40, style));
        rows.push(row);
    }

    let area = Rect::new(0, 0, picture.width() as u16, rows.len() as u16);
    let mut buf = Buffer::empty(area);
    for (y, row) in rows.into_iter().enumerate() {
        buf.set_line(0, y as u16, &Line::from(row), area.width);
    }
    golden::assert_matches(&picture, &buf, &Region::whole(&picture, (0, 0)), &theme);
}

/// The color of the square or dot a row of the screen draws before the model `name`.
fn mark_color(buf: &Buffer, mark: char, name: &str) -> Option<Color> {
    let start = format!("{mark} {name} ");
    for y in 0..buf.area.height {
        let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
        for (at, _) in row.match_indices(&start) {
            // A longer name that starts the same way, as GPT 5.6 Luna does, is another model.
            let after = &row[at + start.len()..];
            if after.starts_with([' ', '(']) {
                let x = row[..at].chars().count() as u16;
                return Some(buf[(x, y)].fg);
            }
        }
    }
    None
}

/// A tab of the panel drawn in full color after `keys`.
fn drawn(mut view: OverviewView, state: &AppState, keys: &[KeyCode]) -> Buffer {
    let theme = golden::theme();
    let ctx = ViewCtx {
        theme: &theme,
        state,
        now: Instant::now(),
    };
    for code in keys {
        view.handle_key(KeyEvent::from(*code), &ctx);
    }
    let area = Rect::new(0, 0, 120, 60);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, &ctx);
    buf
}

/// The color the health card writes the model `name` in, on that model's own row.
fn health_name_color(buf: &Buffer, name: &str) -> Option<Color> {
    for y in 0..buf.area.height {
        let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
        for (at, _) in row.match_indices(name) {
            // Its state follows the name, where a longer name that starts the same goes on.
            if row[at + name.len()..].trim_start().starts_with('▪') {
                let x = row[..at].chars().count() as u16;
                return Some(buf[(x, y)].fg);
            }
        }
    }
    None
}

/// The Status tab's health with Codex's two models, GPT 5.6 first as the model in use.
fn codex_health() -> StatusTabView {
    let model = |name: &str| ModelStanding {
        name: name.to_owned(),
        state: Standing::Healthy,
        why: String::new(),
        covers: None,
        next_check: None,
    };
    let codex = CliHealth {
        id: "codex".to_owned(),
        name: "Codex".to_owned(),
        installed: true,
        main: true,
        models: vec![model("GPT 5.6"), model("GPT 5.6 Luna")],
    };
    StatusTabView {
        health: HealthRows {
            clis: vec![codex],
            ..HealthRows::default()
        },
        ..StatusTabView::default()
    }
}

/// GPT 5.6 Luna has run the most of Codex's models in all, so it draws in Codex's own color
/// and GPT 5.6 in the deep shade, on the Overview's Models tab, on Stats and on the spend
/// card alike, though GPT 5.6 is ahead of it in each one's own numbers, and on the Status
/// tab's health card, which lists GPT 5.6 first.
#[test]
fn a_model_draws_in_one_color_on_every_tab() {
    let ranked = vec![
        tokens("codex", Some("gpt-5.6-luna"), 900),
        tokens("codex", Some("gpt-5.6"), 500),
    ];
    let theme = golden::theme();
    let kit = Kit::new(&theme);
    let own = shade(&kit, Role::CliCodex, 0).fg;
    let deep = shade(&kit, Role::CliCodex, 1).fg;
    assert_ne!(own, deep);

    let mut state = AppState::default();
    state.names = sample::names();
    let mut usage = sample::usage_with_stats();
    usage.all_time_models = ranked.clone();
    let mut luna = usage.by_agent[1].clone();
    luna.model = Some("gpt-5.6-luna".to_owned());
    luna.spent.cost_usd = 0.10;
    usage.by_agent.push(luna);
    state.usage = Some(Arc::new(usage));
    let panel = |tab: Tab| {
        OverviewView::new(tab, UsageWatch::default())
            .at(sample::now())
            .with_period(UsagePeriod::Week)
    };

    // GPT 5.6 Luna moves up beside GPT 5.6, onto the first page of models.
    let mut overview = home::sample::models();
    overview.models.swap(2, 4);
    overview.all_time_models = ranked;
    let mut models = panel(Tab::Overview);
    models.accept(&ViewData::Overview {
        overview: Box::new(overview),
    });
    let models = drawn(models, &state, &[KeyCode::Down, KeyCode::Right]);
    let stats = drawn(panel(Tab::Stats), &state, &[KeyCode::Down, KeyCode::Right]);
    let spend = drawn(panel(Tab::Limits), &state, &[]);
    for (tab, buf, mark) in [
        ("Models", &models, '●'),
        ("Stats, by model", &stats, '■'),
        ("the spend card", &spend, '■'),
    ] {
        assert_eq!(mark_color(buf, mark, "GPT 5.6 Luna"), own, "{tab}");
        assert_eq!(mark_color(buf, mark, "GPT 5.6"), deep, "{tab}");
    }
    let status = panel(Tab::Status).with_status_tab(codex_health());
    let status = drawn(status, &state, &[]);
    assert_eq!(health_name_color(&status, "GPT 5.6 Luna"), own);
    assert_eq!(health_name_color(&status, "GPT 5.6"), deep);
}

/// A panel that holds only the Overview's numbers ranks the health card's models by the list
/// those carry, so the card agrees with the Models tab before any usage numbers have come.
#[test]
fn the_health_card_ranks_by_the_overviews_numbers_when_they_are_all_it_has() {
    let theme = golden::theme();
    let kit = Kit::new(&theme);
    let mut state = AppState::default();
    state.names = sample::names();
    let mut overview = home::sample::models();
    overview.all_time_models = vec![
        tokens("codex", Some("gpt-5.6-luna"), 900),
        tokens("codex", Some("gpt-5.6"), 500),
    ];
    let mut panel = OverviewView::new(Tab::Overview, UsageWatch::default())
        .at(sample::now())
        .with_status_tab(codex_health());
    panel.accept(&ViewData::Overview {
        overview: Box::new(overview),
    });
    let status = drawn(panel, &state, &[KeyCode::Right]);
    assert_eq!(
        health_name_color(&status, "GPT 5.6 Luna"),
        shade(&kit, Role::CliCodex, 0).fg
    );
    assert_eq!(
        health_name_color(&status, "GPT 5.6"),
        shade(&kit, Role::CliCodex, 1).fg
    );
}
