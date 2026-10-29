//! `/mods`, `/pin`, `/unpin`, and the `print` and `pin` words of `/status`, `/usage`,
//! `/context`, and `/limits`.

use serde::{Deserialize, Serialize};

use super::{InputError, SlashCommand, bad, key_and_value};
use crate::settings::SettingScope;
use crate::settings::mods::ModPlace;
use crate::settings::stats::StatsPeriod;
use crate::settings::ui::{ModName, ModShape, Place, UiSettings, Units};

/// What `/status`, `/usage`, `/context`, and `/limits` do with their tab of the usage panel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum ViewAction {
    /// Opens the tab, as `ui.views.<view>.open` says.
    #[default]
    Open,
    /// Prints the tab's cards into the transcript once.
    Print,
    /// Pins the tab's first mod above the input, or unpins it when it's there.
    Pin {
        /// Where the change is saved; unset is the layer the list came from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<SettingScope>,
    },
}

impl ViewAction {
    /// Left out of the wire form, so what older outside agents send reads the same.
    pub fn is_open(&self) -> bool {
        *self == ViewAction::Open
    }
}

/// What `/mods` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum ModsAction {
    /// Every mod in every place.
    List,
    /// One mod's page.
    Page { the_mod: ModName },
    /// Shows or hides the whole band above the input.
    Band { shown: bool },
    Change {
        the_mod: ModName,
        change: ModPlaceChange,
    },
}

/// One change to one mod.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum ModPlaceChange {
    /// Shows it in `place`, or takes it out when it's there. Above the input, that's a pin.
    Toggle {
        place: Place,
    },
    /// Shows it in `place`, as `/pin` does above the input.
    Show {
        place: Place,
    },
    /// Takes it out of `place`, as `/unpin` does above the input.
    Hide {
        place: Place,
    },
    /// Folds its card to its line.
    Fold,
    Open,
    /// One step toward the start of its list in `place`. Unset is the first place that lists
    /// it, above the input first.
    Up {
        place: Option<Place>,
    },
    Down {
        place: Option<Place>,
    },
    Form {
        form: ModShape,
    },
    Units {
        units: Units,
    },
}

impl ModPlaceChange {
    /// The one setting this change writes, with its new value as TOML text, from `ui` as it
    /// is now. `None` when it would change nothing, such as pinning what's already pinned.
    pub fn setting(self, the_mod: ModName, ui: &UiSettings) -> Option<(String, String)> {
        let list = |place: Place, edit: &dyn Fn(&mut Vec<ModName>) -> bool| {
            let mut mods = ui.place(place).to_vec();
            edit(&mut mods).then(|| (place.key().to_owned(), names(&mods)))
        };
        let own = |field: &str, value: String| (format!("ui.mods.{the_mod}.{field}"), value);
        let first_place = |place: Option<Place>| {
            place.or_else(|| {
                Place::ALL
                    .into_iter()
                    .find(|place| ui.place(*place).contains(&the_mod))
            })
        };
        let look = ui.the_mod(the_mod);
        match self {
            ModPlaceChange::Toggle { place } => list(place, &|mods| {
                match mods.iter().position(|shown| *shown == the_mod) {
                    Some(at) => {
                        mods.remove(at);
                    }
                    None => mods.push(the_mod),
                }
                true
            }),
            ModPlaceChange::Show { place } => list(place, &|mods| {
                let missing = !mods.contains(&the_mod);
                if missing {
                    mods.push(the_mod);
                }
                missing
            }),
            ModPlaceChange::Hide { place } => list(place, &|mods| {
                let before = mods.len();
                mods.retain(|shown| *shown != the_mod);
                mods.len() != before
            }),
            ModPlaceChange::Fold => look.open.then(|| own("open", "false".to_owned())),
            ModPlaceChange::Open => (!look.open).then(|| own("open", "true".to_owned())),
            ModPlaceChange::Up { place } => list(first_place(place)?, &|mods| match mods
                .iter()
                .position(|shown| *shown == the_mod)
            {
                Some(at) if at > 0 => {
                    mods.swap(at - 1, at);
                    true
                }
                _ => false,
            }),
            ModPlaceChange::Down { place } => list(first_place(place)?, &|mods| match mods
                .iter()
                .position(|shown| *shown == the_mod)
            {
                Some(at) if at + 1 < mods.len() => {
                    mods.swap(at, at + 1);
                    true
                }
                _ => false,
            }),
            ModPlaceChange::Form { form } => {
                (look.form != form).then(|| own("form", quoted(form.word())))
            }
            ModPlaceChange::Units { units } => {
                (look.units != Some(units)).then(|| own("units", quoted(units.word())))
            }
        }
    }
}

fn quoted(word: &str) -> String {
    toml::Value::String(word.to_owned()).to_string()
}

fn names(mods: &[ModName]) -> String {
    let words = mods
        .iter()
        .map(|the_mod| toml::Value::String(the_mod.name().to_owned()))
        .collect();
    toml::Value::Array(words).to_string()
}

/// The flags every command that saves a change takes, [`super::SCOPE_FLAGS`], in words.
const SCOPE_FLAGS: &str = "--session, --local, --project, or --global";

/// Takes `--session`, `--local`, `--project`, or `--global` out of `words`.
fn take_scope(name: &str, words: &mut Vec<&str>) -> Result<Option<SettingScope>, InputError> {
    let mut scope = None;
    let mut rest = Vec::with_capacity(words.len());
    for word in words.drain(..) {
        if !word.starts_with("--") {
            rest.push(word);
            continue;
        }
        let picked = super::scope_of_flag(word)
            .ok_or_else(|| bad(name, format!("{word} isn't a flag; use {SCOPE_FLAGS}")))?;
        if scope.replace(picked).is_some() {
            return Err(bad(name, format!("pick one of {SCOPE_FLAGS}")));
        }
    }
    *words = rest;
    Ok(scope)
}

/// Takes `print` or `pin`, and the flag that goes with pin, out of `words`.
fn view_action(name: &str, words: &mut Vec<&str>) -> Result<ViewAction, InputError> {
    let scope = take_scope(name, words)?;
    let is_action = |word: &&str| *word == "print" || *word == "pin";
    let action = match words.iter().position(is_action) {
        None => ViewAction::Open,
        Some(at) => match words.remove(at) {
            "print" => ViewAction::Print,
            _ => ViewAction::Pin { scope },
        },
    };
    if words.iter().any(is_action) {
        return Err(bad(name, "use print or pin, not both"));
    }
    if scope.is_some() && !matches!(action, ViewAction::Pin { .. }) {
        return Err(bad(name, format!("{SCOPE_FLAGS} go with pin")));
    }
    Ok(action)
}

fn words(args: &str) -> Vec<&str> {
    args.split_whitespace().collect()
}

pub(super) fn parse_status(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let mut words = words(args);
    let action = view_action(name, &mut words)?;
    if !words.is_empty() {
        return Err(bad(name, "use /status [print|pin]"));
    }
    Ok(SlashCommand::Status { action })
}

pub(super) fn parse_context(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let mut words = words(args);
    let action = view_action(name, &mut words)?;
    let all = words.first() == Some(&"all");
    if words.len() > usize::from(all) {
        return Err(bad(name, "use /context [all] [print|pin]"));
    }
    Ok(SlashCommand::Context { all, action })
}

/// The period stays free, since a CLI's own name for /usage, such as `/cost weekly`, runs it.
pub(super) fn parse_usage(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let mut words = words(args);
    let action = view_action(name, &mut words)?;
    let period = match words.as_slice() {
        [] => None,
        [period] => Some((*period).to_owned()),
        _ => return Err(bad(name, "use /usage [day|week|month|all] [print|pin]")),
    };
    Ok(SlashCommand::Usage { period, action })
}

/// `/stats`, `/stats week` or `/stats pin`. It has no printed form: stats are drawn.
pub(super) fn parse_stats(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    const USAGE: &str = "use /stats [today|week|month|all] [pin]";
    let mut words = words(args);
    let action = view_action(name, &mut words)?;
    let period = match words.as_slice() {
        [] => None,
        [period] if StatsPeriod::parse(period).is_some() => Some((*period).to_owned()),
        _ => return Err(bad(name, USAGE)),
    };
    if action == ViewAction::Print {
        return Err(bad(name, USAGE));
    }
    Ok(SlashCommand::Stats { period, action })
}

/// `/limits print`, `/limits pin`, or `/limits <key> <value>`, which sets one key.
pub(super) fn parse_limits(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    const USAGE: &str = "use /limits [print|pin], or /limits <key> <value>";
    let mut words = words(args);
    let action = view_action(name, &mut words)?;
    if action != ViewAction::Open {
        if !words.is_empty() {
            return Err(bad(name, USAGE));
        }
        return Ok(SlashCommand::Limits {
            key: None,
            value: None,
            action,
        });
    }
    let (key, value) = key_and_value(args);
    if key.is_some() && value.is_none() {
        return Err(bad(name, USAGE));
    }
    Ok(SlashCommand::Limits { key, value, action })
}

/// A name the closed list of cards doesn't have, which may be a mod's file. The app knows the
/// files, so it answers for a name that is none.
fn file_mod(word: &str) -> Option<String> {
    (ModName::from_name(word).is_none() && crate::settings::mods::is_mod_name(word))
        .then(|| word.to_owned())
}

pub(super) fn parse_mods(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let mut words = words(args);
    let scope = take_scope(name, &mut words)?;
    // The bottom line reads its own mod file, so a built-in card goes onto it the way a mod of
    // yours does, by the mod's name.
    if let [first, place] = words.as_slice()
        && scope.is_none()
        && ModPlace::from_word(place) == Some(ModPlace::BottomLine)
        && let Some(card) = ModName::from_name(first)
        && let Some(mod_name) = crate::settings::mods::builtin::mod_of_card(card)
    {
        return Ok(SlashCommand::ModFile {
            name: mod_name.to_owned(),
            words: vec![(*place).to_owned()],
        });
    }
    if let [first, rest @ ..] = words.as_slice()
        && !matches!(*first, "hide" | "show")
        && let Some(mod_name) = file_mod(first)
    {
        if scope.is_some() {
            return Err(bad(
                name,
                format!("{mod_name} is a file, which its page saves; {SCOPE_FLAGS} don't apply"),
            ));
        }
        return Ok(SlashCommand::ModFile {
            name: mod_name,
            words: rest.iter().map(|word| (*word).to_owned()).collect(),
        });
    }
    let action = match words.as_slice() {
        [] => ModsAction::List,
        ["hide"] => ModsAction::Band { shown: false },
        ["show"] => ModsAction::Band { shown: true },
        [the_mod] => ModsAction::Page {
            the_mod: mod_named(name, the_mod)?,
        },
        [the_mod, change @ ..] => {
            let the_mod = mod_named(name, the_mod)?;
            ModsAction::Change {
                the_mod,
                change: mod_change(name, the_mod, change)?,
            }
        }
    };
    if scope.is_some() && matches!(action, ModsAction::List | ModsAction::Page { .. }) {
        return Err(bad(name, format!("{SCOPE_FLAGS} go with a change")));
    }
    Ok(SlashCommand::Mods { action, scope })
}

/// `/pin <mod>` shows it above the input; `/unpin <mod>` takes it away.
pub(super) fn parse_pin(name: &str, args: &str, pin: bool) -> Result<SlashCommand, InputError> {
    let mut words = words(args);
    let scope = take_scope(name, &mut words)?;
    if let [word] = words.as_slice()
        && scope.is_none()
        && let Some(mod_name) = file_mod(word)
    {
        return Ok(SlashCommand::ModFile {
            name: mod_name,
            words: vec![name.to_owned()],
        });
    }
    let the_mod = match words.as_slice() {
        [the_mod] => mod_named(name, the_mod)?,
        [] => {
            return Err(bad(name, format!("needs a mod, such as /{name} context")));
        }
        _ => return Err(bad(name, format!("use /{name} <mod>"))),
    };
    let place = Place::AboveInput;
    let change = if pin {
        fits(name, the_mod, place)?;
        ModPlaceChange::Show { place }
    } else {
        ModPlaceChange::Hide { place }
    };
    Ok(SlashCommand::Mods {
        action: ModsAction::Change { the_mod, change },
        scope,
    })
}

fn mod_named(name: &str, word: &str) -> Result<ModName, InputError> {
    ModName::from_name(word).ok_or_else(|| {
        let names: Vec<&str> = ModName::ALL.iter().map(|the_mod| the_mod.name()).collect();
        bad(
            name,
            format!(
                "there's no mod named {word}; the mods are {}",
                names.join(", ")
            ),
        )
    })
}

fn fits(name: &str, the_mod: ModName, place: Place) -> Result<(), InputError> {
    if the_mod.fits(place) {
        Ok(())
    } else {
        Err(bad(
            name,
            format!("{the_mod} can't show {}", place.phrase()),
        ))
    }
}

fn mod_change(name: &str, the_mod: ModName, words: &[&str]) -> Result<ModPlaceChange, InputError> {
    let change = match words {
        ["fold"] => ModPlaceChange::Fold,
        ["open"] => ModPlaceChange::Open,
        [step @ ("up" | "down"), place @ ..] => {
            let place = match place {
                [] => None,
                [word] => Some(Place::from_word(word).ok_or_else(|| not_a_place(name, word))?),
                _ => return Err(bad(name, format!("use /mods {the_mod} {step} [place]"))),
            };
            if *step == "up" {
                ModPlaceChange::Up { place }
            } else {
                ModPlaceChange::Down { place }
            }
        }
        ["units", units] => {
            let units = Units::from_word(units)
                .filter(|units| the_mod.units().contains(units))
                .ok_or_else(|| {
                    let takes: Vec<&str> = the_mod.units().iter().map(|u| u.word()).collect();
                    match takes.as_slice() {
                        [] => bad(name, format!("{the_mod} has no choice of units")),
                        _ => bad(name, format!("{the_mod} takes {}", takes.join(" or "))),
                    }
                })?;
            ModPlaceChange::Units { units }
        }
        [word] => {
            if let Some(place) = Place::from_word(word) {
                fits(name, the_mod, place)?;
                ModPlaceChange::Toggle { place }
            } else if let Some(form) = ModShape::from_word(word) {
                if !the_mod.forms().contains(&form) {
                    let draws: Vec<&str> = the_mod.forms().iter().map(|f| f.word()).collect();
                    return Err(bad(
                        name,
                        match draws.as_slice() {
                            [] => format!("{the_mod} has no card to draw"),
                            _ => format!("{the_mod} draws {}", draws.join(", ")),
                        },
                    ));
                }
                ModPlaceChange::Form { form }
            } else {
                return Err(not_a_change(name));
            }
        }
        _ => return Err(not_a_change(name)),
    };
    Ok(change)
}

fn not_a_place(name: &str, word: &str) -> InputError {
    let places: Vec<&str> = Place::ALL.iter().map(|place| place.word()).collect();
    bad(
        name,
        format!("{word} isn't a place; use {}", places.join(", ")),
    )
}

fn not_a_change(name: &str) -> InputError {
    bad(
        name,
        "after the mod, use a place (above, bottom-line, status, usage, context, limits), \
         fold, open, up, down, a form (full, compact, line), or units <units>",
    )
}

#[cfg(test)]
mod tests {
    use super::super::{ParsedInput, parse_input};
    use super::*;

    fn parsed(text: &str) -> Result<SlashCommand, InputError> {
        match parse_input(text)? {
            ParsedInput::Slash { command } => Ok(command),
            other => panic!("{text}: {other:?}"),
        }
    }

    fn problem(text: &str) -> String {
        match parsed(text) {
            Err(InputError::BadArguments { problem, .. }) => problem,
            other => panic!("{text}: {other:?}"),
        }
    }

    fn change(the_mod: ModName, change: ModPlaceChange) -> SlashCommand {
        SlashCommand::Mods {
            action: ModsAction::Change { the_mod, change },
            scope: None,
        }
    }

    #[test]
    fn the_overview_commands_open_print_or_pin_their_tab() {
        let pin = ViewAction::Pin { scope: None };
        assert_eq!(
            parsed("/status"),
            Ok(SlashCommand::Status {
                action: ViewAction::Open
            })
        );
        assert_eq!(
            parsed("/status print"),
            Ok(SlashCommand::Status {
                action: ViewAction::Print
            })
        );
        assert_eq!(
            parsed("/context pin --project"),
            Ok(SlashCommand::Context {
                all: false,
                action: ViewAction::Pin {
                    scope: Some(SettingScope::Project)
                }
            })
        );
        assert_eq!(
            parsed("/context all"),
            Ok(SlashCommand::Context {
                all: true,
                action: ViewAction::Open
            })
        );
        assert_eq!(
            parsed("/usage month print"),
            Ok(SlashCommand::Usage {
                period: Some("month".into()),
                action: ViewAction::Print
            })
        );
        assert_eq!(
            parsed("/usage pin"),
            Ok(SlashCommand::Usage {
                period: None,
                action: pin
            })
        );
        assert_eq!(
            parsed("/limits pin --local"),
            Ok(SlashCommand::Limits {
                key: None,
                value: None,
                action: ViewAction::Pin {
                    scope: Some(SettingScope::ProjectLocal)
                }
            })
        );
        assert_eq!(
            parsed("/limits spend_per_run 30"),
            Ok(SlashCommand::Limits {
                key: Some("spend_per_run".into()),
                value: Some("30".into()),
                action: ViewAction::Open
            })
        );
        assert_eq!(problem("/status now"), "use /status [print|pin]");
        assert_eq!(
            problem("/context all now"),
            "use /context [all] [print|pin]"
        );
        assert_eq!(problem("/status print pin"), "use print or pin, not both");
        assert_eq!(
            problem("/status print --global"),
            "--session, --local, --project, or --global go with pin"
        );
        assert_eq!(
            problem("/context pin --global --local"),
            "pick one of --session, --local, --project, or --global"
        );
        assert_eq!(
            problem("/usage pin --everywhere"),
            "--everywhere isn't a flag; use --session, --local, --project, or --global"
        );
    }

    #[test]
    fn stats_takes_a_period_and_pins_but_never_prints() {
        let stats = |period: Option<&str>, action| SlashCommand::Stats {
            period: period.map(str::to_owned),
            action,
        };
        assert_eq!(parsed("/stats"), Ok(stats(None, ViewAction::Open)));
        assert_eq!(
            parsed("/stats week"),
            Ok(stats(Some("week"), ViewAction::Open))
        );
        assert_eq!(
            parsed("/stats pin"),
            Ok(stats(None, ViewAction::Pin { scope: None }))
        );
        assert_eq!(
            problem("/stats print"),
            "use /stats [today|week|month|all] [pin]"
        );
        assert_eq!(
            problem("/stats soon"),
            "use /stats [today|week|month|all] [pin]"
        );
    }

    #[test]
    fn mods_takes_a_mod_and_one_change() {
        use ModName::*;
        assert_eq!(
            parsed("/mods"),
            Ok(SlashCommand::Mods {
                action: ModsAction::List,
                scope: None
            })
        );
        assert_eq!(
            parsed("/mods plan-usage"),
            Ok(SlashCommand::Mods {
                action: ModsAction::Page { the_mod: PlanUsage },
                scope: None
            })
        );
        assert_eq!(
            parsed("/mods hide --global"),
            Ok(SlashCommand::Mods {
                action: ModsAction::Band { shown: false },
                scope: Some(SettingScope::Global)
            })
        );
        let toggle = |place| ModPlaceChange::Toggle { place };
        assert_eq!(
            parsed("/mods context above"),
            Ok(change(Context, toggle(Place::AboveInput)))
        );
        // The bottom line reads its own file, so a card goes onto it as the mod it is.
        assert_eq!(
            parsed("/mods plan-usage bottom-line"),
            Ok(SlashCommand::ModFile {
                name: "plan-usage".to_owned(),
                words: vec!["bottom-line".to_owned()],
            })
        );
        assert_eq!(
            parsed("/mods spend bottom-line"),
            Ok(SlashCommand::ModFile {
                name: "spend".to_owned(),
                words: vec!["bottom-line".to_owned()],
            })
        );
        assert_eq!(
            parsed("/mods context context"),
            Ok(change(Context, toggle(Place::Context)))
        );
        assert_eq!(
            parsed("/mods run-limits fold"),
            Ok(change(RunLimits, ModPlaceChange::Fold))
        );
        assert_eq!(
            parsed("/mods run-limits open"),
            Ok(change(RunLimits, ModPlaceChange::Open))
        );
        assert_eq!(
            parsed("/mods plan-usage up"),
            Ok(change(PlanUsage, ModPlaceChange::Up { place: None }))
        );
        assert_eq!(
            parsed("/mods plan-usage down above"),
            Ok(change(
                PlanUsage,
                ModPlaceChange::Down {
                    place: Some(Place::AboveInput)
                }
            ))
        );
        assert_eq!(
            parsed("/mods plan-usage compact"),
            Ok(change(
                PlanUsage,
                ModPlaceChange::Form {
                    form: ModShape::Compact
                }
            ))
        );
        assert_eq!(
            parsed("/mods context units tokens"),
            Ok(change(
                Context,
                ModPlaceChange::Units {
                    units: Units::Tokens
                }
            ))
        );
        assert_eq!(
            parsed("/mods context above --project"),
            Ok(SlashCommand::Mods {
                action: ModsAction::Change {
                    the_mod: Context,
                    change: toggle(Place::AboveInput)
                },
                scope: Some(SettingScope::Project)
            })
        );
    }

    #[test]
    fn a_name_the_cards_dont_have_may_be_a_mod_file() {
        let file = |name: &str, words: &[&str]| {
            Ok(SlashCommand::ModFile {
                name: name.to_owned(),
                words: words.iter().map(|word| (*word).to_owned()).collect(),
            })
        };
        assert_eq!(parsed("/mods checks"), file("checks", &[]));
        assert_eq!(parsed("/mods bottom-line"), file("bottom-line", &[]));
        assert_eq!(parsed("/mods branch above"), file("branch", &["above"]));
        assert_eq!(parsed("/pin checks"), file("checks", &["pin"]));
        assert_eq!(parsed("/unpin checks"), file("checks", &["unpin"]));
        assert_eq!(
            problem("/mods checks --project"),
            "checks is a file, which its page saves; --session, --local, --project, or --global \
             don't apply"
        );
        // The whole band still hides and shows.
        assert_eq!(
            parsed("/mods hide"),
            Ok(SlashCommand::Mods {
                action: ModsAction::Band { shown: false },
                scope: None
            })
        );
    }

    #[test]
    fn mods_says_what_a_mod_cant_do() {
        assert!(problem("/mods Ctx").starts_with("there's no mod named Ctx;"));
        assert_eq!(
            problem("/mods agent above"),
            "agent can't show above the input"
        );
        assert_eq!(problem("/mods sessions full"), "sessions draws list");
        assert_eq!(problem("/mods agent line"), "agent has no card to draw");
        assert!(problem("/mods agent bottom-line").starts_with("after the mod, use a place"));
        assert_eq!(
            problem("/mods plan-usage units usd"),
            "plan-usage has no choice of units"
        );
        assert_eq!(
            problem("/mods spend units percent"),
            "spend takes usd or tokens"
        );
        assert!(problem("/mods plan-usage up sideways").starts_with("sideways isn't a place;"));
        assert!(problem("/mods plan-usage sparkle").starts_with("after the mod, use a place"));
        assert_eq!(
            problem("/mods plan-usage --project"),
            "--session, --local, --project, or --global go with a change"
        );
    }

    /// The words name the same flags the settings commands take.
    #[test]
    fn the_flags_in_words_are_the_settings_commands_flags() {
        for (flag, _) in super::super::SCOPE_FLAGS {
            assert!(SCOPE_FLAGS.contains(flag), "{flag}");
        }
        assert_eq!(
            SCOPE_FLAGS.matches("--").count(),
            super::super::SCOPE_FLAGS.len()
        );
    }

    #[test]
    fn pin_and_unpin_name_a_mod_above_the_input() {
        let above = Place::AboveInput;
        assert_eq!(
            parsed("/pin context"),
            Ok(change(
                ModName::Context,
                ModPlaceChange::Show { place: above }
            ))
        );
        assert_eq!(
            parsed("/unpin plan-usage --local"),
            Ok(SlashCommand::Mods {
                action: ModsAction::Change {
                    the_mod: ModName::PlanUsage,
                    change: ModPlaceChange::Hide { place: above }
                },
                scope: Some(SettingScope::ProjectLocal)
            })
        );
        assert_eq!(
            parsed("/pin context --session"),
            Ok(SlashCommand::Mods {
                action: ModsAction::Change {
                    the_mod: ModName::Context,
                    change: ModPlaceChange::Show { place: above }
                },
                scope: Some(SettingScope::Session)
            })
        );
        assert_eq!(problem("/pin"), "needs a mod, such as /pin context");
        assert_eq!(problem("/pin agent"), "agent can't show above the input");
        assert_eq!(problem("/unpin a b"), "use /unpin <mod>");
    }

    #[test]
    fn a_change_names_one_setting_and_its_new_value() {
        use ModName::*;
        let above = Place::AboveInput;
        let set = |key: &str, value: &str| Some((key.to_owned(), value.to_owned()));
        let mut ui = UiSettings::default();
        let toggle = ModPlaceChange::Toggle { place: above };

        assert_eq!(
            toggle.setting(Context, &ui),
            set("ui.above_input.mods", r#"["context"]"#)
        );
        ui.above_input.mods = vec![Context, PlanUsage];
        assert_eq!(
            toggle.setting(Context, &ui),
            set("ui.above_input.mods", r#"["plan-usage"]"#)
        );
        assert_eq!(
            ModPlaceChange::Show { place: above }.setting(Context, &ui),
            None
        );
        assert_eq!(
            ModPlaceChange::Hide { place: above }.setting(RunLimits, &ui),
            None
        );
        assert_eq!(
            ModPlaceChange::Show { place: above }.setting(RunLimits, &ui),
            set(
                "ui.above_input.mods",
                r#"["context", "plan-usage", "run-limits"]"#
            )
        );
        let up = ModPlaceChange::Up { place: None };
        assert_eq!(
            up.setting(PlanUsage, &ui),
            set("ui.above_input.mods", r#"["plan-usage", "context"]"#)
        );
        assert_eq!(up.setting(Context, &ui), None);
        // Not above the input, so the first place that lists it moves.
        assert_eq!(
            up.setting(Runs, &ui),
            set(
                "ui.views.usage.mods",
                r#"["plan-usage", "spend", "tokens", "runs", "history"]"#
            )
        );
        let down = ModPlaceChange::Down {
            place: Some(Place::AboveInput),
        };
        assert_eq!(
            down.setting(Context, &ui),
            set("ui.above_input.mods", r#"["plan-usage", "context"]"#)
        );
        assert_eq!(
            ModPlaceChange::Fold.setting(Context, &ui),
            set("ui.mods.context.open", "false")
        );
        assert_eq!(ModPlaceChange::Open.setting(Context, &ui), None);
        let compact = ModPlaceChange::Form {
            form: ModShape::Compact,
        };
        assert_eq!(
            compact.setting(PlanUsage, &ui),
            set("ui.mods.plan-usage.form", r#""compact""#)
        );
        let tokens = ModPlaceChange::Units {
            units: Units::Tokens,
        };
        assert_eq!(
            tokens.setting(Context, &ui),
            set("ui.mods.context.units", r#""tokens""#)
        );
    }

    #[test]
    fn the_new_forms_round_trip_as_json() {
        for text in [
            "/status pin --global",
            "/usage week",
            "/context all print",
            "/mods plan-usage down usage --local",
            "/pin context",
        ] {
            let command = parsed(text).unwrap();
            let json = serde_json::to_string(&command).unwrap();
            let back: SlashCommand = serde_json::from_str(&json).unwrap();
            assert_eq!(back, command, "{json}");
        }
        // What older outside agents send still reads.
        let old: SlashCommand = serde_json::from_str(r#"{"command":"status"}"#).unwrap();
        assert_eq!(
            old,
            SlashCommand::Status {
                action: ViewAction::Open
            }
        );
        let old: SlashCommand =
            serde_json::from_str(r#"{"command":"usage","period":null}"#).unwrap();
        assert_eq!(
            old,
            SlashCommand::Usage {
                period: None,
                action: ViewAction::Open
            }
        );
    }
}
