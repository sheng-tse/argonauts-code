//! `[ui]` settings take effect the moment they arrive, with no restart.

use argonauts_core::KeyScheme;
use argonauts_core::app::AppEvent;
use argonauts_core::settings::ui::UiSettings;
use argonauts_core::settings::ui::{ColorChoice, Motion};

use crate::text::set_ambiguous_width;
use crate::theme::{Brightness, ColorEnv, ColorLevel, Glyphs, Theme};

use super::Cx;
use super::status::{apply_keys, verb_style};

/// Applies `AppEvent::UiSettings`: theme, accent, color, motion, glyphs, ambiguous width, verbs,
/// keys, the live area's share, and the panel settings. The bridge keeps the section itself for the
/// parts that read it as they draw, such as `turn_duration` and `bell`.
pub fn on_event(event: &AppEvent, cx: &mut Cx<'_>) {
    let AppEvent::UiSettings { vim, ui, .. } = event else {
        return;
    };
    let Some(ui) = ui else {
        // An older controller sends only the key scheme and the verbosity.
        cx.out.vim = Some(*vim);
        let keys = if *vim {
            KeyScheme::Vim
        } else {
            KeyScheme::Default
        };
        apply_keys(&mut cx.model.bottom.mode, keys);
        return;
    };
    let terminal = &cx.state.terminal;
    let env = terminal
        .color_env
        .clone()
        .unwrap_or_else(ColorEnv::from_process);
    set_ambiguous_width(ui.ambiguous_width);
    cx.out.theme = Some(theme_for(ui, &env, terminal.background_is_light));
    cx.model.verbs = verb_style(ui.verbs);
    let bottom = &mut cx.model.bottom;
    apply_keys(&mut bottom.mode, ui.keys);
    bottom.live_area_max = Some(ui.live_area_max);
    bottom.workflows.max_rows = usize::try_from(ui.panel.max_rows).unwrap_or(usize::MAX);
    bottom.workflows.show_agents = ui.panel.agents;
}

/// The palette for these settings. `background_is_light` is the terminal's answer to OSC 11,
/// when it gave one in time, for `ui.theme = "auto"`.
pub fn theme_for(ui: &UiSettings, env: &ColorEnv, background_is_light: Option<bool>) -> Theme {
    // The daltonized, ANSI, and custom palettes aren't drawn yet, so each draws as its base:
    // a custom theme as auto until its file is read.
    let light = ui.theme.is_light().or(background_is_light);
    let brightness = match light {
        Some(true) => Brightness::Light,
        Some(false) => Brightness::Dark,
        None => Brightness::detect(env).unwrap_or_default(),
    };
    let mut theme = Theme::for_brightness(brightness);
    if let Some(accent) = ui.accent.as_deref().and_then(parse_hex) {
        theme = theme.with_accent(accent);
    }
    let mut theme = theme.with_level(color_level(ui.color, env));
    // Your own colors go on last, fitted to the level. One that can't be used leaves the
    // theme's own in place.
    theme.apply_colors(
        ui.colors
            .iter()
            .map(|(role, color)| (role.as_str(), color.as_str())),
    );
    theme.reduced_motion = ui.motion == Motion::Reduced;
    theme.glyphs = Glyphs::for_settings(ui.glyphs, ui.ambiguous_width);
    theme
}

/// `NO_COLOR` and `TERM=dumb` turn color off unless `ui.color = "always"` says otherwise, as
/// the NO_COLOR standard allows a setting to.
fn color_level(choice: ColorChoice, env: &ColorEnv) -> ColorLevel {
    match choice {
        ColorChoice::Never => ColorLevel::None,
        ColorChoice::Auto => ColorLevel::detect(env),
        ColorChoice::Always => {
            let env = ColorEnv {
                no_color: false,
                ..env.clone()
            };
            match ColorLevel::detect(&env) {
                ColorLevel::None => ColorLevel::Ansi16,
                level => level,
            }
        }
    }
}

/// `#rrggbb`, the one form `ui.accent` takes.
fn parse_hex(text: &str) -> Option<(u8, u8, u8)> {
    let hex = text.trim().strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
    Some((channel(0)?, channel(2)?, channel(4)?))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::Verbosity;
    use argonauts_core::settings::ui::Glyphs as GlyphSet;
    use argonauts_core::settings::ui::Theme as ThemeChoice;
    use ratatui::style::Color;

    use super::*;
    use crate::bridge::Bridge;
    use crate::model::ViewModel;
    use crate::options::UiOptions;
    use crate::text::{ambiguous_width, str_width};
    use crate::theme::Role;

    fn env(term: &str) -> ColorEnv {
        ColorEnv {
            no_color: false,
            colorterm: Some("truecolor".into()),
            term: Some(term.into()),
            colorfgbg: None,
        }
    }

    #[test]
    fn your_own_colors_go_over_the_theme_s_and_a_bad_one_changes_nothing() {
        use argonauts_core::settings::ui::ColorText;
        let terminal = env("xterm-256color");
        let plain = theme_for(&UiSettings::default(), &terminal, Some(false));
        let mut ui = UiSettings::default();
        ui.colors
            .insert("agent-orange".to_owned(), ColorText::from("agent-pink"));
        ui.colors
            .insert("bar-fill".to_owned(), ColorText::from("#123456"));
        ui.colors
            .insert("no-such-role".to_owned(), ColorText::from("#123456"));
        let theme = theme_for(&ui, &terminal, Some(false));
        assert_eq!(theme.linked_to(Role::AgentOrange), Some(Role::AgentPink));
        assert_eq!(theme.style(Role::AgentOrange), plain.style(Role::AgentPink));
        assert_ne!(theme.style(Role::BarFill), plain.style(Role::BarFill));
        assert_eq!(theme.style(Role::Accent), plain.style(Role::Accent));
    }

    #[test]
    fn auto_follows_the_terminal_background() {
        let ui = UiSettings::default();
        let terminal = env("xterm-256color");
        assert_eq!(
            theme_for(&ui, &terminal, Some(true)).brightness,
            Brightness::Light
        );
        assert_eq!(
            theme_for(&ui, &terminal, Some(false)).brightness,
            Brightness::Dark
        );
        let light_fgbg = ColorEnv {
            colorfgbg: Some("0;15".into()),
            ..terminal.clone()
        };
        assert_eq!(
            theme_for(&ui, &light_fgbg, None).brightness,
            Brightness::Light
        );
        let fixed = UiSettings {
            theme: ThemeChoice::Dark,
            ..UiSettings::default()
        };
        assert_eq!(
            theme_for(&fixed, &terminal, Some(true)).brightness,
            Brightness::Dark
        );
        let light_ansi = UiSettings {
            theme: ThemeChoice::LightAnsi,
            ..UiSettings::default()
        };
        assert_eq!(
            theme_for(&light_ansi, &terminal, Some(false)).brightness,
            Brightness::Light
        );
        let custom = UiSettings {
            theme: ThemeChoice::Custom("harbor".into()),
            ..UiSettings::default()
        };
        assert_eq!(
            theme_for(&custom, &terminal, Some(true)).brightness,
            Brightness::Light
        );
    }

    #[test]
    fn no_color_and_dumb_terminals_lose_color_unless_always() {
        let ui = UiSettings::default();
        let no_color = ColorEnv {
            no_color: true,
            ..env("xterm-256color")
        };
        assert_eq!(theme_for(&ui, &no_color, None).level, ColorLevel::None);
        assert_eq!(theme_for(&ui, &env("dumb"), None).level, ColorLevel::None);
        let always = UiSettings {
            color: ColorChoice::Always,
            ..UiSettings::default()
        };
        assert_eq!(
            theme_for(&always, &no_color, None).level,
            ColorLevel::TrueColor
        );
        assert_eq!(
            theme_for(&always, &env("dumb"), None).level,
            ColorLevel::Ansi16
        );
        let never = UiSettings {
            color: ColorChoice::Never,
            ..UiSettings::default()
        };
        assert_eq!(
            theme_for(&never, &env("xterm-256color"), None).level,
            ColorLevel::None
        );
    }

    #[test]
    fn accent_motion_and_glyphs_apply() {
        let ui = UiSettings {
            accent: Some("#d4a017".into()),
            motion: Motion::Reduced,
            glyphs: GlyphSet::Ascii,
            ..UiSettings::default()
        };
        let theme = theme_for(&ui, &env("xterm-256color"), Some(false));
        assert_eq!(
            theme.style(Role::Accent).fg,
            Some(Color::Rgb(0xd4, 0xa0, 0x17))
        );
        assert!(theme.reduced_motion);
        assert_eq!(theme.glyphs, Glyphs::ASCII);
        let bad = UiSettings {
            accent: Some("gold".into()),
            ..UiSettings::default()
        };
        let default = theme_for(&UiSettings::default(), &env("xterm-256color"), Some(false));
        assert_eq!(
            theme_for(&bad, &env("xterm-256color"), Some(false)).style(Role::Accent),
            default.style(Role::Accent)
        );
    }

    #[test]
    fn two_cell_ambiguous_width_takes_the_ascii_set_and_wide_measures() {
        let wide = UiSettings {
            ambiguous_width: 2,
            ..UiSettings::default()
        };
        let mut bridge = Bridge::new(&UiOptions::default());
        let mut model = ViewModel::default();
        let event = AppEvent::UiSettings {
            vim: false,
            verbosity: Verbosity::Full,
            ui: Some(Box::new(wide)),
        };
        let out = bridge.on_event(&mut model, &event, Instant::now());
        let theme = out.theme.expect("the settings give a theme");
        assert_eq!(theme.glyphs, Glyphs::ASCII);
        assert_eq!(ambiguous_width(), 2);
        assert_eq!(str_width("● done"), 7);

        let narrow = AppEvent::UiSettings {
            vim: false,
            verbosity: Verbosity::Full,
            ui: Some(Box::new(UiSettings::default())),
        };
        let out = bridge.on_event(&mut model, &narrow, Instant::now());
        assert_eq!(out.theme.map(|theme| theme.glyphs), Some(Glyphs::UNICODE));
        assert_eq!(str_width("● done"), 6);
    }
}
