//! A check's environment is an allowlist, never the run host's own: `PATH`, `LANG`, `TERM`,
//! `TMPDIR`, `HOME` set to a temp directory, and `CI=1`. No agent key, and nothing named
//! `*_KEY`, `*_TOKEN`, `*_SECRET`, or `*_PASSWORD`, so a test an agent edited can't print a key
//! into output that reviewers, judges, and the journal see. Python writes no bytecode, so a
//! check leaves no compiled files beside the code it ran.

use std::path::Path;

use argonauts_core::PROVIDER_KEY_ENV_VARS;

/// Variables passed through from Argonauts Code's environment.
const PASSED: &[&str] = &["PATH", "LANG", "TERM"];

/// Where toolchains live. They pass through because `HOME` moves, and rustup, cargo, go, and
/// pyenv would otherwise lose the toolchains and module caches installed under your home.
const TOOLCHAINS: &[&str] = &[
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "GOROOT",
    "GOPATH",
    "GOMODCACHE",
    "JAVA_HOME",
    "NVM_DIR",
    "PYENV_ROOT",
    "VIRTUAL_ENV",
];

/// Toolchain homes that default to a folder of your home, pinned there when unset.
const HOME_DEFAULTS: &[(&str, &str)] = &[
    ("CARGO_HOME", ".cargo"),
    ("RUSTUP_HOME", ".rustup"),
    ("GOPATH", "go"),
    ("PYENV_ROOT", ".pyenv"),
];

const SECRET_SUFFIXES: &[&str] = &["KEY", "TOKEN", "SECRET", "PASSWORD"];

/// The whole environment for a check or setup command, from the run host's variables. `HOME` is
/// `temp_root/home` and `TMPDIR` is `temp_root/tmp`; the caller creates both.
pub fn check_env(
    vars: impl Iterator<Item = (String, String)>,
    temp_root: &Path,
) -> Vec<(String, String)> {
    let vars: Vec<(String, String)> = vars.collect();
    let allowed = |name: &str| {
        (PASSED.contains(&name) || TOOLCHAINS.contains(&name))
            && !looks_secret(name)
            && !PROVIDER_KEY_ENV_VARS.contains(&name)
    };
    let mut env: Vec<(String, String)> = vars
        .iter()
        .filter(|(name, _)| allowed(name))
        .cloned()
        .collect();

    let real_home = vars
        .iter()
        .find(|(name, _)| name == "HOME")
        .map(|(_, value)| Path::new(value))
        .filter(|home| home.is_absolute());
    if let Some(real_home) = real_home {
        for (name, folder) in HOME_DEFAULTS {
            let dir = real_home.join(folder);
            if !env.iter().any(|(set, _)| set == name) && dir.is_dir() {
                env.push(((*name).to_owned(), dir.display().to_string()));
            }
        }
    }

    for (name, value) in [
        ("HOME", temp_root.join("home").display().to_string()),
        ("TMPDIR", temp_root.join("tmp").display().to_string()),
        ("CI", "1".to_owned()),
        ("PYTHONDONTWRITEBYTECODE", "1".to_owned()),
    ] {
        env.push((name.to_owned(), value));
    }
    env
}

/// Names that look like secrets, which never reach a check whatever an allowlist says.
pub fn looks_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_SUFFIXES.iter().any(|suffix| {
        upper == *suffix
            || upper
                .strip_suffix(suffix)
                .is_some_and(|rest| rest.ends_with('_'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn check_env_scrubbed() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".rustup")).unwrap();
        let real_home = home.path().display().to_string();
        let temp = Path::new("/tmp/argonauts-check-1");
        let run_host = vars(&[
            ("PATH", "/usr/bin:/bin"),
            ("HOME", &real_home),
            ("LANG", "en_US.UTF-8"),
            ("TERM", "xterm-256color"),
            ("TMPDIR", "/var/folders/x/T/"),
            ("USER", "me"),
            ("SSH_AUTH_SOCK", "/tmp/agent.sock"),
            ("ANTHROPIC_API_KEY", "sk-ant"),
            ("OPENAI_API_KEY", "sk-oai"),
            ("GOOGLE_APPLICATION_CREDENTIALS", "/me/gcp.json"),
            ("GH_TOKEN", "ghp"),
            ("AWS_SECRET_ACCESS_KEY", "aws"),
            ("DB_PASSWORD", "hunter2"),
            ("CLAUDE_CODE_MESSAGING_TOKEN", "t"),
            ("ARGONAUTS_SESSION", "s-1"),
            ("CARGO_HOME", "/opt/cargo"),
            ("CI", "false"),
        ]);
        let env = check_env(run_host.into_iter(), temp);
        let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            [
                "PATH",
                "LANG",
                "TERM",
                "CARGO_HOME",
                "RUSTUP_HOME",
                "HOME",
                "TMPDIR",
                "CI",
                "PYTHONDONTWRITEBYTECODE"
            ]
        );
        let get = |name: &str| {
            env.iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
                .unwrap()
        };
        assert_eq!(get("HOME"), "/tmp/argonauts-check-1/home");
        assert_eq!(get("TMPDIR"), "/tmp/argonauts-check-1/tmp");
        assert_eq!(get("CI"), "1");
        assert_eq!(get("CARGO_HOME"), "/opt/cargo");
        assert_eq!(
            get("RUSTUP_HOME"),
            home.path().join(".rustup").display().to_string()
        );
        for (_, value) in &env {
            for secret in ["sk-ant", "sk-oai", "ghp", "aws", "hunter2", "/me/gcp.json"] {
                assert_ne!(value, secret);
            }
        }
    }

    #[test]
    fn secret_names() {
        for name in [
            "OPENAI_API_KEY",
            "GITHUB_TOKEN",
            "client_secret",
            "DB_PASSWORD",
            "TOKEN",
        ] {
            assert!(looks_secret(name), "{name}");
        }
        for name in ["PATH", "KEYCHAIN", "MONKEY", "TOKENIZER_PATH", "HOME"] {
            assert!(!looks_secret(name), "{name}");
        }
    }
}
