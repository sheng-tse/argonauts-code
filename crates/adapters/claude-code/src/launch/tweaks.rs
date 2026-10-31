//! Per-launch settings that Claude Code takes as flags or environment variables.

use std::borrow::Cow;

use argonauts_core::settings::context::ContextAsk;
use argonauts_core::settings::sessions::{Compaction, FanoutSwitch, NativeFanout};
use argonauts_core::{AuthMode, LaunchTweak};

/// Outranks `--autocompact`, so an explicit `auto` removes it from the child's environment,
/// along with the switches that would turn compaction off.
const COMPACTION_OVERRIDES: &[&str] = &[
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
    "DISABLE_AUTO_COMPACT",
    "DISABLE_COMPACT",
];

/// The built-in tool that starts a subagent.
pub const SUBAGENT_TOOL: &str = "Agent";
/// The tool Claude Code starts a workflow of its own with (2.1.289 and later).
pub const WORKFLOW_TOOL: &str = "Workflow";

/// Claude Code's largest window, which a model runs with when its name ends in [`ONE_M`].
const ONE_M_TOKENS: u64 = 1_000_000;
const ONE_M: &str = "[1m]";

/// The model as `--model` takes it for the context the session asks for. Claude Code runs
/// Sonnet and Opus with a 1M window under the names `sonnet[1m]` and `opus[1m]`, and their
/// full ids take the same suffix. Under an API key its own aliases get it whenever the
/// largest window is asked for, and a full id only when the registry says the model has
/// that window, since an older model's name with the suffix would be refused.
///
/// On a plan the name is left alone. A `[1m]` name can bill usage credits there without
/// asking when it runs unattended, and Opus 5.5 and Sonnet 5.5 have the window without it.
/// A name that already says its window is left as you wrote it, which is how you ask for
/// an older model's 1M window on a plan.
pub fn model_for(model: &str, ask: ContextAsk, auth: AuthMode) -> Cow<'_, str> {
    if auth == AuthMode::Subscription {
        return Cow::Borrowed(model);
    }
    let alias = matches!(model, "sonnet" | "opus");
    let full_id = model.starts_with("claude-sonnet-") || model.starts_with("claude-opus-");
    let wanted = match ask {
        ContextAsk::AsItIs => false,
        ContextAsk::Largest { known: None } => alias,
        ContextAsk::Largest {
            known: Some(tokens),
        }
        | ContextAsk::Held { tokens } => (alias || full_id) && tokens >= ONE_M_TOKENS,
    };
    if wanted && !model.contains('[') {
        Cow::Owned(format!("{model}{ONE_M}"))
    } else {
        Cow::Borrowed(model)
    }
}

pub fn compaction(setting: Compaction) -> LaunchTweak {
    match setting {
        Compaction::Inherit => LaunchTweak::default(),
        // 2.1.221 and later. It sets the window for this launch without touching your settings.
        Compaction::Auto => LaunchTweak {
            args: vec!["--autocompact".into(), "auto".into()],
            ..LaunchTweak::default()
        },
        // `--autocompact` can't turn compaction off. This keeps `/compact`, which nobody sends
        // under `-p`.
        Compaction::Off => LaunchTweak {
            env_set: vec![("DISABLE_AUTO_COMPACT".into(), "1".into())],
            ..LaunchTweak::default()
        },
    }
}

pub fn compaction_env_remove(setting: Compaction) -> &'static [&'static str] {
    match setting {
        Compaction::Auto => COMPACTION_OVERRIDES,
        Compaction::Inherit | Compaction::Off => &[],
    }
}

/// How a session may start subagents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fanout {
    /// Nothing was asked, so the tool set stays as it was before fan-out settings existed.
    Unset,
    Allowed,
    Capped(u32),
    Off,
}

impl Fanout {
    pub fn from_setting(setting: Option<NativeFanout>) -> Self {
        match setting {
            None => Self::Unset,
            Some(NativeFanout::Switch(FanoutSwitch::On)) => Self::Allowed,
            Some(NativeFanout::Switch(FanoutSwitch::Off) | NativeFanout::Limit { max: 0 }) => {
                Self::Off
            }
            Some(NativeFanout::Limit { max }) => Self::Capped(max),
        }
    }

    /// Whether the subagent tool joins a build session's `--tools`.
    pub fn offers_subagents(self) -> bool {
        matches!(self, Self::Allowed | Self::Capped(_))
    }

    /// Whether Claude Code's own workflow tool joins a build session's `--tools`: only when it
    /// may fan out as it sees fit. A workflow starts agents of its own, which no cap on
    /// subagents covers.
    pub fn offers_workflows(self) -> bool {
        self == Self::Allowed
    }
}

pub fn native_fanout(setting: NativeFanout) -> LaunchTweak {
    match Fanout::from_setting(Some(setting)) {
        Fanout::Unset | Fanout::Allowed => LaunchTweak::default(),
        Fanout::Capped(max) => LaunchTweak {
            // One level only, so the concurrency cap (2.1.217 and later) covers every subagent
            // whether Claude Code counts nested ones per parent or per session.
            env_set: vec![
                (
                    "CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS".into(),
                    max.to_string(),
                ),
                ("CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH".into(), "1".into()),
            ],
            ..LaunchTweak::default()
        },
        Fanout::Off => LaunchTweak {
            args: vec!["--disallowedTools".into(), SUBAGENT_TOOL.into()],
            // Dynamic workflows start agents of their own.
            env_set: vec![("CLAUDE_CODE_DISABLE_WORKFLOWS".into(), "1".into())],
            ..LaunchTweak::default()
        },
    }
}

/// The session's settings say `disableAllHooks`, which also stops installed plugins' hooks. No
/// flag does it: `--safe-mode` also drops CLAUDE.md, skills, and workflows, and `--bare` never
/// reads your login.
pub fn user_hooks_off() -> LaunchTweak {
    LaunchTweak::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sonnet_and_opus_take_the_1m_suffix_when_the_largest_window_is_asked_for() {
        let unlisted = ContextAsk::Largest { known: None };
        let listed_1m = ContextAsk::Largest {
            known: Some(1_000_000),
        };
        let listed_200k = ContextAsk::Largest {
            known: Some(200_000),
        };
        let model_for = |model, ask| model_for(model, ask, AuthMode::ApiKey);
        // Claude Code's own aliases, whatever the registry lists.
        assert_eq!(model_for("sonnet", unlisted), "sonnet[1m]");
        assert_eq!(model_for("opus", listed_1m), "opus[1m]");
        // A full id only when its window is known to be that large.
        assert_eq!(
            model_for("claude-sonnet-5-5", listed_1m),
            "claude-sonnet-5-5[1m]"
        );
        assert_eq!(model_for("claude-opus-4-1", unlisted), "claude-opus-4-1");
        assert_eq!(
            model_for("claude-haiku-4-5", listed_200k),
            "claude-haiku-4-5"
        );
        // Other model lines have no such name, and a name that says its window is yours.
        assert_eq!(model_for("haiku", unlisted), "haiku");
        assert_eq!(model_for("claude-fable-5-1", listed_1m), "claude-fable-5-1");
        assert_eq!(model_for("sonnet[1m]", listed_1m), "sonnet[1m]");
    }

    /// A `[1m]` name can bill usage credits on a plan without asking, so there only a name
    /// you wrote that way runs so.
    #[test]
    fn on_a_plan_the_name_is_left_as_you_wrote_it() {
        let largest = ContextAsk::Largest {
            known: Some(1_000_000),
        };
        let on_a_plan = |model| model_for(model, largest, AuthMode::Subscription);
        assert_eq!(on_a_plan("sonnet"), "sonnet");
        assert_eq!(on_a_plan("opus"), "opus");
        assert_eq!(on_a_plan("claude-opus-5-5"), "claude-opus-5-5");
        assert_eq!(on_a_plan("claude-sonnet-4-6[1m]"), "claude-sonnet-4-6[1m]");
    }

    #[test]
    fn a_lowered_context_or_none_asked_leaves_the_name_alone() {
        let model_for = |model, ask| model_for(model, ask, AuthMode::ApiKey);
        let lowered = ContextAsk::Held { tokens: 200_000 };
        assert_eq!(model_for("sonnet", lowered), "sonnet");
        assert_eq!(model_for("claude-opus-5-5", lowered), "claude-opus-5-5");
        assert_eq!(model_for("opus", ContextAsk::AsItIs), "opus");
        // A cap at or above the 1M window still asks for it.
        let roomy = ContextAsk::Held { tokens: 1_000_000 };
        assert_eq!(model_for("opus", roomy), "opus[1m]");
    }
}
