//! The scope question: the four scopes with the command's default recommended, and a warning
//! when a higher layer wins over the scope you pick, said once at the top when it wins over
//! every file.

use argonauts_core::app::{AppChoice, AppMode, ChoiceKind};
use argonauts_core::events::checkpoint::AnswerAction;
use argonauts_core::names::ChoiceId;
use argonauts_core::settings::SettingScope;

use crate::choices::{app_choice, scope_from_value, scope_options};
use crate::controller::Ctx;

pub(crate) fn scope_question(
    ctx: &Ctx,
    id: ChoiceId,
    key: &str,
    value: &str,
    recommended: SettingScope,
) -> AppChoice {
    let layer = ctx.settings.layer(key);
    let preset_from = ctx.settings.preset_from(&layer);
    let preset_from = preset_from.as_deref();
    let preset = ctx.settings().preset.clone();
    let local_tracked =
        crate::git::tracks(&ctx.env.repo_root, &ctx.env.project_local_config()).unwrap_or(false);
    let hides_every_file = hidden_from_files(&layer, preset_from, &preset);
    let mut options = scope_options(recommended);
    for option in &mut options {
        let AnswerAction::Choose { value } = &option.action else {
            continue;
        };
        let Some(scope) = scope_from_value(value) else {
            continue;
        };
        let mut notes: Vec<String> = match hides_every_file {
            Some(_) => Vec::new(),
            None => hidden_note(&layer, preset_from, scope, &preset)
                .into_iter()
                .collect(),
        };
        if scope == SettingScope::ProjectLocal && local_tracked {
            notes.push(
                "This repo tracks config.local.toml, so your team would see this change."
                    .to_owned(),
            );
        }
        if !notes.is_empty() {
            option.detail = Some(notes.join(" "));
        }
    }

    let now = ctx
        .settings
        .entry(key)
        .map(|entry| entry.value.clone())
        .filter(|value| !value.is_empty());
    let mut body = vec![format!("{key} = {value}")];
    body.push(match now {
        Some(now) => format!("Now {now}, from {}.", layer_words(&layer)),
        None => "Now unset.".to_owned(),
    });
    body.extend(hides_every_file);
    if key.starts_with("approvals.") && run_going(&ctx.mode) {
        body.push(
            "A run is going. It sees a change saved to a file at its next checkpoint, and a \
             change for this session only reaches runs started after it."
                .to_owned(),
        );
    }
    app_choice(
        id,
        ChoiceKind::SettingScope,
        "Where should this be saved?",
        body,
        options,
    )
}

/// The scopes that save to a file.
const FILE_SCOPES: [SettingScope; 3] = [
    SettingScope::ProjectLocal,
    SettingScope::Project,
    SettingScope::Global,
];

/// What to say once when a value from `layer` wins over every file: a preset, a `--set` flag,
/// or this session's own override. `None` when some file would still take effect, as the file
/// that picked the preset does (`preset_from`).
pub(crate) fn hidden_from_files(
    layer: &str,
    preset_from: Option<&str>,
    preset: &str,
) -> Option<String> {
    if !FILE_SCOPES
        .iter()
        .all(|scope| scope.hidden_by_value(layer, preset_from))
    {
        return None;
    }
    Some(match layer {
        "preset" => format!(
            "The {preset} preset sets this, so saving it in a file changes nothing. Use \
             /preset copy {preset} to edit the preset."
        ),
        "flag" => {
            "A --set flag sets this until you quit, so saving it in a file changes nothing now."
                .to_owned()
        }
        "session" => "This session's override sets this until you quit, so saving it in a file \
                      changes nothing now."
            .to_owned(),
        other => format!("The {other} layer sets this, so saving it in a file changes nothing."),
    })
}

/// What to say when a value from `layer` still wins over one saved in `scope`.
pub(crate) fn hidden_note(
    layer: &str,
    preset_from: Option<&str>,
    scope: SettingScope,
    preset: &str,
) -> Option<String> {
    if !scope.hidden_by_value(layer, preset_from) {
        return None;
    }
    let saving = match scope {
        SettingScope::Global => "saving it everywhere",
        SettingScope::Project => "saving it in the shared file",
        SettingScope::ProjectLocal => "saving it just for you",
        SettingScope::Session => "saving it",
    };
    Some(match layer {
        "project" => format!("This project's file sets this, so {saving} won't change this repo."),
        "project-local" => format!(
            "Your own project file (config.local.toml) sets this, so {saving} won't change this \
             repo for you."
        ),
        "preset" if preset_from.is_some() => format!(
            "The {preset} preset sets this, and it's picked in a later file, so {saving} changes \
             nothing. Save it where you picked the preset, or later."
        ),
        "preset" => format!(
            "The {preset} preset sets this, so {saving} changes nothing. Use /preset copy \
             {preset} to edit the preset."
        ),
        "flag" => {
            format!("A --set flag sets this until you quit, so {saving} changes nothing now.")
        }
        "session" => format!(
            "This session's override sets this until you quit, so {saving} changes nothing now."
        ),
        _ => return None,
    })
}

/// Where a value came from, in words.
pub(crate) fn layer_words(layer: &str) -> &'static str {
    match layer {
        "global" => "your global config",
        "project" => "this project's shared file",
        "project-local" => "your own project file",
        "preset" => "the preset",
        "flag" => "a --set flag",
        "session" => "this session",
        _ => "the built-in default",
    }
}

/// How the transcript names a scope.
pub(crate) fn scope_words(scope: SettingScope) -> &'static str {
    match scope {
        SettingScope::Session => "this session",
        SettingScope::ProjectLocal => "just you",
        SettingScope::Project => "this project",
        SettingScope::Global => "everywhere",
    }
}

/// The scope that saves into `layer`, for a reset of the value it holds.
pub(crate) fn scope_of_layer(layer: &str) -> Option<SettingScope> {
    [
        SettingScope::Session,
        SettingScope::ProjectLocal,
        SettingScope::Project,
        SettingScope::Global,
    ]
    .into_iter()
    .find(|scope| scope.layer() == layer)
}

fn run_going(mode: &AppMode) -> bool {
    matches!(
        mode,
        AppMode::Working { .. } | AppMode::Pausing { .. } | AppMode::Parked { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_higher_layer_is_named_with_what_to_do_instead() {
        let note = hidden_note("project", None, SettingScope::Global, "peer-crew").unwrap();
        assert_eq!(
            note,
            "This project's file sets this, so saving it everywhere won't change this repo."
        );
        assert!(hidden_note("project", None, SettingScope::ProjectLocal, "peer-crew").is_none());
        let note = hidden_note("preset", None, SettingScope::Project, "solo").unwrap();
        assert!(note.contains("/preset copy solo"), "{note}");
        assert!(hidden_note("session", None, SettingScope::Global, "p").is_some());
        assert!(hidden_note("session", None, SettingScope::Session, "p").is_none());
        assert!(hidden_note("built-in", None, SettingScope::Global, "p").is_none());
    }

    #[test]
    fn a_layer_over_every_file_is_named_once() {
        let once = hidden_from_files("session", None, "p").unwrap();
        assert_eq!(
            once,
            "This session's override sets this until you quit, so saving it in a file changes \
             nothing now."
        );
        assert!(
            hidden_from_files("flag", None, "p")
                .unwrap()
                .contains("--set flag")
        );
        assert!(
            hidden_from_files("preset", None, "solo")
                .unwrap()
                .contains("/preset copy solo")
        );
        for layer in ["built-in", "global", "project", "project-local"] {
            assert_eq!(hidden_from_files(layer, None, "p"), None, "{layer}");
        }
    }

    #[test]
    fn a_preset_picked_in_a_file_gives_way_to_that_file() {
        assert_eq!(hidden_from_files("preset", Some("global"), "solo"), None);
        assert_eq!(
            hidden_note("preset", Some("global"), SettingScope::Global, "solo"),
            None
        );
        let note = hidden_note("preset", Some("project"), SettingScope::Global, "solo").unwrap();
        assert!(note.contains("picked in a later file"), "{note}");
    }
}
