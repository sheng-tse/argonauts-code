//! The OS sandbox around every CLI session and check: Seatbelt on macOS, Landlock with
//! namespaces on Linux, or a container. With `sandbox.network` on, the commands a session runs
//! reach the internet themselves; with it off, only the CLI's own hosts, through the proxy.
//! It writes only where the session's access allows, and fails closed: when the self-test fails,
//! CLI sessions are refused. Each role's `sandbox.mode` picks Argonauts Code's sandbox (`os` or
//! `container`), each tool's own (`tool`), or none at all (`none`).

pub mod container;
pub mod landlock;
pub mod launcher;
pub mod policy;
pub mod profile;
pub mod proxy;
pub mod scope;
pub mod seatbelt;
pub mod selftest;
pub mod view;

use std::sync::Arc;

use argonauts_core::CommandSpec;
use argonauts_core::agents::session_runner::{SandboxMode, SandboxProfile};
use argonauts_core::settings::approvals::{SandboxChoice, SandboxSettings};

pub use profile::{ProfileInput, can_read, can_write, session_profile};
pub use scope::{FORBIDDEN_PATHS, permission_scope};
pub use selftest::{SelfTest, self_test};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SandboxError {
    /// Nothing works here, such as no `sandbox-exec`, no Landlock, or no docker or podman.
    #[error("no working sandbox: {0}")]
    Unavailable(String),
    /// The profile can't be expressed, such as a relative path.
    #[error("bad sandbox profile: {0}")]
    Invalid(String),
}

/// Wraps a command so it runs inside a profile.
pub trait SandboxWrap: Send + Sync {
    fn mode(&self) -> SandboxMode;

    /// The command to spawn instead, such as `sandbox-exec -p <profile> <program> <args>`. Its
    /// environment, stdin, and files carry over.
    fn wrap(
        &self,
        command: CommandSpec,
        profile: &SandboxProfile,
    ) -> Result<CommandSpec, SandboxError>;

    /// One line for the launch record, such as `seatbelt · writes worktree, temp · network`.
    fn describe(&self, profile: &SandboxProfile) -> String;
}

/// The sandbox `sandbox.mode` asks for, on this OS. A session whose profile asks for a container,
/// as an untrusted repository's do, gets one whatever the mode.
pub fn for_settings(settings: &SandboxSettings) -> Arc<dyn SandboxWrap> {
    let container = container::Container::detect(&settings.container_image);
    let os: Arc<dyn SandboxWrap> = match settings.mode {
        SandboxChoice::Os if cfg!(target_os = "macos") => Arc::new(seatbelt::Seatbelt::new()),
        SandboxChoice::Os => Arc::new(landlock::Landlock::new()),
        SandboxChoice::Container => return Arc::new(container),
        SandboxChoice::Tool | SandboxChoice::None => Arc::new(Unsandboxed),
    };
    Arc::new(ByProfile { os, container })
}

/// Sends each profile to the sandbox its mode names.
struct ByProfile {
    os: Arc<dyn SandboxWrap>,
    container: container::Container,
}

impl SandboxWrap for ByProfile {
    fn mode(&self) -> SandboxMode {
        self.os.mode()
    }

    fn wrap(
        &self,
        command: CommandSpec,
        profile: &SandboxProfile,
    ) -> Result<CommandSpec, SandboxError> {
        match profile.mode {
            SandboxMode::Container => self.container.wrap(command, profile),
            SandboxMode::Os | SandboxMode::None => self.os.wrap(command, profile),
        }
    }

    fn describe(&self, profile: &SandboxProfile) -> String {
        match profile.mode {
            SandboxMode::Container => self.container.describe(profile),
            SandboxMode::Os | SandboxMode::None => self.os.describe(profile),
        }
    }
}

/// The network part of a sandbox's one-line description.
fn describe_network(profile: &SandboxProfile) -> String {
    match (profile.network, profile.proxy_port) {
        (true, _) => "network".to_owned(),
        (false, Some(port)) => format!("network only through the proxy on port {port}"),
        (false, None) => "no network".to_owned(),
    }
}

/// No sandbox of ours: endpoints, and `sandbox.mode` set to `tool` or `none`, which
/// `check_session` reports on every approval.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unsandboxed;

impl SandboxWrap for Unsandboxed {
    fn mode(&self) -> SandboxMode {
        SandboxMode::None
    }

    fn wrap(
        &self,
        command: CommandSpec,
        _profile: &SandboxProfile,
    ) -> Result<CommandSpec, SandboxError> {
        Ok(command)
    }

    fn describe(&self, _profile: &SandboxProfile) -> String {
        "no sandbox".to_owned()
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    mod seatbelt_outside;

    use super::*;

    #[test]
    fn a_container_profile_goes_to_the_container_whatever_the_mode() {
        let settings = SandboxSettings {
            mode: SandboxChoice::Tool,
            container_image: "img".to_owned(),
            ..SandboxSettings::default()
        };
        let sandbox = for_settings(&settings);
        assert_eq!(sandbox.mode(), SandboxMode::None);
        let os = SandboxProfile {
            mode: SandboxMode::Os,
            ..SandboxProfile::default()
        };
        assert_eq!(sandbox.describe(&os), "no sandbox");
        let boxed = SandboxProfile {
            mode: SandboxMode::Container,
            ..SandboxProfile::default()
        };
        assert!(sandbox.describe(&boxed).starts_with("container · "));
    }

    #[test]
    fn the_os_mode_picks_this_systems_sandbox() {
        let settings = SandboxSettings {
            mode: SandboxChoice::Os,
            ..SandboxSettings::default()
        };
        let described = for_settings(&settings).describe(&SandboxProfile::default());
        let name = if cfg!(target_os = "macos") {
            "seatbelt"
        } else {
            "landlock"
        };
        assert!(described.starts_with(name), "{described}");
    }
}
