//! How the screen looks loads forgivingly: a bad value is named with its file, and its default
//! is used, so a typo never blanks a mod or stops the app.

use argonauts_core::settings::ui::{ModName, Theme, Threshold, TokenStyle};

use super::{Repo, layer_of};
use crate::{resolve, write_setting};

fn warning<'a>(warnings: &'a [String], about: &str) -> &'a str {
    warnings
        .iter()
        .find(|warning| warning.contains(about))
        .unwrap_or_else(|| panic!("no warning about {about}: {warnings:#?}"))
}

#[test]
fn a_bad_value_is_named_with_its_file_and_the_default_used() {
    let repo = Repo::new();
    repo.global(
        "[ui.format]\ntokens = \"tiny\"\nclock = \"24h\"\n\n\
         [ui.above_input]\nmods = [\"context\", \"agent\"]\n",
    );
    repo.project("[ui.mods.context]\nform = \"fulll\"\nwarn = \"85%\"\n");
    let resolved = resolve(&repo.trusted()).unwrap();
    let ui = &resolved.settings.ui;
    assert_eq!(ui.format.tokens, TokenStyle::Short);
    assert_eq!(ui.above_input.mods, [ModName::Context]);
    assert_eq!(ui.mods.context.form, None);
    assert_eq!(ui.mods.context.warn, Some(Threshold::percent(85.0)));

    let tokens = warning(&resolved.warnings, "ui.format.tokens");
    assert!(
        tokens.contains("\"tiny\"; it takes \"short\", \"lower\", or \"full\""),
        "{tokens}"
    );
    assert!(
        !resolved
            .warnings
            .iter()
            .any(|w| w.starts_with("unknown setting")),
        "{:?}",
        resolved.warnings
    );
    assert!(tokens.contains(&repo.global_file()), "{tokens}");
    assert!(tokens.ends_with("so the default is used"), "{tokens}");
    let form = warning(&resolved.warnings, "ui.mods.context.form");
    assert!(form.contains(&repo.project_file()), "{form}");
    let agent = warning(&resolved.warnings, "agent can't show above the input");
    assert!(agent.contains(&repo.global_file()), "{agent}");

    // What read stays where it came from, for /config and for saving a change.
    assert_eq!(
        layer_of(&resolved, "ui.mods.context.warn").as_deref(),
        Some("project")
    );
    assert_eq!(
        layer_of(&resolved, "ui.format.clock").as_deref(),
        Some("global")
    );
}

#[test]
fn custom_themes_load_and_save() {
    let repo = Repo::new();
    let file = repo.home().join("config.toml");
    write_setting(&file, "ui.theme", "\"custom:harbor\"").unwrap();
    write_setting(&file, "ui.theme", "\"dark-daltonized\"").unwrap();
    write_setting(&file, "ui.theme", "\"custom:harbor\"").unwrap();
    let resolved = resolve(&repo.trusted()).unwrap();
    assert_eq!(resolved.settings.ui.theme, Theme::Custom("harbor".into()));
    assert!(
        !resolved.warnings.iter().any(|w| w.contains("ui.theme")),
        "{:?}",
        resolved.warnings
    );
    assert!(write_setting(&file, "ui.theme", "\"custom:../x\"").is_err());
}
