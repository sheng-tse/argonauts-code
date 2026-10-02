//! Linux: the session starts under the launcher (the `argonauts` binary with a hidden first
//! argument), which hides denied paths with mounts in a mount namespace of its own, limits writes
//! to the writable paths with Landlock, and, without network, runs the session in a network
//! namespace where only the proxy leads out. It fails closed: on a kernel without Landlock ABI 3
//! or without unprivileged user namespaces, every session is refused with the reason.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;

use argonauts_core::CommandSpec;
use argonauts_core::worker::{SandboxMode, SandboxProfile};

use super::launcher::{LaunchPlan, launcher_args};
use super::profile::check_profile;
use super::{SandboxError, SandboxWrap, describe_network};
use crate::process::resolve_program;

/// The variables a harness reads its proxy from, as curl, Node, Go, and Rust's HTTP clients do.
const PROXY_VARS: &[&str] = &[
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
];

#[derive(Debug, Clone, Default)]
pub struct Landlock {
    /// The `argonauts` binary, which runs the launcher.
    launcher: Option<PathBuf>,
}

impl Landlock {
    /// Launches through the running `argonauts` binary.
    pub fn new() -> Self {
        Self {
            launcher: std::env::current_exe().ok(),
        }
    }

    /// Launches through `launcher`, a binary whose `main` starts with
    /// [`launcher_main`](super::launcher::launcher_main).
    pub fn with_launcher(launcher: impl Into<PathBuf>) -> Self {
        Self {
            launcher: Some(launcher.into()),
        }
    }
}

impl SandboxWrap for Landlock {
    fn mode(&self) -> SandboxMode {
        SandboxMode::Os
    }

    fn wrap(
        &self,
        command: CommandSpec,
        profile: &SandboxProfile,
    ) -> Result<CommandSpec, SandboxError> {
        check_profile(profile)?;
        supported()?;
        let launcher = self
            .launcher
            .clone()
            .filter(|path| path.is_absolute() && path.is_file())
            .ok_or_else(|| {
                SandboxError::Unavailable(
                    "can't find the argonauts binary that starts the sandbox".to_owned(),
                )
            })?;
        let program = resolve_program(
            &command.program,
            std::env::var_os("PATH").as_deref(),
            &command.cwd,
        )
        .map_err(|err| SandboxError::Invalid(format!("can't sandbox the command: {err}")))?;
        let plan = LaunchPlan {
            writable: profile.writable.clone(),
            readable: profile.readable.clone(),
            deny_read: profile.deny_read.clone(),
            network: profile.network,
            proxies: reachable_proxies(profile, &command),
        };
        let args = launcher_args(&plan, program.as_os_str(), &command.args);
        Ok(CommandSpec {
            program: launcher.to_string_lossy().into_owned(),
            args,
            ..command
        })
    }

    fn describe(&self, profile: &SandboxProfile) -> String {
        format!(
            "landlock · writes {} · denies {} · {}",
            count(profile.writable.len()),
            count(profile.deny_read.len()),
            describe_network(profile)
        )
    }
}

#[cfg(target_os = "linux")]
fn supported() -> Result<(), SandboxError> {
    super::launcher::landlock_support()
        .and_then(|()| super::launcher::namespace_support())
        .map_err(|why| {
            SandboxError::Unavailable(format!(
                "{why}. Harness sessions are refused until that's fixed"
            ))
        })
}

#[cfg(not(target_os = "linux"))]
fn supported() -> Result<(), SandboxError> {
    Err(SandboxError::Unavailable(
        "the Landlock sandbox runs only on Linux".to_owned(),
    ))
}

fn count(n: usize) -> String {
    if n == 1 {
        "1 path".to_owned()
    } else {
        format!("{n} paths")
    }
}

/// What a session without network may still reach on this machine's loopback: the Argonauts
/// proxy when its profile names one, else the proxies `command` points its harness at.
pub fn reachable_proxies(profile: &SandboxProfile, command: &CommandSpec) -> Vec<SocketAddr> {
    match (profile.network, profile.proxy_port) {
        (true, _) => Vec::new(),
        (false, Some(port)) => vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))],
        (false, None) => loopback_proxies(command),
    }
}

/// The proxies on this machine's loopback that `command` points its harness at, which the
/// session can still reach without network. A proxy elsewhere stays out of reach.
fn loopback_proxies(command: &CommandSpec) -> Vec<SocketAddr> {
    let value = |name: &str| -> Option<String> {
        if let Some((_, value)) = command.env_set.iter().rev().find(|(key, _)| key == name) {
            return Some(value.clone());
        }
        if command.env_remove.iter().any(|key| key == name) {
            return None;
        }
        std::env::var(name).ok()
    };
    let mut proxies = Vec::new();
    for addr in PROXY_VARS
        .iter()
        .filter_map(|name| value(name))
        .filter_map(|url| loopback_addr(&url))
    {
        if !proxies.contains(&addr) {
            proxies.push(addr);
        }
    }
    proxies
}

/// `http://127.0.0.1:8080`, `localhost:8080/`, `socks5h://user@[::1]:1080` and the like, when the
/// host is loopback and the port is given.
fn loopback_addr(url: &str) -> Option<SocketAddr> {
    let rest = url.trim();
    let rest = rest.split_once("://").map_or(rest, |(_, rest)| rest);
    let rest = rest.split(['/', '?', '#']).next()?;
    let rest = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
    let (host, port) = rest.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let ip = match host.trim_start_matches('[').trim_end_matches(']') {
        "localhost" => IpAddr::V4(Ipv4Addr::LOCALHOST),
        other => other.parse().ok()?,
    };
    let loopback = match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6 == Ipv6Addr::LOCALHOST,
    };
    loopback.then_some(SocketAddr::new(ip, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_proxy_addresses() {
        let ok = |url: &str, want: &str| {
            assert_eq!(loopback_addr(url), Some(want.parse().unwrap()), "{url}");
        };
        ok("http://127.0.0.1:8080", "127.0.0.1:8080");
        ok("http://localhost:3128/", "127.0.0.1:3128");
        ok("127.0.0.1:9", "127.0.0.1:9");
        ok("socks5h://user:pw@[::1]:1080", "[::1]:1080");
        ok("http://127.0.0.2:80?x", "127.0.0.2:80");
        for url in [
            "http://proxy.corp:8080",
            "http://10.0.0.1:8080",
            "http://127.0.0.1",
            "http://localhost:notaport",
            "",
        ] {
            assert_eq!(loopback_addr(url), None, "{url}");
        }
    }

    #[test]
    fn proxies_come_from_the_commands_own_environment_first() {
        let mut command = CommandSpec::new("/bin/sh", "/");
        command
            .env_set
            .push(("HTTPS_PROXY".to_owned(), "http://127.0.0.1:7001".to_owned()));
        command
            .env_set
            .push(("http_proxy".to_owned(), "http://127.0.0.1:7001".to_owned()));
        command
            .env_set
            .push(("ALL_PROXY".to_owned(), "http://proxy.corp:8080".to_owned()));
        command.env_remove.extend(
            PROXY_VARS
                .iter()
                .map(|name| (*name).to_owned())
                .filter(|name| !command.env_set.iter().any(|(key, _)| key == name)),
        );
        assert_eq!(
            loopback_proxies(&command),
            vec!["127.0.0.1:7001".parse::<SocketAddr>().unwrap()]
        );
    }

    #[test]
    fn the_profiles_proxy_is_the_only_way_out() {
        let mut command = CommandSpec::new("sh", "/tmp");
        command
            .env_set
            .push(("HTTPS_PROXY".to_owned(), "http://127.0.0.1:7004".to_owned()));
        command.env_remove.extend(
            PROXY_VARS
                .iter()
                .filter(|name| **name != "HTTPS_PROXY")
                .map(|name| (*name).to_owned()),
        );
        let mut profile = SandboxProfile {
            proxy_port: Some(4128),
            ..SandboxProfile::default()
        };
        assert_eq!(
            reachable_proxies(&profile, &command),
            vec!["127.0.0.1:4128".parse::<SocketAddr>().unwrap()]
        );
        assert_eq!(
            Landlock::new().describe(&profile),
            "landlock · writes 0 paths · denies 0 paths · network only through the proxy on port 4128"
        );
        // A gate has no proxy of ours, so the ones its command names stay reachable.
        profile.proxy_port = None;
        assert_eq!(
            reachable_proxies(&profile, &command),
            vec!["127.0.0.1:7004".parse::<SocketAddr>().unwrap()]
        );
        profile.network = true;
        assert!(reachable_proxies(&profile, &command).is_empty());
    }

    #[test]
    fn describe_counts_paths() {
        let profile = SandboxProfile {
            writable: vec!["/w".into()],
            deny_read: vec!["/a".into(), "/b".into()],
            ..SandboxProfile::default()
        };
        assert_eq!(
            Landlock::new().describe(&profile),
            "landlock · writes 1 path · denies 2 paths · no network"
        );
    }

    #[test]
    fn a_relative_path_is_refused_before_anything_runs() {
        let profile = SandboxProfile {
            writable: vec!["relative".into()],
            ..SandboxProfile::default()
        };
        let err = Landlock::new()
            .wrap(CommandSpec::new("/bin/sh", "/"), &profile)
            .unwrap_err();
        assert!(matches!(err, SandboxError::Invalid(_)), "{err}");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn landlock_fails_closed_off_linux() {
        let err = Landlock::new()
            .wrap(CommandSpec::new("/bin/sh", "/"), &SandboxProfile::default())
            .unwrap_err();
        assert!(
            matches!(&err, SandboxError::Unavailable(why) if why.contains("only on Linux")),
            "{err}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_missing_launcher_is_refused() {
        if super::supported().is_err() {
            return;
        }
        let err = Landlock::with_launcher("/nonexistent/argonauts")
            .wrap(CommandSpec::new("/bin/sh", "/"), &SandboxProfile::default())
            .unwrap_err();
        assert!(
            matches!(&err, SandboxError::Unavailable(why) if why.contains("argonauts binary")),
            "{err}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_wrap_runs_the_launcher_with_the_plan() {
        use super::super::launcher::LAUNCHER_ARG;
        if super::supported().is_err() {
            return;
        }
        let mut command = CommandSpec::new("sh", "/tmp").args(["-c", "true"]);
        command
            .env_set
            .push(("HTTPS_PROXY".to_owned(), "http://127.0.0.1:7002".to_owned()));
        let profile = SandboxProfile {
            writable: vec!["/tmp".into()],
            ..SandboxProfile::default()
        };
        let wrapped = Landlock::with_launcher("/bin/true")
            .wrap(command, &profile)
            .unwrap();
        assert_eq!(wrapped.program, "/bin/true");
        assert_eq!(wrapped.args[0], LAUNCHER_ARG);
        let plan: LaunchPlan = serde_json::from_str(&wrapped.args[2]).unwrap();
        assert_eq!(plan.writable, vec![PathBuf::from("/tmp")]);
        assert_eq!(plan.proxies, vec!["127.0.0.1:7002".parse().unwrap()]);
        assert!(wrapped.args[3].ends_with("/sh"), "{:?}", wrapped.args);
        assert_eq!(&wrapped.args[4..], ["-c", "true"]);
        assert!(wrapped.env_set.iter().any(|(key, _)| key == "HTTPS_PROXY"));
    }
}
