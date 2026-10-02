//! Two choices the sandbox settings make: what happens in a repository you haven't trusted
//! (`sandbox.untrusted_repos`), and whether your own harness hooks run in Argonauts sessions
//! (`sandbox.user_hooks`).

use argonauts_core::settings::approvals::{SandboxSettings, UntrustedRepos, UserHooks};
use argonauts_core::{Adapter, LaunchTweak};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoAction {
    Proceed,
    /// Run its sessions in a container.
    Container,
    Refuse(String),
}

/// Harnesses load a repository's own hooks, MCP servers, and agents, each in its own way, so an
/// untrusted repository runs in a container or not at all.
pub fn untrusted_repo(settings: &SandboxSettings, trusted: bool) -> RepoAction {
    if trusted {
        return RepoAction::Proceed;
    }
    match settings.untrusted_repos {
        UntrustedRepos::Container => RepoAction::Container,
        UntrustedRepos::Refuse => RepoAction::Refuse(
            "This repository isn't trusted, and its hooks, MCP servers, or agents could run in \
             a session. Review its config and trust it, or set \
             sandbox.untrusted_repos = \"container\"."
                .to_owned(),
        ),
    }
}

/// The launch tweak for `sandbox.user_hooks`: the adapter's `user_hooks_off` when set to
/// `disable`, nothing for `inherit`. Its effect says where the harness can't do it.
pub fn user_hooks(adapter: &dyn Adapter, settings: &SandboxSettings) -> LaunchTweak {
    match settings.user_hooks {
        UserHooks::Disable => adapter.user_hooks_off(),
        UserHooks::Inherit => LaunchTweak::default(),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::{CommandSpec, SessionRequest, StopPlan, StreamParser, TweakEffect};

    use super::*;
    use crate::testutil::ScriptAdapter;

    #[test]
    fn untrusted_repo_policy() {
        let mut settings = SandboxSettings::default();
        assert_eq!(untrusted_repo(&settings, true), RepoAction::Proceed);
        assert_eq!(untrusted_repo(&settings, false), RepoAction::Container);

        settings.untrusted_repos = UntrustedRepos::Refuse;
        assert_eq!(untrusted_repo(&settings, true), RepoAction::Proceed);
        match untrusted_repo(&settings, false) {
            RepoAction::Refuse(why) => assert!(why.contains("sandbox.untrusted_repos"), "{why}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// A harness with a verified flag that skips your hooks.
    struct SkipsHooks(ScriptAdapter);

    impl Adapter for SkipsHooks {
        fn id(&self) -> &'static str {
            self.0.id()
        }
        fn display_name(&self) -> &'static str {
            self.0.display_name()
        }
        fn binary(&self) -> &'static str {
            self.0.binary()
        }
        fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
            self.0.command(req)
        }
        fn parser(&self) -> Box<dyn StreamParser> {
            self.0.parser()
        }
        fn version_args(&self) -> &'static [&'static str] {
            self.0.version_args()
        }
        fn parse_version(&self, output: &str) -> Option<String> {
            self.0.parse_version(output)
        }
        fn tested_versions(&self) -> &'static [&'static str] {
            self.0.tested_versions()
        }
        fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
            self.0.cheapest_model()
        }
        fn stop_plan(&self) -> StopPlan {
            self.0.stop_plan()
        }
        fn user_hooks_off(&self) -> LaunchTweak {
            LaunchTweak {
                args: vec!["--no-user-hooks".into()],
                ..LaunchTweak::default()
            }
        }
    }

    #[test]
    fn user_hooks_policy() {
        let mut settings = SandboxSettings::default();
        let skips = SkipsHooks(ScriptAdapter::new("true"));
        let plain = ScriptAdapter::new("true");

        assert_eq!(user_hooks(&skips, &settings), LaunchTweak::default());
        assert_eq!(user_hooks(&plain, &settings), LaunchTweak::default());

        settings.user_hooks = UserHooks::Disable;
        let tweak = user_hooks(&skips, &settings);
        assert_eq!(tweak.args, ["--no-user-hooks"]);
        assert_eq!(tweak.effect, TweakEffect::Exact);
        assert!(matches!(
            user_hooks(&plain, &settings).effect,
            TweakEffect::NotPossible { .. }
        ));
    }
}
