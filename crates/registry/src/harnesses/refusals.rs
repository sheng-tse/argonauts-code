//! What no `[[harness]]` entry may do, whoever wrote it: carry a secret, switch approvals off,
//! fetch its program at launch, or run an agent whose tools escape the sandbox.

use std::path::Path;

use argonauts_core::session::VENDOR_KEY_ENV_VARS;

use super::wrappers::{base_name, follow};

const SECRET_SUFFIXES: &[&str] = &["_KEY", "_TOKEN", "_SECRET", "_PASSWORD"];

/// Variables that let Hermes approve its own commands, run hooks, or take work from a board.
const BYPASS_VARIABLES: &[&str] = &["HERMES_YOLO_MODE", "HERMES_ACCEPT_HOOKS"];
const BYPASS_VARIABLE_PREFIXES: &[&str] = &["HERMES_KANBAN_"];

/// Long flags that switch approvals or the sandbox off in any CLI that has them.
const BYPASS_FLAGS: &[&str] = &[
    "--yolo",
    "--accept-hooks",
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "--dangerously-bypass-approvals-and-sandbox",
];

/// Hermes' short forms, which mean other things to other CLIs: `-z` runs a one-shot without
/// approvals and `-w` hands the turn to a worktree Argonauts doesn't manage.
const HERMES_BYPASS_FLAGS: &[&str] = &["-z", "-w"];

/// Programs that download a package and run it, so what runs can change between launches.
const FETCHING_LAUNCHERS: &[&str] = &["npx", "uvx", "bunx", "pnpx"];
/// The same, as a package manager's subcommand.
const FETCHING_SUBCOMMANDS: &[(&str, &[&str])] = &[
    ("npm", &["exec"]),
    ("pnpm", &["dlx"]),
    ("yarn", &["dlx"]),
    ("uv", &["tool", "run"]),
    ("bun", &["x"]),
];

/// Variables that load code into the agent or the tools it runs, or pick which program runs,
/// so a relative value or a later commit could change what runs. `home` gives an agent a home
/// of its own in place of `HOME`.
const CODE_LOADING_VARIABLES: &[&str] = &[
    "PATH",
    "HOME",
    "XDG_CONFIG_HOME",
    "ENV",
    "BASH_ENV",
    "ZDOTDIR",
    "PROMPT_COMMAND",
    "PS4",
    "SHELLOPTS",
    "BASHOPTS",
    "NODE_OPTIONS",
    "NODE_PATH",
    "PYTHONPATH",
    "PYTHONHOME",
    "PYTHONSTARTUP",
    "PYTHONUSERBASE",
    "PYTHONBREAKPOINT",
    "PERL5LIB",
    "PERLLIB",
    "PERL5OPT",
    "RUBYLIB",
    "RUBYOPT",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "CLASSPATH",
    "GIT_EXEC_PATH",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
];
/// The dynamic loaders' variables, such as `LD_PRELOAD` and `DYLD_INSERT_LIBRARIES`, and git's
/// config overrides, which can name hooks and fsmonitor programs.
const CODE_LOADING_PREFIXES: &[&str] = &["LD_", "DYLD_", "GIT_CONFIG"];

pub(super) const OPENCLAW_ACP: &str = "`openclaw acp` isn't a profile: its ACP bridge runs tools \
    in the Gateway, outside the task's sandbox and worktree, passes the working folder only as \
    text at the top of the prompt, and refuses per-session MCP servers";

/// Why `name` can't be set by an entry, if it can't.
pub(super) fn env_name_refusal(name: &str) -> Option<String> {
    let upper = name.to_ascii_uppercase();
    let secret = SECRET_SUFFIXES
        .iter()
        .any(|suffix| upper.ends_with(suffix) || upper == suffix[1..])
        || VENDOR_KEY_ENV_VARS.contains(&upper.as_str());
    if secret {
        return Some(format!(
            "env_set {name} looks like a secret. Keep keys in your environment and list the \
             name in `env` instead"
        ));
    }
    let bypass = BYPASS_VARIABLES.contains(&upper.as_str())
        || BYPASS_VARIABLE_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix));
    bypass.then(|| format!("env_set {name} switches the agent's approvals off"))
}

/// Why `name` can't be in `env_set`, if it can't. `home.env` skips the code-loading check,
/// since Argonauts fills it with a folder it made from a trusted template.
pub(super) fn env_set_refusal(name: &str) -> Option<String> {
    env_name_refusal(name).or_else(|| {
        let upper = name.to_ascii_uppercase();
        let loads_code = CODE_LOADING_VARIABLES.contains(&upper.as_str())
            || CODE_LOADING_PREFIXES
                .iter()
                .any(|prefix| upper.starts_with(prefix));
        loads_code.then(|| {
            format!(
                "env_set {name} can load code or pick the program that runs, so a commit could \
                 change what runs"
            )
        })
    })
}

/// Why `arg` can't be passed, if it can't. `hermes` adds Hermes' own short flags.
pub(super) fn arg_refusal(arg: &str, hermes: bool) -> Option<String> {
    let flag = arg.split_once('=').map_or(arg, |(flag, _)| flag);
    let refused = BYPASS_FLAGS.contains(&flag) || (hermes && HERMES_BYPASS_FLAGS.contains(&flag));
    refused.then(|| format!("{flag} switches the agent's approvals or sandbox off"))
}

/// Why this launch can't be used, if it fetches its program each time, itself or through a
/// wrapper such as `env`.
pub(super) fn launcher_refusal(program: &str, args: &[String]) -> Option<String> {
    follow(program, args)
        .steps
        .iter()
        .find_map(|step| fetch_refusal(step.program, step.args))
}

/// Whether this launch runs `openclaw acp`, itself or through a wrapper.
pub(super) fn runs_openclaw_acp(program: &str, args: &[String]) -> bool {
    follow(program, args).steps.iter().any(|step| {
        base_name(program_name(step.program)) == "openclaw"
            && step.args.iter().any(|arg| arg == "acp")
    })
}

fn fetch_refusal(program: &str, args: &[String]) -> Option<String> {
    let base = base_name(program_name(program));
    let name = base.as_str();
    let fetches = FETCHING_LAUNCHERS.contains(&name)
        || FETCHING_SUBCOMMANDS.iter().any(|(manager, subcommand)| {
            name == *manager
                && args.len() >= subcommand.len()
                && args
                    .iter()
                    .zip(subcommand.iter())
                    .all(|(arg, want)| arg == want)
        });
    fetches.then(|| {
        format!(
            "{name} downloads and runs a package at every launch, so what runs can change \
             under you. Install the agent and name its absolute path"
        )
    })
}

/// The file name a path or bare program ends in, such as `hermes` for `/usr/bin/hermes`.
pub(super) fn program_name(program: &str) -> &str {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_names_by_suffix_and_by_list() {
        for name in [
            "OPENAI_API_KEY",
            "gh_token",
            "DB_PASSWORD",
            "CLIENT_SECRET",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "TOKEN",
        ] {
            assert!(env_name_refusal(name).is_some(), "{name}");
        }
        for name in ["TOKENIZER_MODE", "KEYBOARD", "NO_UPDATE", "HERMES_HOME"] {
            assert!(env_name_refusal(name).is_none(), "{name}");
        }
    }

    #[test]
    fn bypass_flags_with_values() {
        assert!(arg_refusal("--yolo", false).is_some());
        assert!(arg_refusal("--yolo=true", false).is_some());
        assert!(arg_refusal("-z", true).is_some());
        assert!(arg_refusal("-z", false).is_none());
        assert!(arg_refusal("--model", true).is_none());
    }

    #[test]
    fn fetching_launchers() {
        let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        assert!(launcher_refusal("/usr/local/bin/npx", &args(&["pkg"])).is_some());
        assert!(launcher_refusal("uvx", &[]).is_some());
        assert!(launcher_refusal("/opt/bin/uv", &args(&["tool", "run", "x"])).is_some());
        assert!(launcher_refusal("/opt/bin/uv", &args(&["tool", "list"])).is_none());
        assert!(launcher_refusal("/opt/bin/npm", &args(&["exec", "x"])).is_some());
        assert!(launcher_refusal("/opt/bin/agent", &args(&["npx"])).is_none());
        // Through wrappers, which run their first argument that isn't a flag.
        assert!(launcher_refusal("/usr/bin/env", &args(&["npx", "-y", "pkg"])).is_some());
        assert!(launcher_refusal("/usr/bin/env", &args(&["-u", "X", "bunx", "pkg"])).is_some());
        assert!(
            launcher_refusal(
                "/usr/bin/nohup",
                &args(&["/opt/bin/uv", "tool", "run", "x"])
            )
            .is_some()
        );
        assert!(
            launcher_refusal("/usr/bin/env", &args(&["/bin/sh", "/usr/local/bin/npx"])).is_some()
        );
        assert!(launcher_refusal("/usr/local/bin/bun", &args(&["x", "pkg"])).is_some());
        assert!(launcher_refusal("/usr/bin/env", &args(&["/opt/bin/agent", "npx"])).is_none());
    }

    #[test]
    fn openclaw_acp_through_wrappers() {
        let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        assert!(runs_openclaw_acp("/opt/bin/openclaw", &args(&["acp"])));
        assert!(runs_openclaw_acp(
            "/usr/bin/env",
            &args(&["openclaw", "acp"])
        ));
        assert!(runs_openclaw_acp(
            "/usr/bin/nohup",
            &args(&["/opt/bin/openclaw", "acp"])
        ));
        assert!(!runs_openclaw_acp(
            "/usr/bin/env",
            &args(&["openclaw", "gateway"])
        ));
    }

    #[test]
    fn code_loading_variables() {
        for name in [
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "NODE_OPTIONS",
            "PYTHONPATH",
            "PATH",
        ] {
            assert!(env_set_refusal(name).is_some(), "{name}");
            assert!(env_name_refusal(name).is_none(), "{name}");
        }
        for name in [
            "NO_UPDATE",
            "HERMES_HOME",
            "PYTHONUNBUFFERED",
            "GIT_AUTHOR_NAME",
        ] {
            assert!(env_set_refusal(name).is_none(), "{name}");
        }
    }
}
