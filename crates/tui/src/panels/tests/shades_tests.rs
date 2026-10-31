use std::sync::Arc;
use std::time::Instant;

use argonauts_core::app::ViewData;
use argonauts_core::app::status_tab::{
    CliHealth, HealthRows, ModelStanding, Standing, StatusTabView,
};
use argonauts_core::models::model_catalog::{CatalogEntry, ModelCatalog};
use argonauts_core::runs::api::UsagePeriod;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use ratatui::style::{Color, Style};

use crate::base::kit::Kit;
use crate::base::kit::fit;
use crate::base::theme::Role;
use crate::base::theme::shades::*;
use crate::base::theme::{Brightness, ColorLevel, Glyphs, Theme};
use crate::frame::state::AppState;
use crate::frame::view::{View, ViewCtx};
use crate::testing::pictures::golden::{self, Picture, Region};
use crate::{
    panels::{OverviewView, Tab, UsageWatch, home},
    testing::sample,
};
use argonauts_core::app::overview::ModelTokens;
use argonauts_core::look::display_names::DisplayNames;

/// The four CLIs' hues.
const CLIS: [Role; 4] = [
    Role::CliClaude,
    Role::CliCodex,
    Role::CliAgy,
    Role::CliOpenCode,
];

fn full_color_theme(brightness: Brightness) -> Theme {
    Theme::for_brightness(brightness).with_level(ColorLevel::TrueColor)
}

#[test]
fn a_clis_first_three_models_take_its_hue_and_the_rest_its_grey() {
    for brightness in [Brightness::Dark, Brightness::Light] {
        let theme = full_color_theme(brightness);
        let kit = Kit::new(&theme);
        for cli in CLIS {
            for place in 0..OWN_TEXTURES {
                assert_eq!(shade(&kit, cli, place), kit.fg(cli), "{cli:?} {place}");
            }
            for place in OWN_TEXTURES..OWN_TEXTURES + 3 {
                assert_eq!(shade(&kit, cli, place), kit.fg(Role::CatOther), "{cli:?}");
            }
        }
    }
}

/// A CLI's first three models are █ ▚ ⣿, shapes Ghostty draws itself at full strength, and
/// the rest one solid cell, which their grey tells from the first's hue.
#[test]
fn each_place_takes_its_shape_and_the_rest_share_a_solid_cell() {
    let theme = Theme::dark();
    let kit = Kit::new(&theme);
    let textures: Vec<&str> = (0..6).map(|place| texture(&kit, place)).collect();
    assert_eq!(textures, ["█", "▚", "⣿", "█", "█", "█"]);
    let mut ascii = Theme::dark();
    ascii.glyphs = Glyphs::ASCII;
    let kit = Kit::new(&ascii);
    let textures: Vec<&str> = (0..5).map(|place| texture(&kit, place)).collect();
    assert_eq!(textures, ["#", "=", "+", ":", ":"]);
}

/// None of the shapes is one Ghostty paints as a flat mix of its colour into the background.
#[test]
fn no_model_draws_in_a_shade_glyph() {
    for level in [
        ColorLevel::TrueColor,
        ColorLevel::Ansi256,
        ColorLevel::Ansi16,
    ] {
        let theme = Theme::dark().with_level(level);
        let kit = Kit::new(&theme);
        for place in 0..OWN_TEXTURES + 2 {
            let shape = texture(&kit, place);
            assert!(
                !["▓", "▒", "░"].contains(&shape),
                "{level:?} {place}: {shape}"
            );
        }
    }
}

/// A bar that ends in half a cell ends in the left half of its shape.
#[test]
fn half_a_cell_is_the_left_half_of_the_shape() {
    let theme = Theme::dark();
    let kit = Kit::new(&theme);
    let halves: Vec<Option<&str>> = (0..5).map(|place| half_texture(&kit, place)).collect();
    assert_eq!(
        halves,
        [Some("▌"), Some("▘"), Some("⡇"), Some("▌"), Some("▌")]
    );
    let mut ascii = Theme::dark();
    ascii.glyphs = Glyphs::ASCII;
    let kit = Kit::new(&ascii);
    assert!((0..5).all(|place| half_texture(&kit, place).is_none()));
}

/// A hue of your own is the one all three of the CLI's first models take.
#[test]
fn a_color_you_set_is_the_hue_of_the_clis_models() {
    let mut theme = full_color_theme(Brightness::Dark);
    let yours = Style::new().fg(Color::Rgb(0x80, 0xc0, 0x40));
    theme.set(Role::CliClaude, yours);
    let kit = Kit::new(&theme);
    for place in 0..OWN_TEXTURES {
        assert_eq!(shade(&kit, Role::CliClaude, place), yours);
    }
}

/// Textures tell the models apart however few colors the terminal shows, and in sixteen
/// colors Claude Code's models draw in the terminal's own text.
#[test]
fn textures_carry_the_models_where_colors_are_few() {
    for level in [ColorLevel::Ansi16, ColorLevel::None] {
        let theme = Theme::dark().with_level(level);
        let kit = Kit::new(&theme);
        assert_eq!(shade(&kit, Role::CliClaude, 1).fg, None, "{level:?}");
        assert_ne!(texture(&kit, 0), texture(&kit, 1));
        assert_ne!(texture(&kit, 1), texture(&kit, 2));
    }
    // Without colour the rest's grey can't tell it from a first model's solid cell.
    let plain = Theme::dark().with_level(ColorLevel::None);
    let kit = Kit::new(&plain);
    assert_eq!(texture(&kit, OWN_TEXTURES), "░");
    assert_eq!(half_texture(&kit, OWN_TEXTURES), None);
    let many = Theme::dark().with_level(ColorLevel::Ansi256);
    let kit = Kit::new(&many);
    assert_eq!(shade(&kit, Role::CliCodex, 2).fg, Some(Color::Indexed(75)));
    assert_eq!(shade(&kit, Role::CliCodex, 3).fg, Some(Color::Indexed(103)));
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
fn a_model_takes_its_place_among_its_clis_models_by_what_ran_here() {
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

/// The app's list breaks ties already, by the whole computer's count, and puts the models
/// that never ran here after the rest: models with as many tokens keep its order.
#[test]
fn models_with_as_many_tokens_keep_the_lists_order() {
    let names = DisplayNames::default();
    let ranked = [
        tokens("claude-code", Some("claude-haiku-5-5"), 6_500),
        tokens("claude-code", Some("claude-opus-5"), 120),
        tokens("claude-code", Some("claude-haiku-4-5"), 120),
        tokens("claude-code", Some("claude-opus-4-8"), 0),
        tokens("claude-code", Some("claude-fable-5-1"), 0),
    ];
    let shades = ModelColors::new(&ranked, &names);
    let places: Vec<usize> = ranked
        .iter()
        .map(|model| shades.place(&model.agent, model.model.as_deref()))
        .collect();
    assert_eq!(places, [0, 1, 2, 3, 4]);
}

/// A screen's colours come from the list, then the models it shows that the list lacks, each
/// after its CLI's others, so the screen draws no two of a CLI's models alike.
#[test]
fn a_screens_colours_take_the_list_then_its_own_models() {
    let names = DisplayNames::default();
    let ranked = [
        tokens("codex", Some("gpt-6-luna"), 1_470),
        tokens("codex", Some("gpt-6.1-sol"), 630),
    ];
    let shown = [
        ("codex", Some("gpt-6-astra")),
        ("codex", Some("gpt-6.1-sol")),
        ("codex", Some("gpt-5.6-terra")),
    ];
    let shades = ModelColors::for_screen(&ranked, &names, shown);
    let theme = full_color_theme(Brightness::Dark);
    let kit = Kit::new(&theme);
    let looks: Vec<(&str, Style)> = ["gpt-6-luna", "gpt-6.1-sol", "gpt-6-astra", "gpt-5.6-terra"]
        .into_iter()
        .map(|model| {
            (
                shades.texture(&kit, "codex", Some(model)),
                shades.style(&kit, "codex", Some(model)),
            )
        })
        .collect();
    let (hue, grey) = (kit.fg(Role::CliCodex), kit.fg(Role::CatOther));
    assert_eq!(looks, [("█", hue), ("▚", hue), ("⣿", hue), ("█", grey)]);
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

/// The picture of the textures: each model's swatch as a legend writes it, then the tokens
/// bar of Claude Code's four models and Codex's two, a cell apart, and each of Claude Code's
/// models on a bar of its own, in the colors `model_textures.html` gives by their values.
#[test]
fn the_textures_match_their_picture() {
    let picture = Picture::load("model_textures");
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
    let paint = |agent: &str, model: &str, cells: usize| {
        let texture = shades.texture(&kit, agent, Some(model));
        Span::styled(
            texture.repeat(cells),
            shades.style(&kit, agent, Some(model)),
        )
    };

    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new(), Vec::new(), Vec::new()];
    for (at, (agent, model, sum)) in models.iter().enumerate() {
        if at > 0 {
            rows[0].push(Span::raw("   "));
        }
        rows[0].push(paint(agent, model, 2));
        rows[0].push(Span::raw(format!(
            " {}",
            names.model(agent, Some(model)).unwrap()
        )));
        if at == 4 {
            rows[2].push(Span::raw(" "));
        }
        rows[2].push(paint(agent, model, (*sum as usize).div_ceil(8)));
    }
    rows.push(Vec::new());
    for (agent, model, sum) in &models[..4] {
        let mut row = vec![paint(agent, model, 2)];
        row.push(Span::raw(format!(
            " {}",
            names.model(agent, Some(model)).unwrap()
        )));
        let mut row = fit(row, 15);
        row.push(paint(agent, model, (*sum as usize * 40).div_ceil(100)));
        rows.push(row);
    }

    let area = Rect::new(0, 0, picture.width() as u16, rows.len() as u16);
    let mut buf = Buffer::empty(area);
    for (y, row) in rows.into_iter().enumerate() {
        buf.set_line(0, y as u16, &Line::from(row), area.width);
    }
    golden::assert_matches(&picture, &buf, &Region::whole(&picture, (0, 0)), &theme);
}

/// The mark a row of the screen draws before the model `name`, a square, a dot or a texture,
/// and its color.
fn mark_before(buf: &Buffer, name: &str) -> Option<(String, Color)> {
    let start = format!(" {name} ");
    for y in 0..buf.area.height {
        let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
        for (at, _) in row.match_indices(&start) {
            // A longer name that starts the same way, as GPT 5.6 Luna does, is another model.
            let after = &row[at + start.len()..];
            // A table's swatch stands two blanks before the name, a legend's mark one.
            let before = row[..at].chars().count();
            let Some(x) = (1..=2)
                .filter_map(|back| before.checked_sub(back))
                .find(|x| buf[(*x as u16, y)].symbol() != " ")
            else {
                continue;
            };
            let mark = buf[(x as u16, y)].symbol();
            if after.starts_with([' ', '(']) && mark != " " {
                return Some((mark.to_owned(), buf[(x as u16, y)].fg));
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

/// Codex's models in all: GPT 5.6 Luna has run the most, and GPT 5.6 is the fourth.
fn codex_ranked() -> Vec<ModelTokens> {
    vec![
        tokens("codex", Some("gpt-5.6-luna"), 900),
        tokens("codex", Some("gpt-6-astra"), 800),
        tokens("codex", Some("gpt-6-sol"), 700),
        tokens("codex", Some("gpt-5.6"), 500),
    ]
}

/// GPT 5.6 Luna draws in Codex's hue and GPT 5.6, its fourth model in all, in the grey of the
/// rest, on the Overview's Models tab, on Stats and on the spend card alike, though GPT 5.6 is
/// ahead of it in each one's own numbers, and on the Status tab's health card, which lists GPT
/// 5.6 first.
#[test]
fn a_model_draws_in_one_color_on_every_tab() {
    let ranked = codex_ranked();
    let theme = golden::theme();
    let kit = Kit::new(&theme);
    let own = shade(&kit, Role::CliCodex, 0).fg;
    let rest = shade(&kit, Role::CliCodex, OWN_TEXTURES).fg;
    assert_ne!(own, rest);

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
        OverviewView::new(
            tab,
            UsageWatch::default(),
            crate::panels::view::NoConfig::boxed(),
        )
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
    // Each draws both solid, the first in Codex's hue and the rest's after the third in grey.
    for (tab, buf) in [
        ("Models", &models),
        ("Stats, by model", &stats),
        ("the spend card", &spend),
    ] {
        let mark = |name| mark_before(buf, name).map(|(mark, color)| (mark, Some(color)));
        assert_eq!(mark("GPT 5.6 Luna"), Some(("█".to_owned(), own)), "{tab}");
        assert_eq!(mark("GPT 5.6"), Some(("█".to_owned(), rest)), "{tab}");
    }
    let status = panel(Tab::Status).with_status_tab(codex_health());
    let status = drawn(status, &state, &[]);
    assert_eq!(health_name_color(&status, "GPT 5.6 Luna"), own);
    assert_eq!(health_name_color(&status, "GPT 5.6"), rest);
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
    overview.all_time_models = codex_ranked();
    let mut panel = OverviewView::new(
        Tab::Overview,
        UsageWatch::default(),
        crate::panels::view::NoConfig::boxed(),
    )
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
        shade(&kit, Role::CliCodex, OWN_TEXTURES).fg
    );
}
