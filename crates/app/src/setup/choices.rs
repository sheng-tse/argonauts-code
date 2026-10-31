//! The app choices' table in one place: each kind's options, keys, and actions, and what Esc
//! (`SetAside`) means for it.

use argonauts_core::app::{AppChoice, ChoiceKind};
use argonauts_core::base::names::ChoiceId;
use argonauts_core::runs::events::approval::{AnswerAction, AnswerOption, FreeText};

pub(crate) use argonauts_core::app::NOT_CHANGED;
pub(crate) const JUST_ASKING: &str = "Start work on it? · no, just asking";

/// What Esc on an app choice does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnSetAside {
    /// Stays waiting, like a run's question.
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
    /// Closes with "Just this request.": what the request asked holds for its run alone.
    JustThisRequest,
    /// Answers No.
    No,
    /// Closes saying nothing ran: a dry run was all it showed.
    NothingRan,
    /// Closes, and the run that waited on the answer doesn't start.
    NotStarted,
}

pub(crate) fn on_set_aside(kind: ChoiceKind) -> OnSetAside {
    match kind {
        ChoiceKind::StartWork | ChoiceKind::RefusedCommands => OnSetAside::SetAside,
        ChoiceKind::Permission | ChoiceKind::GoAhead | ChoiceKind::Fallback => OnSetAside::No,
        ChoiceKind::RewindTurn | ChoiceKind::Git | ChoiceKind::Templates => OnSetAside::NotChanged,
        ChoiceKind::TryWorkflow => OnSetAside::NothingRan,
        ChoiceKind::ConfirmTakeOver | ChoiceKind::ConfirmRewind => OnSetAside::Cancel,
        ChoiceKind::SettingScope | ChoiceKind::ConfirmSafetySetting => OnSetAside::NotChanged,
        ChoiceKind::TrustProject | ChoiceKind::ConfirmConnect => OnSetAside::NotNow,
        ChoiceKind::DetectedChecks => OnSetAside::NextRun,
        ChoiceKind::TermsNote | ChoiceKind::Init => OnSetAside::Recommended,
        ChoiceKind::KeepInstructions => OnSetAside::JustThisRequest,
        ChoiceKind::Setup => OnSetAside::NotChanged,
        ChoiceKind::NeededUpdate => OnSetAside::NotStarted,
        ChoiceKind::RenamedSettings => OnSetAside::NotChanged,
    }
}

/// The fixed options of each kind, in order. `Init` steps build theirs with [`init_options`].
pub(crate) fn options(kind: ChoiceKind) -> Vec<AnswerOption> {
    match kind {
        ChoiceKind::StartWork => vec![
            option('s', "Captain", choose("captain"), false),
            option('c', "Crew run", choose("crew"), false),
            option('n', "No, just asking", choose("no"), false),
        ],
        ChoiceKind::ConfirmTakeOver => vec![option('t', "Take over", AnswerAction::TakeOver, true)],
        ChoiceKind::ConfirmSafetySetting => vec![
            option('y', "Change it", AnswerAction::Approve, false),
            option('n', "Keep it as it is", AnswerAction::Cancel, false),
        ],
        ChoiceKind::TrustProject => vec![
            option('y', "Trust them", AnswerAction::Approve, false),
            option('n', "Not now", AnswerAction::Cancel, false),
            option('d', "Show the file", AnswerAction::ShowDiff, false),
        ],
        ChoiceKind::DetectedChecks => vec![
            option('y', "Use these", AnswerAction::Approve, true),
            option('e', "Edit them", AnswerAction::ApproveWithEdits, false),
            option('n', "Skip", AnswerAction::Cancel, false),
        ],
        ChoiceKind::TermsNote => vec![option('c', "Continue", AnswerAction::Approve, true)],
        ChoiceKind::ConfirmRewind => vec![
            option('y', "Rewind", AnswerAction::Approve, false),
            option('d', "Show the diff", AnswerAction::ShowDiff, false),
        ],
        ChoiceKind::ConfirmConnect => vec![
            option('y', "Write it", AnswerAction::Approve, false),
            option('n', "Not now", AnswerAction::Cancel, false),
        ],
        ChoiceKind::RefusedCommands => vec![
            option('y', "Yes, and continue", choose("continue"), true),
            option('n', "No", choose("no"), false),
        ],
        ChoiceKind::GoAhead => vec![
            option('y', "Yes, go ahead", choose("yes"), true),
            option('n', "No, keep planning", choose("no"), false),
        ],
        ChoiceKind::RenamedSettings => vec![
            option('r', "Rename them", AnswerAction::Approve, false),
            option('l', "Leave them", AnswerAction::Cancel, false),
        ],
        // A preview's answer is No unless you say otherwise.
        ChoiceKind::TryWorkflow => vec![
            option('y', "Yes, run it", AnswerAction::Approve, false),
            option('n', "No", choose("no"), true),
        ],
        // Built for each question: they name the CLI, the command, and the turns.
        ChoiceKind::Init
        | ChoiceKind::Permission
        | ChoiceKind::RewindTurn
        | ChoiceKind::Fallback
        | ChoiceKind::Setup
        | ChoiceKind::Git
        | ChoiceKind::Templates
        | ChoiceKind::NeededUpdate => Vec::new(),
        // Nothing asks where to save any more; a recorded one still reads.
        ChoiceKind::SettingScope | ChoiceKind::KeepInstructions => Vec::new(),
    }
}

/// The `Choose` value of a dry run's Edit it.
pub(crate) const EDIT_IT: &str = "edit";

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
        ChoiceKind::DetectedChecks => FreeText::Edits,
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
                    ('s', "Captain", choose("captain")),
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
            (ChoiceKind::SettingScope, Vec::new(), OnSetAside::NotChanged),
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
                ChoiceKind::DetectedChecks,
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
                ChoiceKind::ConfirmRewind,
                vec![('y', "Rewind", Approve), ('d', "Show the diff", ShowDiff)],
                OnSetAside::Cancel,
            ),
            (
                ChoiceKind::ConfirmConnect,
                vec![('y', "Write it", Approve), ('n', "Not now", Cancel)],
                OnSetAside::NotNow,
            ),
            (ChoiceKind::Init, Vec::new(), OnSetAside::Recommended),
            (
                ChoiceKind::KeepInstructions,
                Vec::new(),
                OnSetAside::JustThisRequest,
            ),
            (ChoiceKind::Permission, Vec::new(), OnSetAside::No),
            (
                ChoiceKind::RefusedCommands,
                vec![
                    ('y', "Yes, and continue", choose("continue")),
                    ('n', "No", choose("no")),
                ],
                OnSetAside::SetAside,
            ),
            (ChoiceKind::RewindTurn, Vec::new(), OnSetAside::NotChanged),
            (
                ChoiceKind::GoAhead,
                vec![
                    ('y', "Yes, go ahead", choose("yes")),
                    ('n', "No, keep planning", choose("no")),
                ],
                OnSetAside::No,
            ),
            (ChoiceKind::Fallback, Vec::new(), OnSetAside::No),
            (
                ChoiceKind::TryWorkflow,
                vec![('y', "Yes, run it", Approve), ('n', "No", choose("no"))],
                OnSetAside::NothingRan,
            ),
            (ChoiceKind::Setup, Vec::new(), OnSetAside::NotChanged),
            (ChoiceKind::Git, Vec::new(), OnSetAside::NotChanged),
            (ChoiceKind::Templates, Vec::new(), OnSetAside::NotChanged),
            (ChoiceKind::NeededUpdate, Vec::new(), OnSetAside::NotStarted),
            (
                ChoiceKind::RenamedSettings,
                vec![
                    ('r', "Rename them", AnswerAction::Approve),
                    ('l', "Leave them", AnswerAction::Cancel),
                ],
                OnSetAside::NotChanged,
            ),
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
                ChoiceKind::DetectedChecks => FreeText::Edits,
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
                | ChoiceKind::DetectedChecks
                | ChoiceKind::TrustProject
                | ChoiceKind::ConfirmRewind
                | ChoiceKind::ConfirmConnect
                | ChoiceKind::KeepInstructions
                | ChoiceKind::Permission
                | ChoiceKind::RefusedCommands
                | ChoiceKind::RewindTurn
                | ChoiceKind::GoAhead
                | ChoiceKind::Fallback
                | ChoiceKind::TryWorkflow
                | ChoiceKind::Setup
                | ChoiceKind::Git
                | ChoiceKind::Templates
                | ChoiceKind::NeededUpdate
                | ChoiceKind::RenamedSettings => {}
            }
        }

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
