//! Live runs of `argonauts run` against the real agent CLIs. They spend quota, so each one is
//! ignored and runs only when `ARGONAUTS_RECORD` names it and `ARGONAUTS_LIVE_DIR` is an
//! absolute scratch folder outside any repository you care about.
//!
//! ```sh
//! ARGONAUTS_LIVE_DIR=/tmp/argonauts-live ARGONAUTS_RECORD=live_claude_code \
//!   cargo test -p argonauts-cli --test live -- --ignored live_claude_code
//! ```
//!
//! `ARGONAUTS_CODEX_MODEL` may switch Codex to its fallback cheapest model.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use argonauts_adapter_antigravity_cli::AntigravityCli;
use argonauts_adapter_claude_code::ClaudeCodeAdapter;
use argonauts_adapter_codex::{CodexAdapter, FALLBACK_CHEAPEST_MODEL};
use argonauts_core::Adapter;

const TASK: &str = "Create a file named hello.txt containing the word hi";

#[test]
#[ignore = "live call; see the module docs"]
fn live_claude_code() {
    // Claude Code reports dollars, so its native cap backs up a small card budget.
    live(
        "live_claude_code",
        &ClaudeCodeAdapter,
        None,
        &["--budget-usd", "0.5"],
    );
}

#[test]
#[ignore = "live call; see the module docs"]
fn live_codex() {
    let model = std::env::var("ARGONAUTS_CODEX_MODEL").ok();
    if let Some(model) = &model {
        assert_eq!(
            model, FALLBACK_CHEAPEST_MODEL,
            "live runs use only the cheapest models"
        );
    }
    live("live_codex", &CodexAdapter, model, TOKEN_BUDGET);
}

#[test]
#[ignore = "live call; see the module docs"]
fn live_antigravity_cli() {
    live("live_antigravity_cli", &AntigravityCli, None, TOKEN_BUDGET);
}

/// Codex and Antigravity CLI report tokens only, so their budget counts tokens.
const TOKEN_BUDGET: &[&str] = &[
    "--set",
    "budget.unit=tokens",
    "--set",
    "budget.per_card=200000",
];

fn live(test: &str, adapter: &dyn Adapter, model: Option<String>, budget: &[&str]) {
    let Some(root) = live_dir(test) else {
        eprintln!("skipping {test}: set ARGONAUTS_RECORD={test} and ARGONAUTS_LIVE_DIR");
        return;
    };
    let (cheapest, effort) = adapter.cheapest_model();
    let model = model.unwrap_or_else(|| cheapest.to_owned());
    let effort = effort.unwrap_or("low");
    let dir = root.join(format!("e2e-{}-{}", adapter.id(), unix_seconds()));
    let repo = dir.join("repo");
    init_repo(&repo);

    let mut args = vec![
        "run",
        "--solo",
        "--plain",
        "--agent",
        adapter.id(),
        "--model",
        &model,
        "--effort",
        effort,
        "--merge",
        "leave",
        "--gate",
        "test -f hello.txt",
    ];
    args.extend_from_slice(budget);
    args.push(TASK);
    let output = run_and_save_output(&dir, &repo, &args);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(output.status.code(), Some(0), "see {}", dir.display());
    assert!(
        stdout.contains("gate passed: test -f hello.txt"),
        "{stdout}"
    );
    let branch = git(
        &repo,
        &[
            "branch",
            "--list",
            "argonauts/*",
            "--format=%(refname:short)",
        ],
    );
    let committed = git(&repo, &["show", &format!("{}:hello.txt", branch.trim())]);
    assert_eq!(committed.trim(), "hi");
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn init_repo(repo: &Path) {
    fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "-q", "-b", "main"]);
    fs::write(
        repo.join("README.md"),
        "# Throwaway repo for one live run\n",
    )
    .unwrap();
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-q", "-m", "initial"]);
}

fn run_and_save_output(dir: &Path, repo: &Path, args: &[&str]) -> Output {
    // The agent CLIs need the real HOME and PATH to find their logins.
    let output = Command::new(env!("CARGO_BIN_EXE_argonauts"))
        .args(args)
        .current_dir(repo)
        .env("ARGONAUTS_HOME", dir.join("argonauts-home"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    fs::write(dir.join("run.stdout"), stdout.as_bytes()).unwrap();
    fs::write(dir.join("run.stderr"), stderr.as_bytes()).unwrap();
    println!("{stdout}\n--- stderr\n{stderr}");
    output
}

fn live_dir(test: &str) -> Option<PathBuf> {
    if std::env::var("ARGONAUTS_RECORD").ok()? != test {
        return None;
    }
    let dir = PathBuf::from(std::env::var_os("ARGONAUTS_LIVE_DIR")?);
    assert!(dir.is_absolute(), "ARGONAUTS_LIVE_DIR must be absolute");
    Some(dir)
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "argonauts-live")
        .env("GIT_AUTHOR_EMAIL", "argonauts-live@localhost")
        .env("GIT_COMMITTER_NAME", "argonauts-live")
        .env("GIT_COMMITTER_EMAIL", "argonauts-live@localhost")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}
