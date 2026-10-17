//! Whether clock times are written in 12 hours or 24, as `ui.format.clock` says: `auto` goes by
//! the terminal's locale, as Claude Code's does.

use argonauts_core::settings::ui::ClockStyle as Setting;

/// Sets the clock every time on the screen is written in.
pub fn set_clock(style: Setting) {
    setting::store(match style {
        Setting::TwelveHour => false,
        Setting::TwentyFourHour => true,
        Setting::Auto => writes_24_hours(&locale()),
    });
}

/// Times are written `16:40` rather than `4:40 PM`.
pub fn twenty_four_hours() -> bool {
    setting::load()
}

/// `16:40` in 24 hours, or `None` for the 12-hour form, which each place writes its own way.
pub fn on_24_hours(hour: u8, minute: u8) -> Option<String> {
    twenty_four_hours().then(|| format!("{hour}:{minute:02}"))
}

/// The locale times are written in: the first of `LC_ALL`, `LC_TIME`, and `LANG` that is set.
fn locale() -> String {
    ["LC_ALL", "LC_TIME", "LANG"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

/// Whether a locale such as `en_GB.UTF-8` writes times in 24 hours. The places that write 12 are
/// listed; no locale, `C`, `POSIX`, and plain `en` are taken as `en_US`.
fn writes_24_hours(locale: &str) -> bool {
    const TWELVE_LANGUAGES: &[&str] = &["ar", "bn", "hi", "ko", "ur"];
    const TWELVE_REGIONS: &[&str] = &[
        "AU", "BD", "CO", "EG", "IN", "KR", "MX", "NZ", "PH", "PK", "SA", "TW", "US",
    ];
    let name = locale.split(['.', '@']).next().unwrap_or_default();
    if matches!(name, "" | "C" | "POSIX") {
        return false;
    }
    let (language, region) = name.split_once(['_', '-']).unwrap_or((name, ""));
    if TWELVE_LANGUAGES.contains(&language) {
        return false;
    }
    match region {
        "" => language != "en",
        // English Canada writes 12 hours, French Canada 24.
        "CA" => language != "en",
        region => !TWELVE_REGIONS.contains(&region),
    }
}

/// One value for the whole process, so every time on screen agrees.
#[cfg(not(test))]
mod setting {
    use std::sync::atomic::{AtomicBool, Ordering};

    static TWENTY_FOUR: AtomicBool = AtomicBool::new(false);

    pub fn store(on: bool) {
        TWENTY_FOUR.store(on, Ordering::Relaxed);
    }

    pub fn load() -> bool {
        TWENTY_FOUR.load(Ordering::Relaxed)
    }
}

/// Unit tests run in parallel threads, so each keeps its own clock and one that sets 24 hours
/// can't change another's times.
#[cfg(test)]
mod setting {
    use std::cell::Cell;

    thread_local! {
        static TWENTY_FOUR: Cell<bool> = const { Cell::new(false) };
    }

    pub fn store(on: bool) {
        TWENTY_FOUR.with(|cell| cell.set(on));
    }

    pub fn load() -> bool {
        TWENTY_FOUR.with(Cell::get)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_goes_by_where_the_locale_is() {
        for twelve in [
            "",
            "C",
            "POSIX",
            "en",
            "en_US.UTF-8",
            "en_CA",
            "ko_KR.UTF-8",
            "es_MX",
        ] {
            assert!(!writes_24_hours(twelve), "{twelve}");
        }
        for twenty_four in [
            "en_GB.UTF-8",
            "de_DE",
            "fr_CA",
            "ja_JP.UTF-8",
            "pt-BR",
            "de",
        ] {
            assert!(writes_24_hours(twenty_four), "{twenty_four}");
        }
    }

    #[test]
    fn the_setting_picks_the_clock() {
        let time = super::super::ClockTime::new(16, 5).unwrap();
        set_clock(Setting::TwentyFourHour);
        assert_eq!(on_24_hours(16, 5).as_deref(), Some("16:05"));
        assert_eq!(time.to_string(), "16:05");
        assert_eq!(
            super::super::ClockStyle::now(),
            super::super::ClockStyle::TwentyFour
        );
        set_clock(Setting::TwelveHour);
        assert_eq!(on_24_hours(16, 5), None);
        assert_eq!(time.to_string(), "4:05 PM");
    }
}
