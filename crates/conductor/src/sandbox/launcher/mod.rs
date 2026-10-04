//! The program the Linux sandbox runs first. It is the `argonauts` binary itself, started with
//! [`LAUNCHER_ARG`]: it builds the session's view of the filesystem and its network in fresh
//! namespaces, applies Landlock, and then becomes the harness. It has to run before anything
//! starts a thread, since the kernel gives a user namespace only to a single-threaded process,
//! so `main` calls [`launcher_main`] first.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(unix)]
pub mod relay;

use std::convert::Infallible;
use std::ffi::{OsStr, OsString};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
pub(crate) use linux::{landlock_support, namespace_support};

/// The first argument that starts the launcher instead of the app.
pub const LAUNCHER_ARG: &str = "__argonauts-sandbox";

/// What the launcher does with the rest of its arguments: run a session in the Linux sandbox,
/// bridge the proxy to a container (outside it), or relay it (inside).
const MODE_OS: &str = "os";
const MODE_BRIDGE: &str = "bridge";
const MODE_RELAY: &str = "relay";

/// The start of the name of the folder that holds a container's proxy sockets.
pub const BRIDGE_DIR: &str = "argonauts-bridge";

/// The launcher's exit code when it can't set the sandbox up. The harness never ran.
pub const LAUNCH_FAILED: i32 = 125;

/// The profile, as the launcher gets it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchPlan {
    pub writable: Vec<PathBuf>,
    pub readable: Vec<PathBuf>,
    pub deny_read: Vec<PathBuf>,
    /// Without it the session gets its own network namespace, where only `proxies` lead out.
    pub network: bool,
    /// Proxies on this machine's loopback that the session's harness is told to use. Each is
    /// reachable inside at the same address, and nothing else outside is.
    pub proxies: Vec<SocketAddr>,
    /// Paths that stay closed even where they don't exist yet, as the profile's `closed`.
    #[serde(default)]
    pub closed: Vec<PathBuf>,
}

/// The launcher's arguments for running `program` with `args` under `plan`.
pub fn launcher_args(plan: &LaunchPlan, program: &OsStr, args: &[String]) -> Vec<String> {
    let mut all = vec![
        LAUNCHER_ARG.to_owned(),
        MODE_OS.to_owned(),
        serde_json::to_string(plan).unwrap_or_default(),
        program.to_string_lossy().into_owned(),
    ];
    all.extend(args.iter().cloned());
    all
}

/// Outside a container: serve the proxy sockets in `dir`, then run the container with `runtime`
/// and the arguments that follow these.
pub fn bridge_args(dir: &Path, proxies: &[SocketAddr], runtime: &str) -> Vec<String> {
    vec![
        LAUNCHER_ARG.to_owned(),
        MODE_BRIDGE.to_owned(),
        dir.to_string_lossy().into_owned(),
        join(proxies),
        runtime.to_owned(),
    ]
}

/// Inside a container: listen at each proxy's address, carry connections to the sockets in
/// `dir`, then run `program` with the arguments that follow these.
pub fn relay_args(dir: &Path, proxies: &[SocketAddr], program: &str) -> Vec<String> {
    vec![
        LAUNCHER_ARG.to_owned(),
        MODE_RELAY.to_owned(),
        dir.to_string_lossy().into_owned(),
        join(proxies),
        program.to_owned(),
    ]
}

fn join(proxies: &[SocketAddr]) -> String {
    proxies
        .iter()
        .map(SocketAddr::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// The socket in a bridge folder for the proxy at `index`.
pub fn bridge_socket(dir: &Path, index: usize) -> PathBuf {
    dir.join(format!("p{index}"))
}

/// Runs the launcher when this process was started as one, and gives its exit code. `None` for
/// any other start. On success the harness replaces this process, so it never returns.
pub fn launcher_main() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new(LAUNCHER_ARG)) {
        return None;
    }
    let rest: Vec<OsString> = args.collect();
    let err = match run(rest) {
        Ok(never) => match never {},
        Err(err) => err,
    };
    eprintln!("argonauts sandbox: {err}");
    Some(LAUNCH_FAILED)
}

fn run(args: Vec<OsString>) -> Result<Infallible, String> {
    let mut args = args.into_iter();
    let mode = args.next().unwrap_or_default();
    let mut text = |what: &str| -> Result<String, String> {
        args.next()
            .ok_or_else(|| format!("the launcher got no {what}"))?
            .into_string()
            .map_err(|_| format!("the launcher's {what} isn't text"))
    };
    if mode == MODE_BRIDGE || mode == MODE_RELAY {
        let dir = PathBuf::from(text("folder")?);
        let proxies = text("proxies")?
            .split(',')
            .map(|addr| addr.parse::<SocketAddr>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| format!("the launcher's proxies are bad: {err}"))?;
        let program = PathBuf::from(text("program")?);
        let rest: Vec<OsString> = args.collect();
        return if mode == MODE_BRIDGE {
            bridge(&dir, &proxies, program, rest)
        } else {
            relay(&dir, &proxies, program, rest)
        };
    }
    if mode != MODE_OS {
        return Err(format!("unknown launcher mode {}", mode.to_string_lossy()));
    }
    let plan: LaunchPlan = serde_json::from_str(&text("plan")?)
        .map_err(|err| format!("the launcher's plan is bad: {err}"))?;
    let program = PathBuf::from(text("program")?);
    if !program.is_absolute() {
        return Err(format!("{} is not an absolute path", program.display()));
    }
    launch(plan, program, args.collect())
}

#[cfg(target_os = "linux")]
fn launch(plan: LaunchPlan, program: PathBuf, args: Vec<OsString>) -> Result<Infallible, String> {
    linux::launch(plan, program, args)
}

#[cfg(not(target_os = "linux"))]
fn launch(
    _plan: LaunchPlan,
    _program: PathBuf,
    _args: Vec<OsString>,
) -> Result<Infallible, String> {
    Err("the Landlock sandbox runs only on Linux".to_owned())
}

#[cfg(target_os = "linux")]
fn bridge(
    dir: &Path,
    proxies: &[SocketAddr],
    runtime: PathBuf,
    args: Vec<OsString>,
) -> Result<Infallible, String> {
    linux::run_bridge(dir, proxies, runtime, args)
}

#[cfg(target_os = "linux")]
fn relay(
    dir: &Path,
    proxies: &[SocketAddr],
    program: PathBuf,
    args: Vec<OsString>,
) -> Result<Infallible, String> {
    linux::run_relay(dir, proxies, program, args)
}

#[cfg(not(target_os = "linux"))]
fn bridge(
    _dir: &Path,
    _proxies: &[SocketAddr],
    _runtime: PathBuf,
    _args: Vec<OsString>,
) -> Result<Infallible, String> {
    Err("the container proxy bridge runs only on Linux".to_owned())
}

#[cfg(not(target_os = "linux"))]
fn relay(
    _dir: &Path,
    _proxies: &[SocketAddr],
    _program: PathBuf,
    _args: Vec<OsString>,
) -> Result<Infallible, String> {
    Err("the container proxy relay runs only on Linux".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_survives_the_trip_through_arguments() {
        let plan = LaunchPlan {
            writable: vec!["/w".into()],
            readable: vec!["/r \"quoted\"".into()],
            deny_read: vec!["/d".into()],
            network: false,
            proxies: vec!["127.0.0.1:8080".parse().unwrap()],
            closed: vec!["/w/.claude".into()],
        };
        let args = launcher_args(&plan, OsStr::new("/bin/echo"), &["hi".to_owned()]);
        assert_eq!(args[0], LAUNCHER_ARG);
        assert_eq!(args[1], MODE_OS);
        assert_eq!(serde_json::from_str::<LaunchPlan>(&args[2]).unwrap(), plan);
        assert_eq!(&args[3..], ["/bin/echo", "hi"]);
    }

    #[test]
    fn bad_launcher_arguments_are_refused() {
        let os = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        let err = |list: &[&str]| run(os(list)).map(|never| match never {}).unwrap_err();
        assert!(err(&["nope"]).contains("unknown launcher mode"));
        assert!(err(&["os"]).contains("no plan"));
        assert!(err(&["os", "{"]).contains("plan is bad"));
        let plan = serde_json::to_string(&LaunchPlan {
            writable: Vec::new(),
            readable: Vec::new(),
            deny_read: Vec::new(),
            network: true,
            proxies: Vec::new(),
            closed: Vec::new(),
        })
        .unwrap();
        assert!(err(&["os", &plan]).contains("no program"));
        assert!(err(&["os", &plan, "sh"]).contains("not an absolute path"));
        assert!(err(&["relay", "/d", "nope", "/bin/sh"]).contains("proxies are bad"));
        assert!(err(&["bridge", "/d"]).contains("no proxies"));
    }

    #[test]
    fn bridge_and_relay_arguments() {
        let proxies: Vec<SocketAddr> =
            vec!["127.0.0.1:1".parse().unwrap(), "[::1]:2".parse().unwrap()];
        let dir = Path::new("/tmp/argonauts-bridge-x");
        assert_eq!(
            bridge_args(dir, &proxies, "/usr/bin/docker"),
            [
                LAUNCHER_ARG,
                MODE_BRIDGE,
                "/tmp/argonauts-bridge-x",
                "127.0.0.1:1,[::1]:2",
                "/usr/bin/docker"
            ]
        );
        assert_eq!(relay_args(dir, &proxies, "codex")[4], "codex");
        assert_eq!(bridge_socket(dir, 1), dir.join("p1"));
    }
}
