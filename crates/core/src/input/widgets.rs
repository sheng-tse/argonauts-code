//! `/widgets`, `/pin`, `/unpin`, and the `print` and `pin` words of `/status`, `/usage`,
//! `/context`, and `/budget`.

use serde::{Deserialize, Serialize};

use super::{InputError, SlashCommand, bad, key_and_value};
use crate::settings::SettingScope;
use crate::settings::mods::ModPlace;
use crate::settings::stats::StatsPeriod;
use crate::settings::ui::{Place, UiSettings, Units, WidgetForm, WidgetName};

/// What `/status`, `/usage`, `/context`, and `/budget` do with their tab of the Overview.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum ViewAction {
    /// Opens the tab, as `ui.views.<view>.open` says.
    #[default]
    Open,
    /// Prints the tab's cards into the transcript once.
    Print,
    /// Pins the tab's first widget above the input, or unpins it when it's there.
    Pin {
        /// Where the change is saved; unset is the layer the list came from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<SettingScope>,
    },
}

impl ViewAction {
    /// Left out of the wire form, so what older front ends send reads the same.
    pub fn is_open(&self) -> bool {
        *self == ViewAction::Open
    }
}

/// What `/widgets` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum WidgetsAction {
    /// Every widget in every place.
    List,
    /// One widget's page.
    Page { widget: WidgetName },
    /// Shows or hides the whole band above the input.
    Band { shown: bool },
    Change {
        widget: WidgetName,
        change: WidgetChange,
    },
}

/// One change to one widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum WidgetChange {
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
        form: WidgetForm,
    },
    Units {
        units: Units,
    },
}

impl WidgetChange {
    /// The one setting this change writes, with its new value as TOML text, from `ui` as it
    /// is now. `None` when it would change nothing, such as pinning what's already pinned.
    pub fn setting(self, widget: WidgetName, ui: &UiSettings) -> Option<(String, String)> {
        let list = |place: Place, edit: &dyn Fn(&mut Vec<WidgetName>) -> bool| {
            let mut widgets = ui.place(place).to_vec();
            edit(&mut widgets).then(|| (place.key().to_owned(), names(&widgets)))
        };
        let own = |field: &str, value: String| (format!("ui.mods.{widget}.{field}"), value);
        let first_place = |place: Option<Place>| {
            place.or_else(|| {
                Place::ALL
                    .into_iter()
                    .find(|place| ui.place(*place).contains(&widget))
            })
        };
        let look = ui.widget(widget);
        match self {
            WidgetChange::Toggle { place } => list(place, &|widgets| {
                match widgets.iter().position(|shown| *shown == widget) {
                    Some(at) => {
                        widgets.remove(at);
                    }
                    None => widgets.push(widget),
                }
                true
            }),
            WidgetChange::Show { place } => list(place, &|widgets| {
                let missing = !widgets.contains(&widget);
                if missing {
                    widgets.push(widget);
                }
                missing
            }),
            WidgetChange::Hide { place } => list(place, &|widgets| {
                let before = widgets.len();
                widgets.retain(|shown| *shown != widget);
                widgets.len() != before
            }),
            WidgetChange::Fold => look.open.then(|| own("open", "false".to_owned())),
            WidgetChange::Open => (!look.open).then(|| own("open", "true".to_owned())),
            WidgetChange::Up { place } => list(first_place(place)?, &|widgets| match widgets
                .iter()
                .position(|shown| *shown == widget)
            {
                Some(at) if at > 0 => {
                    widgets.swap(at - 1, at);
                    true
                }
                _ => false,
            }),
            WidgetChange::Down { place } => list(first_place(place)?, &|widgets| match widgets
                .iter()
                .position(|shown| *shown == widget)
            {
                Some(at) if at + 1 < widgets.len() => {
                    widgets.swap(at, at + 1);
                    true
                }
                _ => false,
            }),
            WidgetChange::Form { form } => {
                (look.form != form).then(|| own("form", quoted(form.word())))
            }
            WidgetChange::Units { units } => {
                (look.units != Some(units)).then(|| own("units", quoted(units.word())))
            }
        }
    }
}

fn quoted(word: &str) -> String {
    toml::Value::String(word.to_owned()).to_string()
}

fn names(widgets: &[WidgetName]) -> String {
    let words = widgets
        .iter()
        .map(|widget| toml::Value::String(widget.name().to_owned()))
        .collect();
    toml::Value::Array(words).to_string()
}

const SCOPE_FLAGS: &str = "--global, --project, or --local";

/// Takes `--global`, `--project`, or `--local` out of `words`.
fn take_scope(name: &str, words: &mut Vec<&str>) -> Result<Option<SettingScope>, InputError> {
    let mut scope = None;
    let mut rest = Vec::with_capacity(words.len());
    for word in words.drain(..) {
        if !word.starts_with("--") {
            rest.push(word);
            continue;
        }
        let picked = SettingScope::from_flag(word)
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

/// `/stats`, `/stats 7d` or `/stats pin`. It has no printed form: stats are drawn.
pub(super) fn parse_stats(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    const USAGE: &str = "use /stats [today|7d|30d|all] [pin]";
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

/// `/budget print`, `/budget pin`, or `/budget <key> <value>`, which sets one key.
pub(super) fn parse_budget(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    const USAGE: &str = "use /limits [print|pin], or /limits <key> <value>";
    let mut words = words(args);
    let action = view_action(name, &mut words)?;
    if action != ViewAction::Open {
        if !words.is_empty() {
            return Err(bad(name, USAGE));
        }
        return Ok(SlashCommand::Budget {
            key: None,
            value: None,
            action,
        });
    }
    let (key, value) = key_and_value(args);
    if key.is_some() && value.is_none() {
        return Err(bad(name, USAGE));
    }
    Ok(SlashCommand::Budget { key, value, action })
}

/// A name the closed list of cards doesn't have, which may be a mod's file. The app knows the
/// files, so it answers for a name that is none.
fn file_mod(word: &str) -> Option<String> {
    (WidgetName::from_name(word).is_none() && crate::settings::mods::is_mod_name(word))
        .then(|| word.to_owned())
}

pub(super) fn parse_widgets(name: &str, args: &str) -> Result<SlashCommand, InputError> {
    let mut words = words(args);
    let scope = take_scope(name, &mut words)?;
    // The bottom line reads its own mod file, so a built-in card goes onto it the way a mod of
    // yours does, by the mod's name.
    if let [first, place] = words.as_slice()
        && scope.is_none()
        && ModPlace::from_word(place) == Some(ModPlace::ModeLine)
        && let Some(card) = WidgetName::from_name(first)
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
        [] => WidgetsAction::List,
        ["hide"] => WidgetsAction::Band { shown: false },
        ["show"] => WidgetsAction::Band { shown: true },
        [widget] => WidgetsAction::Page {
            widget: widget_named(name, widget)?,
        },
        [widget, change @ ..] => {
            let widget = widget_named(name, widget)?;
            WidgetsAction::Change {
                widget,
                change: widget_change(name, widget, change)?,
            }
        }
    };
    if scope.is_some() && matches!(action, WidgetsAction::List | WidgetsAction::Page { .. }) {
        return Err(bad(name, format!("{SCOPE_FLAGS} go with a change")));
    }
    Ok(SlashCommand::Widgets { action, scope })
}

/// `/pin <widget>` shows it above the input; `/unpin <widget>` takes it away.
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
    let widget = match words.as_slice() {
        [widget] => widget_named(name, widget)?,
        [] => {
            return Err(bad(name, format!("needs a mod, such as /{name} context")));
        }
        _ => return Err(bad(name, format!("use /{name} <mod>"))),
    };
    let place = Place::AboveInput;
    let change = if pin {
        fits(name, widget, place)?;
        WidgetChange::Show { place }
    } else {
        WidgetChange::Hide { place }
    };
    Ok(SlashCommand::Widgets {
        action: WidgetsAction::Change { widget, change },
        scope,
    })
}

fn widget_named(name: &str, word: &str) -> Result<WidgetName, InputError> {
    WidgetName::from_name(word).ok_or_else(|| {
        let names: Vec<&str> = WidgetName::ALL.iter().map(|widget| widget.name()).collect();
        bad(
            name,
            format!(
                "there's no mod named {word}; the mods are {}",
                names.join(", ")
            ),
        )
    })
}

fn fits(name: &str, widget: WidgetName, place: Place) -> Result<(), InputError> {
    if widget.fits(place) {
        Ok(())
    } else {
        Err(bad(name, format!("{widget} can't show {}", place.phrase())))
    }
}

fn widget_change(
    name: &str,
    widget: WidgetName,
    words: &[&str],
) -> Result<WidgetChange, InputError> {
    let change = match words {
        ["fold"] => WidgetChange::Fold,
        ["open"] => WidgetChange::Open,
        [step @ ("up" | "down"), place @ ..] => {
            let place = match place {
                [] => None,
                [word] => Some(Place::from_word(word).ok_or_else(|| not_a_place(name, word))?),
                _ => return Err(bad(name, format!("use /mods {widget} {step} [place]"))),
            };
            if *step == "up" {
                WidgetChange::Up { place }
            } else {
                WidgetChange::Down { place }
            }
        }
        ["units", units] => {
            let units = Units::from_word(units)
                .filter(|units| widget.units().contains(units))
                .ok_or_else(|| {
                    let takes: Vec<&str> = widget.units().iter().map(|u| u.word()).collect();
                    match takes.as_slice() {
                        [] => bad(name, format!("{widget} has no choice of units")),
                        _ => bad(name, format!("{widget} takes {}", takes.join(" or "))),
                    }
                })?;
            WidgetChange::Units { units }
        }
        [word] => {
            if let Some(place) = Place::from_word(word) {
                fits(name, widget, place)?;
                WidgetChange::Toggle { place }
            } else if let Some(form) = WidgetForm::from_word(word) {
                if !widget.forms().contains(&form) {
                    let draws: Vec<&str> = widget.forms().iter().map(|f| f.word()).collect();
                    return Err(bad(
                        name,
                        match draws.as_slice() {
                            [] => format!("{widget} has no card to draw"),
                            _ => format!("{widget} draws {}", draws.join(", ")),
                        },
                    ));
                }
                WidgetChange::Form { form }
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
        "after the mod, use a place (above, mode-line, status, usage, context, limit), \
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

    fn change(widget: WidgetName, change: WidgetChange) -> SlashCommand {
        SlashCommand::Widgets {
            action: WidgetsAction::Change { widget, change },
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
            Ok(SlashCommand::Budget {
                key: None,
                value: None,
                action: ViewAction::Pin {
                    scope: Some(SettingScope::ProjectLocal)
                }
            })
        );
        assert_eq!(
            parsed("/limits spend_per_run 30"),
            Ok(SlashCommand::Budget {
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
            "--global, --project, or --local go with pin"
        );
        assert_eq!(
            problem("/context pin --global --local"),
            "pick one of --global, --project, or --local"
        );
        assert_eq!(
            problem("/usage pin --everywhere"),
            "--everywhere isn't a flag; use --global, --project, or --local"
        );
    }

    #[test]
    fn stats_takes_a_period_and_pins_but_never_prints() {
        let stats = |period: Option<&str>, action| SlashCommand::Stats {
            period: period.map(str::to_owned),
            action,
        };
        assert_eq!(parsed("/stats"), Ok(stats(None, ViewAction::Open)));
        assert_eq!(parsed("/stats 7d"), Ok(stats(Some("7d"), ViewAction::Open)));
        assert_eq!(
            parsed("/stats pin"),
            Ok(stats(None, ViewAction::Pin { scope: None }))
        );
        assert_eq!(
            problem("/stats print"),
            "use /stats [today|7d|30d|all] [pin]"
        );
        assert_eq!(
            problem("/stats soon"),
            "use /stats [today|7d|30d|all] [pin]"
        );
    }

    #[test]
    fn widgets_takes_a_widget_and_one_change() {
        use WidgetName::*;
        assert_eq!(
            parsed("/mods"),
            Ok(SlashCommand::Widgets {
                action: WidgetsAction::List,
                scope: None
            })
        );
        assert_eq!(
            parsed("/mods plan-usage"),
            Ok(SlashCommand::Widgets {
                action: WidgetsAction::Page { widget: Quota },
                scope: None
            })
        );
        assert_eq!(
            parsed("/mods hide --global"),
            Ok(SlashCommand::Widgets {
                action: WidgetsAction::Band { shown: false },
                scope: Some(SettingScope::Global)
            })
        );
        let toggle = |place| WidgetChange::Toggle { place };
        assert_eq!(
            parsed("/mods context above"),
            Ok(change(Context, toggle(Place::AboveInput)))
        );
        // The bottom line reads its own file, so a card goes onto it as the mod it is.
        assert_eq!(
            parsed("/mods plan-usage mode-line"),
            Ok(SlashCommand::ModFile {
                name: "plan-usage".to_owned(),
                words: vec!["mode-line".to_owned()],
            })
        );
        assert_eq!(
            parsed("/mods spend mode-line"),
            Ok(SlashCommand::ModFile {
                name: "spend".to_owned(),
                words: vec!["mode-line".to_owned()],
            })
        );
        assert_eq!(
            parsed("/mods context context"),
            Ok(change(Context, toggle(Place::Context)))
        );
        assert_eq!(
            parsed("/mods run-limits fold"),
            Ok(change(Budget, WidgetChange::Fold))
        );
        assert_eq!(
            parsed("/mods run-limits open"),
            Ok(change(Budget, WidgetChange::Open))
        );
        assert_eq!(
            parsed("/mods plan-usage up"),
            Ok(change(Quota, WidgetChange::Up { place: None }))
        );
        assert_eq!(
            parsed("/mods plan-usage down above"),
            Ok(change(
                Quota,
                WidgetChange::Down {
                    place: Some(Place::AboveInput)
                }
            ))
        );
        assert_eq!(
            parsed("/mods plan-usage compact"),
            Ok(change(
                Quota,
                WidgetChange::Form {
                    form: WidgetForm::Compact
                }
            ))
        );
        assert_eq!(
            parsed("/mods context units tokens"),
            Ok(change(
                Context,
                WidgetChange::Units {
                    units: Units::Tokens
                }
            ))
        );
        assert_eq!(
            parsed("/mods context above --project"),
            Ok(SlashCommand::Widgets {
                action: WidgetsAction::Change {
                    widget: Context,
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
            "checks is a file, which its page saves; --global, --project, or --local don't apply"
        );
        // The whole band still hides and shows.
        assert_eq!(
            parsed("/mods hide"),
            Ok(SlashCommand::Widgets {
                action: WidgetsAction::Band { shown: false },
                scope: None
            })
        );
    }

    #[test]
    fn widgets_says_what_a_widget_cant_do() {
        assert!(problem("/mods Ctx").starts_with("there's no mod named Ctx;"));
        assert_eq!(
            problem("/mods agent above"),
            "agent can't show above the input"
        );
        assert_eq!(problem("/mods sessions full"), "sessions draws list");
        assert_eq!(problem("/mods agent line"), "agent has no card to draw");
        assert!(problem("/mods agent mode-line").starts_with("after the mod, use a place"));
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
            "--global, --project, or --local go with a change"
        );
    }

    #[test]
    fn pin_and_unpin_name_a_widget_above_the_input() {
        let above = Place::AboveInput;
        assert_eq!(
            parsed("/pin context"),
            Ok(change(
                WidgetName::Context,
                WidgetChange::Show { place: above }
            ))
        );
        assert_eq!(
            parsed("/unpin plan-usage --local"),
            Ok(SlashCommand::Widgets {
                action: WidgetsAction::Change {
                    widget: WidgetName::Quota,
                    change: WidgetChange::Hide { place: above }
                },
                scope: Some(SettingScope::ProjectLocal)
            })
        );
        assert_eq!(problem("/pin"), "needs a mod, such as /pin context");
        assert_eq!(problem("/pin agent"), "agent can't show above the input");
        assert_eq!(problem("/unpin a b"), "use /unpin <mod>");
    }

    #[test]
    fn a_change_names_one_setting_and_its_new_value() {
        use WidgetName::*;
        let above = Place::AboveInput;
        let set = |key: &str, value: &str| Some((key.to_owned(), value.to_owned()));
        let mut ui = UiSettings::default();
        let toggle = WidgetChange::Toggle { place: above };

        assert_eq!(
            toggle.setting(Context, &ui),
            set("ui.above_input.mods", r#"["context"]"#)
        );
        ui.above_input.mods = vec![Context, Quota];
        assert_eq!(
            toggle.setting(Context, &ui),
            set("ui.above_input.mods", r#"["plan-usage"]"#)
        );
        assert_eq!(
            WidgetChange::Show { place: above }.setting(Context, &ui),
            None
        );
        assert_eq!(
            WidgetChange::Hide { place: above }.setting(Budget, &ui),
            None
        );
        assert_eq!(
            WidgetChange::Show { place: above }.setting(Budget, &ui),
            set(
                "ui.above_input.mods",
                r#"["context", "plan-usage", "run-limits"]"#
            )
        );
        let up = WidgetChange::Up { place: None };
        assert_eq!(
            up.setting(Quota, &ui),
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
        let down = WidgetChange::Down {
            place: Some(Place::AboveInput),
        };
        assert_eq!(
            down.setting(Context, &ui),
            set("ui.above_input.mods", r#"["plan-usage", "context"]"#)
        );
        assert_eq!(
            WidgetChange::Fold.setting(Context, &ui),
            set("ui.mods.context.open", "false")
        );
        assert_eq!(WidgetChange::Open.setting(Context, &ui), None);
        let compact = WidgetChange::Form {
            form: WidgetForm::Compact,
        };
        assert_eq!(
            compact.setting(Quota, &ui),
            set("ui.mods.plan-usage.form", r#""compact""#)
        );
        let tokens = WidgetChange::Units {
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
        // What older front ends send still reads.
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
