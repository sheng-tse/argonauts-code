//! Per-launch settings that Claude Code takes as flags or environment variables.

use argonauts_core::LaunchTweak;
use argonauts_core::settings::sessions::{Compaction, FanoutSwitch, NativeFanout};

/// Outranks `--autocompact`, so an explicit `auto` removes it from the child's environment,
/// along with the switches that would turn compaction off.
const COMPACTION_OVERRIDES: &[&str] = &[
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
    "DISABLE_AUTO_COMPACT",
    "DISABLE_COMPACT",
];

/// The built-in tool that starts a subagent.
pub const SUBAGENT_TOOL: &str = "Agent";

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

/// The session's settings say `disableAllHooks`, which also stops installed plugins' mods. No
/// flag does it: `--safe-mode` also drops CLAUDE.md, skills, and workflows, and `--bare` never
/// reads your login.
pub fn user_hooks_off() -> LaunchTweak {
    LaunchTweak::default()
}
