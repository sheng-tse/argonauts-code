//! A scope in words, and the scope that holds a layer's values, for resets.

use argonauts_core::settings::SettingScope;

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
