//! Per role on one CLI: the effective compaction and fan-out cap with their effects, and a
//! warning for every role with lifetime `run` or `project` whose compaction is off.

use argonauts_core::names::RoleName;
use argonauts_core::settings::Settings;
use argonauts_core::settings::sessions::{Compaction, FanoutSwitch, NativeFanout, SessionLifetime};
use argonauts_core::{Adapter, TweakEffect};
use serde::Serialize;

/// Claude Code reads it from its environment and then never compacts on its own.
const CLAUDE_DISABLE_COMPACT: &str = "DISABLE_AUTO_COMPACT";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoleCheck {
    pub role: RoleName,
    pub lifetime: SessionLifetime,
    /// Such as `inherit → off, DISABLE_AUTO_COMPACT=1 in your environment`.
    pub compaction: String,
    pub compaction_effect: TweakEffect,
    /// Whether this role's sessions run without compaction, so a full window means a rotation.
    pub compaction_off: bool,
    /// `on`, `off`, or `max N`.
    pub fanout: String,
    pub fanout_effect: TweakEffect,
    pub warning: Option<String>,
}

/// The roles `adapter` may fill under `settings`, read against your own environment.
pub fn role_checks(adapter: &dyn Adapter, settings: &Settings) -> Vec<RoleCheck> {
    role_checks_in(adapter, settings, &|name| std::env::var(name).ok())
}

/// Like [`role_checks`], with `env` in place of the environment sessions inherit.
pub fn role_checks_in(
    adapter: &dyn Adapter,
    settings: &Settings,
    env: &dyn Fn(&str) -> Option<String>,
) -> Vec<RoleCheck> {
    roles_filled_by(adapter.id(), settings)
        .into_iter()
        .map(|role| role_check(adapter, settings, role, env))
        .collect()
}

/// The default role when it's this CLI, every configured role it may fill, and `worker`
/// for a crew member, since workers come from the crew unless a role says otherwise.
fn roles_filled_by(adapter_id: &str, settings: &Settings) -> Vec<RoleName> {
    let members = settings.crew.members();
    let in_crew = members.is_empty() || members.iter().any(|w| w.agent == adapter_id);
    let fills = |role: RoleName| match settings.roles.get(&role) {
        Some(config) if !config.models.is_empty() => {
            config.models.iter().any(|w| w.agent == adapter_id)
        }
        _ if role == RoleName::Main => settings.crew.main.agent == adapter_id,
        _ => in_crew,
    };
    let mut roles: Vec<RoleName> = settings.roles.keys().copied().collect();
    for always in [RoleName::Main, RoleName::Worker] {
        if !roles.contains(&always) {
            roles.push(always);
        }
    }
    roles.sort();
    roles.retain(|role| fills(*role));
    roles
}

fn role_check(
    adapter: &dyn Adapter,
    settings: &Settings,
    role: RoleName,
    env: &dyn Fn(&str) -> Option<String>,
) -> RoleCheck {
    let config = settings.roles.get(&role);
    let lifetime = config
        .and_then(|c| c.lifetime)
        .unwrap_or(settings.sessions.lifetime);
    let compaction = config
        .and_then(|c| c.compaction)
        .unwrap_or(settings.compaction);
    let fanout = config
        .and_then(|c| c.native_fanout)
        .unwrap_or(settings.native_fanout);

    let (compaction_label, compaction_effect, compaction_off) =
        effective_compaction(adapter, compaction, env);
    let warning = match lifetime {
        SessionLifetime::Task => None,
        SessionLifetime::Run | SessionLifetime::Project | SessionLifetime::Seat
            if !compaction_off =>
        {
            None
        }
        SessionLifetime::Run => Some(format!(
            "{} keeps one session per run (lifetime run), but its compaction is off on {}.",
            role.as_str(),
            adapter.id()
        )),
        SessionLifetime::Project => Some(format!(
            "{} keeps one session per project (lifetime project), but its compaction is off on {}.",
            role.as_str(),
            adapter.id()
        )),
        SessionLifetime::Seat => Some(format!(
            "{} keeps one session per council seat (lifetime seat), but its compaction is off on {}.",
            role.as_str(),
            adapter.id()
        )),
    };
    RoleCheck {
        role,
        lifetime,
        compaction: compaction_label,
        compaction_effect,
        compaction_off,
        fanout: fanout_label(fanout),
        fanout_effect: adapter.native_fanout(fanout).effect,
        warning,
    }
}

/// The label, the adapter's effect, and whether sessions end up without compaction.
fn effective_compaction(
    adapter: &dyn Adapter,
    setting: Compaction,
    env: &dyn Fn(&str) -> Option<String>,
) -> (String, TweakEffect, bool) {
    let tweak = adapter.compaction(setting);
    let effect = tweak.effect.clone();
    match setting {
        Compaction::Inherit => {
            let (label, off) = inherited(adapter.id(), env);
            (label, effect, off)
        }
        Compaction::Auto => match &effect {
            TweakEffect::NotPossible { note } => {
                (format!("auto, not controllable: {note}"), effect, false)
            }
            TweakEffect::Approximated { note } => {
                (format!("auto, approximated: {note}"), effect, false)
            }
            _ => ("auto".to_owned(), effect, false),
        },
        Compaction::Off => match &effect {
            TweakEffect::Approximated { note } => {
                (format!("off →  approximated: {note}"), effect, false)
            }
            TweakEffect::NotPossible { note } => {
                (format!("off →  not possible: {note}"), effect, false)
            }
            _ => ("off".to_owned(), effect, true),
        },
    }
}

/// What a CLI does when Argonauts Code passes nothing.
fn inherited(adapter_id: &str, env: &dyn Fn(&str) -> Option<String>) -> (String, bool) {
    match adapter_id {
        "claude-code" if env(CLAUDE_DISABLE_COMPACT).is_some_and(|v| truthy(&v)) => (
            format!("inherit →  off, {CLAUDE_DISABLE_COMPACT}=1 in your environment"),
            true,
        ),
        "claude-code" => (
            "inherit →  auto, from your Claude Code settings".to_owned(),
            false,
        ),
        "codex" => (
            "inherit →  auto, the model's default threshold".to_owned(),
            false,
        ),
        "antigravity-cli" => (
            "inherit →  auto, agy has no setting for it".to_owned(),
            false,
        ),
        _ => ("inherit, the CLI's own setting".to_owned(), false),
    }
}

fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn fanout_label(fanout: NativeFanout) -> String {
    match fanout {
        NativeFanout::Switch(FanoutSwitch::On) => "on".to_owned(),
        NativeFanout::Switch(FanoutSwitch::Off) => "off".to_owned(),
        NativeFanout::Limit { max } => format!("max {max}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::tests::FakeCli;
    use argonauts_core::LaunchTweak;
    use argonauts_core::names::ModelRef;
    use argonauts_core::settings::roles::RoleSettings;

    fn role(workers: &[&str], lifetime: SessionLifetime, compaction: Compaction) -> RoleSettings {
        RoleSettings {
            models: workers.iter().map(|w| ModelRef::new(*w, None)).collect(),
            lifetime: Some(lifetime),
            compaction: Some(compaction),
            ..RoleSettings::default()
        }
    }

    fn check(checks: &[RoleCheck], role: RoleName) -> &RoleCheck {
        checks
            .iter()
            .find(|c| c.role == role)
            .unwrap_or_else(|| panic!("no {role:?} in {checks:?}"))
    }

    #[test]
    fn doctor_reports_effective_compaction() {
        let claude = FakeCli::named("claude-code").with_compaction(|setting| match setting {
            Compaction::Off => LaunchTweak {
                env_set: vec![("DISABLE_AUTO_COMPACT".into(), "1".into())],
                ..LaunchTweak::default()
            },
            _ => LaunchTweak::default(),
        });
        let codex = FakeCli::named("codex").with_compaction(|setting| match setting {
            Compaction::Off => LaunchTweak {
                effect: TweakEffect::Approximated {
                    note: "compacts by 90% of the window at the latest".into(),
                },
                ..LaunchTweak::default()
            },
            _ => LaunchTweak::default(),
        });
        let mut settings = Settings::default();
        settings.roles.insert(
            RoleName::Consultant,
            role(
                &["claude-code", "codex"],
                SessionLifetime::Project,
                Compaction::Off,
            ),
        );
        settings.roles.insert(
            RoleName::Reviewer,
            role(&["claude-code"], SessionLifetime::Task, Compaction::Off),
        );
        settings.roles.insert(
            RoleName::Narrator,
            role(&["claude-code"], SessionLifetime::Run, Compaction::Inherit),
        );
        settings.native_fanout = NativeFanout::Limit { max: 2 };

        let quiet = |_: &str| None;
        let claude_checks = role_checks_in(&claude, &settings, &quiet);
        let consultant = check(&claude_checks, RoleName::Consultant);
        assert_eq!(consultant.compaction, "off");
        assert!(consultant.compaction_off);
        assert_eq!(
            consultant.warning.as_deref(),
            Some(
                "consultant keeps one session per project (lifetime project), but its compaction is off on claude-code."
            )
        );
        assert_eq!(consultant.fanout, "max 2");
        assert_eq!(consultant.fanout_effect, TweakEffect::EnforcedByConductor);
        // Off is fine for a session that ends with its task.
        let reviewer = check(&claude_checks, RoleName::Reviewer);
        assert!(reviewer.compaction_off);
        assert_eq!(reviewer.warning, None);
        let narrator = check(&claude_checks, RoleName::Narrator);
        assert_eq!(
            narrator.compaction,
            "inherit →  auto, from your Claude Code settings"
        );
        assert_eq!(narrator.warning, None);
        // claude-code is the main agent and in the default crew.
        assert!(claude_checks.iter().any(|c| c.role == RoleName::Main));
        assert!(claude_checks.iter().any(|c| c.role == RoleName::Worker));

        // Your own environment can turn compaction off for a role that inherits.
        let disabled = |name: &str| (name == "DISABLE_AUTO_COMPACT").then(|| "1".to_owned());
        let claude_checks = role_checks_in(&claude, &settings, &disabled);
        let narrator = check(&claude_checks, RoleName::Narrator);
        assert_eq!(
            narrator.compaction,
            "inherit →  off, DISABLE_AUTO_COMPACT=1 in your environment"
        );
        assert_eq!(
            narrator.warning.as_deref(),
            Some(
                "narrator keeps one session per run (lifetime run), but its compaction is off on claude-code."
            )
        );

        // Codex can't turn compaction off, so its consultant still compacts, late.
        let codex_checks = role_checks_in(&codex, &settings, &disabled);
        let consultant = check(&codex_checks, RoleName::Consultant);
        assert_eq!(
            consultant.compaction,
            "off →  approximated: compacts by 90% of the window at the latest"
        );
        assert!(!consultant.compaction_off);
        assert_eq!(consultant.warning, None);
        assert!(!codex_checks.iter().any(|c| c.role == RoleName::Reviewer));
        assert!(!codex_checks.iter().any(|c| c.role == RoleName::Main));
    }

    #[test]
    fn roles_follow_workers_and_the_crew() {
        let agy = FakeCli::named("antigravity-cli");
        let mut settings = Settings {
            crew: vec![ModelRef::new("claude-code", None)].into(),
            ..Settings::default()
        };
        assert!(role_checks_in(&agy, &settings, &|_| None).is_empty());

        settings.roles.insert(
            RoleName::Draft,
            role(
                &["antigravity-cli"],
                SessionLifetime::Task,
                Compaction::Inherit,
            ),
        );
        let checks = role_checks_in(&agy, &settings, &|_| None);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].role, RoleName::Draft);
        assert_eq!(
            checks[0].compaction,
            "inherit →  auto, agy has no setting for it"
        );
        assert_eq!(checks[0].fanout, "max 3");
    }
}
