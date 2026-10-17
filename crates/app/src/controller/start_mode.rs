//! The approval mode the interactive app starts in, as `approvals.start_mode` says. Shift+Tab's
//! pick is kept in this folder's state with the approvals it was made under, so `last` brings
//! it back at the next start, and an edit of the approvals in a settings file since then wins
//! over it. `argonauts run` and outside agents never read it.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::settings::approvals::{ApprovalMode, ApprovalSettings, StartMode};
use argonauts_core::sha256_hex;
use serde::{Deserialize, Serialize};

/// The file in a folder's state that holds Shift+Tab's last pick.
const FILE: &str = "approval-mode.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LastPick {
    mode: ApprovalMode,
    /// The approvals the pick was made under, as [`fingerprint`] writes them.
    approvals: String,
}

/// The approvals as the settings resolve them, the start mode aside, as a short fingerprint.
fn fingerprint(approvals: &ApprovalSettings) -> String {
    let mut approvals = approvals.clone();
    approvals.start_mode = StartMode::default();
    let text = serde_json::to_string(&approvals).unwrap_or_default();
    sha256_hex(text.as_bytes())
}

fn file(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE)
}

/// Keeps Shift+Tab's pick for the next start of the app in this folder.
pub(crate) fn remember(
    state_dir: &Path,
    mode: ApprovalMode,
    approvals: &ApprovalSettings,
) -> anyhow::Result<()> {
    let pick = LastPick {
        mode,
        approvals: fingerprint(approvals),
    };
    let path = file(state_dir);
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("couldn't make {}", state_dir.display()))?;
    let text = serde_json::to_string_pretty(&pick)?;
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, text)
        .with_context(|| format!("couldn't write {}", partial.display()))?;
    std::fs::rename(&partial, &path)
        .with_context(|| format!("couldn't write {}", path.display()))?;
    Ok(())
}

/// The mode to start in when it isn't the one the approvals give: the one the setting names,
/// or with `last` Shift+Tab's last pick while the approvals are still the ones it was made
/// under. `None` starts as the approvals say.
pub(crate) fn at_start(state_dir: &Path, approvals: &ApprovalSettings) -> Option<ApprovalMode> {
    match approvals.start_mode {
        StartMode::Approvals => None,
        StartMode::Last => {
            let text = std::fs::read_to_string(file(state_dir)).ok()?;
            let pick: LastPick = serde_json::from_str(&text).ok()?;
            (pick.approvals == fingerprint(approvals)).then_some(pick.mode)
        }
        named => named.named(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approvals(start_mode: StartMode) -> ApprovalSettings {
        ApprovalSettings {
            start_mode,
            ..ApprovalSettings::default()
        }
    }

    #[test]
    fn last_brings_back_the_last_pick_for_each_mode() {
        let dir = tempfile::tempdir().unwrap();
        let settings = approvals(StartMode::Last);
        assert_eq!(at_start(dir.path(), &settings), None, "nothing picked yet");
        for mode in [
            ApprovalMode::Supervised,
            ApprovalMode::AutoPreview,
            ApprovalMode::Auto,
            ApprovalMode::Autopilot,
        ] {
            remember(dir.path(), mode, &settings).unwrap();
            assert_eq!(at_start(dir.path(), &settings), Some(mode));
        }
    }

    #[test]
    fn an_edit_of_the_approvals_after_the_pick_wins() {
        let dir = tempfile::tempdir().unwrap();
        let settings = approvals(StartMode::Last);
        remember(dir.path(), ApprovalMode::Autopilot, &settings).unwrap();
        let edited = ApprovalSettings {
            autopilot: true,
            ..settings.clone()
        };
        assert_eq!(at_start(dir.path(), &edited), None);
        // The start mode itself is no edit of the approvals.
        let named = approvals(StartMode::Approvals);
        assert_eq!(at_start(dir.path(), &named), None);
        let back = approvals(StartMode::Last);
        assert_eq!(at_start(dir.path(), &back), Some(ApprovalMode::Autopilot));
    }

    #[test]
    fn a_named_mode_starts_whatever_was_picked_last() {
        let dir = tempfile::tempdir().unwrap();
        remember(
            dir.path(),
            ApprovalMode::Autopilot,
            &approvals(StartMode::Last),
        )
        .unwrap();
        for (start, mode) in [
            (StartMode::Supervised, ApprovalMode::Supervised),
            (StartMode::AutoPreview, ApprovalMode::AutoPreview),
            (StartMode::Auto, ApprovalMode::Auto),
            (StartMode::Autopilot, ApprovalMode::Autopilot),
        ] {
            assert_eq!(at_start(dir.path(), &approvals(start)), Some(mode));
        }
        assert_eq!(at_start(dir.path(), &approvals(StartMode::Approvals)), None);
    }

    /// The pick the start brought back holds until the guided setup writes its approvals
    /// step's answer, which is the mode from then on.
    #[tokio::test]
    async fn the_setups_answer_is_the_mode_once_it_is_written() {
        use crate::controller::runs::{self, RunsState};
        use crate::controller::test_support::{CliOptions, TestCtx};

        let (parts, _) = TestCtx::new(CliOptions::default());
        let state_dir = parts.ctx.env.state_dir.clone();
        remember(
            &state_dir,
            ApprovalMode::Autopilot,
            &parts.ctx.settings().approvals,
        )
        .unwrap();
        let mut runs = RunsState::new(&parts.ctx);
        assert_eq!(
            runs::status_parts(&runs).approval_mode,
            Some(ApprovalMode::Autopilot),
            "the start brings the last pick back"
        );
        runs::setup_answered(&mut runs, ApprovalMode::Supervised);
        assert_eq!(
            runs::status_parts(&runs).approval_mode,
            Some(ApprovalMode::Supervised)
        );
    }

    /// Autopilot coming back by itself says so once at start, in its own line with no mark.
    /// Supervised and auto come back quietly, as the bottom line shows them.
    #[tokio::test]
    async fn only_autopilot_coming_back_says_so_at_start() {
        use argonauts_core::app::AppEvent;
        use argonauts_core::log::NoteLevel;
        use argonauts_core::settings::approvals::AUTOPILOT_BACK;

        use crate::controller::Controller;
        use crate::controller::test_support::{CliOptions, TestCtx};

        for (mode, says) in [
            (ApprovalMode::Autopilot, true),
            (ApprovalMode::Auto, false),
            (ApprovalMode::Supervised, false),
        ] {
            let (mut parts, _) = TestCtx::new(CliOptions::default());
            let approvals = parts.ctx.settings().approvals.clone();
            remember(&parts.ctx.env.state_dir, mode, &approvals).unwrap();
            let _controller = Controller::new(parts.ctx, parts.internal).unwrap();
            let mut said = Vec::new();
            while let Ok(event) = parts.events.try_recv() {
                if let AppEvent::Notice { level, text } = event {
                    said.push((level, text));
                }
            }
            let back = (NoteLevel::Caution, AUTOPILOT_BACK.to_owned());
            assert_eq!(said.contains(&back), says, "{mode:?}: {said:?}");
            let mentions = said.iter().filter(|(_, text)| text == AUTOPILOT_BACK);
            assert_eq!(mentions.count(), usize::from(says), "{mode:?}: {said:?}");
        }
    }

    #[test]
    fn a_broken_file_starts_as_the_approvals_say() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), "not json").unwrap();
        assert_eq!(at_start(dir.path(), &approvals(StartMode::Last)), None);
    }
}
