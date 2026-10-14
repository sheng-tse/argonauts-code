//! The built-in cards, written as the same files you'd write. A file of yours with the same
//! name replaces one, and deleting your file brings the built-in one back.
//!
//! Two of them aren't a body from the kit: the bottom line and stats draw themselves, and
//! their files hold their own settings. They're listed with the rest, each with its own page.

use super::{ModFile, check};
use crate::settings::ui::ModName;

/// A mod that ships with Argonauts Code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Builtin {
    pub name: &'static str,
    /// Its file, as `/setup` or a copy writes it.
    pub text: &'static str,
    pub kind: BuiltinKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinKind {
    /// A card the kit draws from the file.
    Card,
    /// It draws itself, and its file holds its own settings.
    Own {
        /// One line for `/mods`.
        about: &'static str,
    },
}

/// The README `/setup` writes into `mods/`: every field, each body with a picture, the JSON a
/// command prints, and how to check a mod. `{version}` stands where the version goes.
pub const README: &str = crate::template!("mods/README.md");

pub const BOTTOM_LINE: &str = "bottom-line";
pub const STATS: &str = "stats";

pub const BUILTIN: &[Builtin] = &[
    card("context", crate::template!("mods/context.toml")),
    card("plan-usage", crate::template!("mods/plan-usage.toml")),
    card("spend", crate::template!("mods/spend.toml")),
    card("tokens", crate::template!("mods/tokens.toml")),
    card("runs", crate::template!("mods/runs.toml")),
    card("waiting", crate::template!("mods/waiting.toml")),
    Builtin {
        name: STATS,
        text: crate::template!("mods/stats.toml"),
        kind: BuiltinKind::Own {
            about: "what the crew did, per CLI and model and all together",
        },
    },
    Builtin {
        name: BOTTOM_LINE,
        text: crate::template!("mods/bottom-line.toml"),
        kind: BuiltinKind::Own {
            about: "the line under the input: its parts, their order and their side",
        },
    },
];

/// Mod files to start from: a stacked bar and meters from built-in numbers, the two that draw
/// themselves, a line of words from a command, and a stacked bar from a command that prints
/// JSON. The built-in cards draw from settings of their own, so `/setup` writes only the
/// others ([`is_written_by_setup`]).
pub const STARTERS: &[(&str, &str)] = &[
    ("context", crate::template!("mods/context.toml")),
    ("plan-usage", crate::template!("mods/plan-usage.toml")),
    (STATS, crate::template!("mods/stats.toml")),
    (BOTTOM_LINE, crate::template!("mods/bottom-line.toml")),
    ("branch", crate::template!("mods/branch.toml")),
    ("checks", crate::template!("mods/checks.toml")),
];

/// A program a starter's command names beside its file: the mod's name, the program's file
/// name, and its text. It prints nothing until you put your numbers in.
pub const STARTER_PROGRAMS: &[(&str, &str, &str)] =
    &[("checks", "checks.sh", crate::template!("mods/checks.sh"))];

const fn card(name: &'static str, text: &'static str) -> Builtin {
    Builtin {
        name,
        text,
        kind: BuiltinKind::Card,
    }
}

pub fn builtin(name: &str) -> Option<&'static Builtin> {
    BUILTIN.iter().find(|builtin| builtin.name == name)
}

pub fn is_builtin(name: &str) -> bool {
    builtin(name).is_some()
}

/// The card of the closed list that a built-in mod's name stands for. The band above the
/// input and the usage panel's tabs draw those cards themselves, where the place lists in your
/// settings say.
pub fn closed_card(name: &str) -> Option<ModName> {
    let is_card = matches!(
        builtin(name),
        Some(Builtin {
            kind: BuiltinKind::Card,
            ..
        })
    );
    ModName::from_name(name).filter(|_| is_card)
}

/// Whether `/setup` writes the starter `name`: a built-in card's file would change nothing on
/// the card, which draws from settings of its own.
pub fn is_written_by_setup(name: &str) -> bool {
    closed_card(name).is_none()
}

/// The built-in mod a card of the closed list is, by the mod's name: `plan-usage` for the card
/// the lists call `plan_usage`.
pub fn mod_of_card(card: ModName) -> Option<&'static str> {
    BUILTIN
        .iter()
        .map(|builtin| builtin.name)
        .find(|name| closed_card(name) == Some(card))
}

/// Whether a file of this name holds settings of its own and not a card.
pub fn draws_itself(name: &str) -> bool {
    matches!(
        builtin(name),
        Some(Builtin {
            kind: BuiltinKind::Own { .. },
            ..
        })
    )
}

/// The built-in cards, read from their files.
pub fn cards() -> Vec<ModFile> {
    BUILTIN
        .iter()
        .filter(|builtin| builtin.kind == BuiltinKind::Card)
        .filter_map(|builtin| check::read(builtin.name, builtin.text).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::mods::{BuiltinSource, ModSource};

    #[test]
    fn every_built_in_card_reads_without_a_problem() {
        for builtin in BUILTIN {
            if builtin.kind == BuiltinKind::Card {
                let file = check::read(builtin.name, builtin.text);
                assert!(file.is_ok(), "{}: {file:?}", builtin.name);
            }
        }
        assert_eq!(cards().len(), 6);
    }

    #[test]
    fn every_built_in_source_has_a_card() {
        let cards = cards();
        for source in BuiltinSource::ALL {
            assert!(
                cards
                    .iter()
                    .any(|card| card.data == ModSource::Builtin { source }),
                "{source:?}"
            );
        }
    }

    #[test]
    fn the_starter_cards_read_too() {
        for (name, text) in STARTERS {
            if draws_itself(name) {
                assert!(text.parse::<toml::Table>().is_ok(), "{name}");
            } else {
                assert!(check::read(name, text).is_ok(), "{name}");
            }
        }
        assert_eq!(
            STARTERS.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            [
                "context",
                "plan-usage",
                "stats",
                "bottom-line",
                "branch",
                "checks"
            ]
        );
    }

    #[test]
    fn a_starter_s_program_is_the_one_its_command_names_and_the_readme_shows() {
        for (name, file, text) in STARTER_PROGRAMS {
            let (_, starter) = STARTERS.iter().find(|(have, _)| have == name).unwrap();
            let ModSource::Command { command, .. } = check::read(name, starter).unwrap().data
            else {
                panic!("{name} runs no command");
            };
            assert_eq!(command, [format!("./{file}")]);
            assert!(README.contains(text.trim_end()), "{file}");
        }
    }

    #[test]
    fn a_built_in_command_is_only_ever_a_starter() {
        // Nothing runs a program unless you put its file there.
        assert!(cards().iter().all(|card| !card.data.is_command()));
    }

    /// The README names every field, body, place, colour and source a mod file can use, so a
    /// new one can't be added without its line there.
    #[test]
    fn the_readme_names_everything_a_mod_file_can_say() {
        use crate::settings::mods::{BuiltinSource, ModBody, ModColor, ModForm, ModPlace};
        let has = |word: &str| assert!(README.contains(&format!("`{word}`")), "{word}");
        ModBody::ALL.iter().for_each(|body| has(body.word()));
        ModPlace::ALL.iter().for_each(|place| has(place.word()));
        ModForm::ALL.iter().for_each(|form| has(form.word()));
        ModColor::ALL.iter().for_each(|color| has(color.word()));
        BuiltinSource::ALL
            .iter()
            .for_each(|source| has(source.word()));
        for key in ["name", "title", "body", "places", "shown", "form", "key"] {
            has(key);
        }
        for key in ["source", "command", "every", "timeout", "warn", "danger"] {
            assert!(README.contains(&format!(" {key}`")), "{key}");
        }
        for key in ["summary", "level", "badge", "parts"] {
            has(key);
        }
        for key in ["label", "value", "max", "color", "text", "note"] {
            has(&format!("parts[].{key}"));
        }
        assert!(README.contains("{version}"));
    }

    #[test]
    fn the_readme_s_examples_are_real_files() {
        use crate::settings::mods::ModReading;
        let blocks = |fence: &str| -> Vec<&str> {
            README
                .split(&format!("```{fence}\n"))
                .skip(1)
                .filter_map(|rest| rest.split("```").next())
                .collect()
        };
        let toml = blocks("toml");
        assert_eq!(toml.len(), 2);
        assert!(
            check::read("checks", toml[0]).is_ok(),
            "{:?}",
            check::read("checks", toml[0])
        );
        assert!(
            check::read("branch", toml[1]).is_ok(),
            "{:?}",
            check::read("branch", toml[1])
        );
        let json = blocks("json");
        assert_eq!(json.len(), 1);
        assert_eq!(
            ModReading::from_output(json[0]).unwrap(),
            ModReading::sample(crate::settings::mods::ModBody::Stack)
        );
    }
}
