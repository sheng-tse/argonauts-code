//! The built-in ACP profiles, OpenCode's and Hermes', as data in `clis/`, and Hermes'
//! home template.

use argonauts_core::models::registry::{CliManifest, RegistryFile};

use super::turned_on::builtin_counts;

/// Each file is a `models.toml` holding one `[[cli]]` entry, so you can copy one into
/// your own file and change it.
const BUILTIN: &[(&str, &str)] = &[
    (
        "opencode-acp.toml",
        include_str!("../../clis/opencode-acp.toml"),
    ),
    ("hermes.toml", include_str!("../../clis/hermes.toml")),
];

/// Home templates as (name, [(path inside the home, contents)]).
const HOME_TEMPLATES: &[(&str, &[(&str, &str)])] = &[(
    "hermes",
    &[("config.yaml", include_str!("../../clis/hermes/config.yaml"))],
)];

/// `opencode-acp`, and `hermes` when it's in `turned_on` (see `agents_turned_on`). Both stay
/// experimental until they pass conformance.
pub fn builtin_clis(turned_on: &[String]) -> Vec<CliManifest> {
    every_builtin_cli()
        .into_iter()
        .filter(|manifest| builtin_counts(&manifest.id, turned_on))
        .collect()
}

/// Every built-in profile, the ones still off included. A setting that names one isn't a typo,
/// and its provider is known, though it only runs once it's on.
pub(crate) fn every_builtin_cli() -> Vec<CliManifest> {
    builtin_files()
        .into_iter()
        .filter_map(|(_, parsed)| parsed.ok())
        .flatten()
        .collect()
}

/// Every built-in file parsed, for the loader, which reports one that doesn't parse, and for
/// the tests, which fail on it.
pub(super) fn builtin_files() -> Vec<(&'static str, Result<Vec<CliManifest>, String>)> {
    BUILTIN
        .iter()
        .map(|(name, text)| {
            let parsed = toml::from_str::<RegistryFile>(text)
                .map(|file| file.clis)
                .map_err(|err| err.to_string());
            (*name, parsed)
        })
        .collect()
}

/// A built-in home template's files as (path inside the home, contents). Templates come only
/// from the binary or `~/.argonauts/clis/`, never from a repo.
pub fn builtin_home_template(name: &str) -> Option<Vec<(&'static str, &'static str)>> {
    HOME_TEMPLATES
        .iter()
        .find(|(template, _)| *template == name)
        .map(|(_, files)| files.to_vec())
}
