//! The app choices' table in one place: each kind's options, keys, and actions, and what Esc
//! (`SetAside`) means for it.

use argonauts_core::app::{AppChoice, ChoiceKind};
use argonauts_core::events::checkpoint::{AnswerAction, AnswerOption, FreeText};
use argonauts_core::names::ChoiceId;
use argonauts_core::settings::SettingScope;

pub(crate) use argonauts_core::app::NOT_CHANGED;
pub(crate) const JUST_ASKING: &str = "Start work on it? · no, just asking";

/// What Esc on an app choice does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnSetAside {
    /// Stays waiting, like a run's card.
    SetAside,
    /// Closes with "Not changed.".
    NotChanged,
    /// The files stay untrusted, and the question comes back at the next start.
    NotNow,
    /// Offered again when the next run starts.
    NextRun,
    /// Takes the recommended answers for this step and the rest of the setup.
    Recommended,
    /// Closes and nothing changes. The TUI builds these, so the controller never sees one.
    Cancel,
}

pub(crate) fn on_set_aside(kind: ChoiceKind) -> OnSetAside {
    match kind {
        ChoiceKind::StartWork => OnSetAside::SetAside,
        ChoiceKind::ConfirmTakeOver | ChoiceKind::ConfirmRevert => OnSetAside::Cancel,
        ChoiceKind::SettingScope | ChoiceKind::ConfirmSafetySetting => OnSetAside::NotChanged,
        ChoiceKind::TrustProject | ChoiceKind::ConfirmConnect => OnSetAside::NotNow,
        ChoiceKind::DetectedGates => OnSetAside::NextRun,
        ChoiceKind::TermsNote | ChoiceKind::Init => OnSetAside::Recommended,
    }
}

/// The fixed options of each kind, in order. `SettingScope` recommends the command's default
/// through [`scope_options`], and `Init` steps build theirs with [`init_options`].
pub(crate) fn options(kind: ChoiceKind) -> Vec<AnswerOption> {
    match kind {
        ChoiceKind::StartWork => vec![
            option('s', "Solo", choose("solo"), false),
            option('c', "Crew run", choose("crew"), false),
            option('n', "No, just asking", choose("no"), false),
        ],
        ChoiceKind::ConfirmTakeOver => vec![option('t', "Take over", AnswerAction::TakeOver, true)],
        ChoiceKind::SettingScope => scope_options(SettingScope::ProjectLocal),
        ChoiceKind::ConfirmSafetySetting => vec![
            option('y', "Change it", AnswerAction::Approve, false),
            option('n', "Keep it as it is", AnswerAction::Cancel, false),
        ],
        ChoiceKind::TrustProject => vec![
            option('y', "Trust them", AnswerAction::Approve, false),
            option('n', "Not now", AnswerAction::Cancel, false),
            option('d', "Show the file", AnswerAction::ShowDiff, false),
        ],
        ChoiceKind::DetectedGates => vec![
            option('y', "Use these", AnswerAction::Approve, true),
            option('e', "Edit them", AnswerAction::ApproveWithEdits, false),
            option('n', "Skip", AnswerAction::Cancel, false),
        ],
        ChoiceKind::TermsNote => vec![option('c', "Continue", AnswerAction::Approve, true)],
        ChoiceKind::ConfirmRevert => vec![
            option('y', "Revert", AnswerAction::Approve, false),
            option('d', "Show the diff", AnswerAction::ShowDiff, false),
        ],
        ChoiceKind::ConfirmConnect => vec![
            option('y', "Write it", AnswerAction::Approve, false),
            option('n', "Not now", AnswerAction::Cancel, false),
        ],
        ChoiceKind::Init => Vec::new(),
    }
}

/// Just this session `s`, This project, just me `m`, This project, shared `p`, Everywhere `e`.
pub(crate) fn scope_options(recommended: SettingScope) -> Vec<AnswerOption> {
    [
        ('s', SettingScope::Session),
        ('m', SettingScope::ProjectLocal),
        ('p', SettingScope::Project),
        ('e', SettingScope::Global),
    ]
    .into_iter()
    .map(|(key, scope)| {
        option(
            key,
            scope_label(scope),
            choose(scope_value(scope)),
            scope == recommended,
        )
    })
    .collect()
}

pub(crate) fn scope_label(scope: SettingScope) -> &'static str {
    match scope {
        SettingScope::Session => "Just this session",
        SettingScope::ProjectLocal => "This project, just me",
        SettingScope::Project => "This project, shared",
        SettingScope::Global => "Everywhere",
    }
}

/// The `Choose` value for a scope, its serde name.
pub(crate) fn scope_value(scope: SettingScope) -> &'static str {
    match scope {
        SettingScope::Session => "session",
        SettingScope::ProjectLocal => "project_local",
        SettingScope::Project => "project",
        SettingScope::Global => "global",
    }
}

pub(crate) fn scope_from_value(value: &str) -> Option<SettingScope> {
    [
        SettingScope::Session,
        SettingScope::ProjectLocal,
        SettingScope::Project,
        SettingScope::Global,
    ]
    .into_iter()
    .find(|scope| scope_value(*scope) == value)
}

/// One answer of a guided-setup step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InitAnswer {
    pub label: String,
    /// What `Choose` carries back.
    pub value: String,
    pub detail: Option<String>,
}

/// `a`, `b`, `c` … in order, the first recommended. A step with nothing to choose gets
/// Continue `c`.
pub(crate) fn init_options(answers: &[InitAnswer]) -> Vec<AnswerOption> {
    if answers.is_empty() {
        return options(ChoiceKind::TermsNote);
    }
    answers
        .iter()
        .zip('a'..='z')
        .enumerate()
        .map(|(index, (answer, key))| AnswerOption {
            key,
            label: answer.label.clone(),
            action: choose(&answer.value),
            recommended: index == 0,
            detail: answer.detail.clone(),
        })
        .collect()
}

pub(crate) fn app_choice(
    id: ChoiceId,
    kind: ChoiceKind,
    title: impl Into<String>,
    body: Vec<String>,
    options: Vec<AnswerOption>,
) -> AppChoice {
    let free_text = match kind {
        ChoiceKind::DetectedGates => FreeText::Edits,
        _ => FreeText::None,
    };
    AppChoice {
        id,
        kind,
        title: title.into(),
        body,
        options,
        free_text,
        bell: false,
    }
}

pub(crate) fn option(
    key: char,
    label: &str,
    action: AnswerAction,
    recommended: bool,
) -> AnswerOption {
    AnswerOption {
        key,
        label: label.to_owned(),
        action,
        recommended,
        detail: None,
    }
}

pub(crate) fn choose(value: &str) -> AnswerAction {
    AnswerAction::Choose {
        value: value.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCOPES: [SettingScope; 4] = [
        SettingScope::Session,
        SettingScope::ProjectLocal,
        SettingScope::Project,
        SettingScope::Global,
    ];

    /// An option's key, label, and action.
    type Shape<'a> = (char, &'a str, AnswerAction);

    fn shape(options: &[AnswerOption]) -> Vec<Shape<'_>> {
        options
            .iter()
            .map(|option| (option.key, option.label.as_str(), option.action.clone()))
            .collect()
    }

    #[test]
    fn app_choice_options_follow_the_table() {
        use AnswerAction::{Approve, ApproveWithEdits, Cancel, ShowDiff, TakeOver};
        let table: Vec<(ChoiceKind, Vec<Shape>, OnSetAside)> = vec![
            (
                ChoiceKind::StartWork,
                vec![
                    ('s', "Solo", choose("solo")),
                    ('c', "Crew run", choose("crew")),
                    ('n', "No, just asking", choose("no")),
                ],
                OnSetAside::SetAside,
            ),
            (
                ChoiceKind::ConfirmTakeOver,
                vec![('t', "Take over", TakeOver)],
                OnSetAside::Cancel,
            ),
            (
                ChoiceKind::SettingScope,
                vec![
                    ('s', "Just this session", choose("session")),
                    ('m', "This project, just me", choose("project_local")),
                    ('p', "This project, shared", choose("project")),
                    ('e', "Everywhere", choose("global")),
                ],
                OnSetAside::NotChanged,
            ),
            (
                ChoiceKind::ConfirmSafetySetting,
                vec![
                    ('y', "Change it", Approve),
                    ('n', "Keep it as it is", Cancel),
                ],
                OnSetAside::NotChanged,
            ),
            (
                ChoiceKind::TrustProject,
                vec![
                    ('y', "Trust them", Approve),
                    ('n', "Not now", Cancel),
                    ('d', "Show the file", ShowDiff),
                ],
                OnSetAside::NotNow,
            ),
            (
                ChoiceKind::DetectedGates,
                vec![
                    ('y', "Use these", Approve),
                    ('e', "Edit them", ApproveWithEdits),
                    ('n', "Skip", Cancel),
                ],
                OnSetAside::NextRun,
            ),
            (
                ChoiceKind::TermsNote,
                vec![('c', "Continue", Approve)],
                OnSetAside::Recommended,
            ),
            (
                ChoiceKind::ConfirmRevert,
                vec![('y', "Revert", Approve), ('d', "Show the diff", ShowDiff)],
                OnSetAside::Cancel,
            ),
            (
                ChoiceKind::ConfirmConnect,
                vec![('y', "Write it", Approve), ('n', "Not now", Cancel)],
                OnSetAside::NotNow,
            ),
            (ChoiceKind::Init, Vec::new(), OnSetAside::Recommended),
        ];
        for (kind, expected, esc) in &table {
            assert_eq!(shape(&options(*kind)), *expected, "{kind:?}");
            assert_eq!(on_set_aside(*kind), *esc, "{kind:?}");
            let choice = app_choice(
                ChoiceId::numbered(1),
                *kind,
                "t",
                Vec::new(),
                options(*kind),
            );
            let free_text = match kind {
                ChoiceKind::DetectedGates => FreeText::Edits,
                _ => FreeText::None,
            };
            assert_eq!(choice.free_text, free_text, "{kind:?}");
        }
        // Every kind is in the table once.
        let kinds: std::collections::HashSet<ChoiceKind> =
            table.iter().map(|(kind, ..)| *kind).collect();
        assert_eq!(kinds.len(), table.len());
        for kind in kinds {
            match kind {
                ChoiceKind::StartWork
                | ChoiceKind::ConfirmTakeOver
                | ChoiceKind::SettingScope
                | ChoiceKind::ConfirmSafetySetting
                | ChoiceKind::TermsNote
                | ChoiceKind::Init
                | ChoiceKind::DetectedGates
                | ChoiceKind::TrustProject
                | ChoiceKind::ConfirmRevert
                | ChoiceKind::ConfirmConnect => {}
            }
        }

        // The scope question recommends the command's default, and only that.
        for scope in SCOPES {
            let options = scope_options(scope);
            let picked: Vec<AnswerAction> = options
                .iter()
                .filter(|option| option.recommended)
                .map(|option| option.action.clone())
                .collect();
            assert_eq!(picked, vec![choose(scope_value(scope))]);
            assert_eq!(scope_from_value(scope_value(scope)), Some(scope));
            let serde_name = serde_json::to_value(scope).unwrap();
            assert_eq!(serde_name, scope_value(scope));
        }
        assert_eq!(scope_from_value("somewhere"), None);

        // A setup step: a, b, c in order, the first recommended; nothing to pick is Continue.
        let answers = ["one", "two", "three"].map(|value| InitAnswer {
            label: value.to_owned(),
            value: value.to_owned(),
            detail: None,
        });
        let steps = init_options(&answers);
        let keys: Vec<(char, bool)> = steps
            .iter()
            .map(|option| (option.key, option.recommended))
            .collect();
        assert_eq!(keys, vec![('a', true), ('b', false), ('c', false)]);
        assert_eq!(steps[1].action, choose("two"));
        assert_eq!(shape(&init_options(&[])), vec![('c', "Continue", Approve)]);
    }
}
