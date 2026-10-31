//! Judging an installed CLI by what it offers, not by an exact version: probe `--help` for the
//! flags the adapter needs, and check the version against a range around the tested versions.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use argonauts_core::app::status_tab::VersionStanding;

use super::command::ask;
use super::standing::standing;

pub const HELP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FlagProbe {
    pub found: Vec<String>,
    pub missing: Vec<String>,
    /// Why the probe couldn't run, if it couldn't.
    pub problem: Option<String>,
}

impl FlagProbe {
    /// Every flag found, or nothing to look for.
    pub fn complete(&self) -> bool {
        self.missing.is_empty() && self.problem.is_none()
    }
}

// Checked against claude 2.1.285, codex-cli 0.158.0 (`codex exec --help`), and agy 1.2.13.
const CLAUDE_CODE_FLAGS: &[&str] = &[
    "--print",
    "--output-format",
    "--verbose",
    "--include-partial-messages",
    "--model",
    "--effort",
    "--permission-mode",
    "--permission-prompts",
    "--tools",
    "--disallowedTools",
    "--settings",
    "--setting-sources",
    "--mcp-config",
    "--strict-mcp-config",
    "--json-schema",
    "--max-budget-usd",
    "--resume",
    "--autocompact",
    "--name",
];
const CODEX_FLAGS: &[&str] = &[
    "--json",
    "--color",
    "--cd",
    "--sandbox",
    "--skip-git-repo-check",
    "--model",
    "--config",
    "--output-schema",
    "--disable",
];
const ANTIGRAVITY_FLAGS: &[&str] = &[
    "--print",
    "--output-format",
    "--model",
    "--effort",
    "--mode",
    "--sandbox",
    "--disable-slash-commands",
    "--json-schema",
    "--conversation",
];

/// The flags each built-in adapter relies on, by adapter id.
pub fn required_flags(adapter_id: &str) -> &'static [&'static str] {
    match adapter_id {
        "claude-code" => CLAUDE_CODE_FLAGS,
        "codex" => CODEX_FLAGS,
        "antigravity-cli" => ANTIGRAVITY_FLAGS,
        _ => &[],
    }
}

/// The help commands whose text lists those flags. Codex's session flags belong to `exec`.
fn help_commands(adapter_id: &str) -> &'static [&'static [&'static str]] {
    match adapter_id {
        "codex" => &[&["--help"], &["exec", "--help"]],
        _ => &[&["--help"]],
    }
}

/// Runs `<binary> --help` (and the subcommand help the adapter uses) and looks for each flag.
pub async fn probe_flags(binary: &Path, adapter_id: &str) -> FlagProbe {
    probe_flags_within(binary, adapter_id, HELP_TIMEOUT).await
}

pub async fn probe_flags_within(binary: &Path, adapter_id: &str, timeout: Duration) -> FlagProbe {
    let flags = required_flags(adapter_id);
    if flags.is_empty() {
        return FlagProbe::default();
    }
    let mut help = String::new();
    for args in help_commands(adapter_id) {
        match ask(binary, args, timeout).await {
            Ok(answer) => {
                help.push_str(&answer.text);
                help.push('\n');
            }
            Err(problem) => {
                return FlagProbe {
                    problem: Some(problem),
                    ..FlagProbe::default()
                };
            }
        }
    }
    find_flags(&help, flags)
}

/// Sorts `flags` into found and missing by whether `help` mentions each as a whole flag.
pub fn find_flags(help: &str, flags: &[&str]) -> FlagProbe {
    let (found, missing): (Vec<&str>, Vec<&str>) =
        flags.iter().partition(|flag| mentions(help, flag));
    FlagProbe {
        found: found.into_iter().map(str::to_owned).collect(),
        missing: missing.into_iter().map(str::to_owned).collect(),
        problem: None,
    }
}

fn mentions(help: &str, flag: &str) -> bool {
    let part_of_name = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    help.match_indices(flag).any(|(at, _)| {
        let before = help[..at].chars().next_back();
        let after = help[at + flag.len()..].chars().next();
        !before.is_some_and(part_of_name) && !after.is_some_and(part_of_name)
    })
}

/// How an installed CLI's version stands against the versions its adapter was recorded with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionFit {
    /// From the oldest recorded version to the newest.
    Tested,
    /// A later version of the newest recorded major, which should work.
    Newer,
    /// Older than any recording, a later major, or no version at all.
    Untested,
}

/// The fit by the one rule /doctor states a version's standing with.
pub fn version_fit(version: &str, tested: &[&str]) -> VersionFit {
    match standing(Some(version), tested, &[]) {
        VersionStanding::Checked => VersionFit::Tested,
        VersionStanding::Newer => VersionFit::Newer,
        _ => VersionFit::Untested,
    }
}

/// Whether `version` is tested or newer: from the oldest tested one up to, not including, the
/// next major version after the newest.
pub fn in_tested_range(version: &str, tested: &[&str]) -> bool {
    version_fit(version, tested) != VersionFit::Untested
}

/// The newest of `tested`, as written there.
pub fn newest_tested<'a>(tested: &[&'a str]) -> Option<&'a str> {
    tested.iter().copied().max_by_key(|v| numbers(v))
}

/// The range [`in_tested_range`] accepts, such as `2.1.284 up to 3.0`.
pub fn tested_range_label(tested: &[&str]) -> Option<String> {
    let tested: Vec<_> = tested.iter().filter_map(|v| numbers(v)).collect();
    let oldest = tested.iter().min()?;
    let newest = tested.iter().max()?;
    Some(format!(
        "{}.{}.{} up to {}.0",
        oldest.0,
        oldest.1,
        oldest.2,
        newest.0 + 1
    ))
}

/// `major.minor.patch`, with missing parts as 0 and anything after the numbers ignored.
pub(crate) fn numbers(version: &str) -> Option<(u64, u64, u64)> {
    let version = version.trim().trim_start_matches('v');
    let end = version
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(version.len());
    let mut parts = version[..end].split('.').filter(|p| !p.is_empty());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_run_from_the_oldest_to_the_next_major() {
        let tested = ["2.1.284", "2.1.290"];
        assert!(in_tested_range("2.1.284", &tested));
        assert!(in_tested_range("2.1.285", &tested));
        assert!(in_tested_range("2.1.999", &tested));
        assert!(!in_tested_range("2.1.283", &tested));
        assert!(in_tested_range("2.2.0", &tested));
        assert!(!in_tested_range("3.0.0", &tested));
        assert!(in_tested_range("v2.1.300-beta.1", &tested));
        assert!(!in_tested_range("banana", &tested));
        assert!(!in_tested_range("2.1.285", &[]));
        assert_eq!(
            tested_range_label(&tested).as_deref(),
            Some("2.1.284 up to 3.0")
        );
    }

    #[test]
    fn a_later_patch_or_minor_is_newer_and_a_later_major_untested() {
        let tested = ["2.1.284", "2.1.285", "2.1.289"];
        assert_eq!(version_fit("2.1.289", &tested), VersionFit::Tested);
        assert_eq!(version_fit("2.1.286", &tested), VersionFit::Tested);
        assert_eq!(version_fit("2.1.291", &tested), VersionFit::Newer);
        assert_eq!(version_fit("v2.1.300-beta.1", &tested), VersionFit::Newer);
        assert_eq!(version_fit("2.2.0", &tested), VersionFit::Newer);
        assert_eq!(version_fit("3.0.0", &tested), VersionFit::Untested);
        assert_eq!(version_fit("2.1.283", &tested), VersionFit::Untested);
        assert_eq!(version_fit("banana", &tested), VersionFit::Untested);
        assert_eq!(version_fit("2.1.289", &[]), VersionFit::Untested);
        assert_eq!(newest_tested(&tested), Some("2.1.289"));
        assert_eq!(newest_tested(&["0.9.0", "0.10.1"]), Some("0.10.1"));
    }

    #[test]
    fn flags_match_whole_words_only() {
        let help = "  -p, --print   Print\n  --disallowedTools, --disallowed-tools <tools...>\n  --model-name <x>\n  --json\n";
        let probe = find_flags(
            help,
            &["--print", "--disallowedTools", "--model", "--json", "--js"],
        );
        assert_eq!(probe.found, ["--print", "--disallowedTools", "--json"]);
        assert_eq!(probe.missing, ["--model", "--js"]);
        assert!(!probe.complete());
        assert!(required_flags("some-plugin").is_empty());
    }

    /// Splits one line of shell into words, handling single and double quotes.
    fn shell_words(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut word = String::new();
        let mut quote = None;
        let mut started = false;
        for c in line.chars() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), c) => word.push(c),
                (None, '\'' | '"') => {
                    quote = Some(c);
                    started = true;
                }
                (None, ' ') => {
                    if started {
                        words.push(std::mem::take(&mut word));
                        started = false;
                    }
                }
                (None, c) => {
                    word.push(c);
                    started = true;
                }
            }
        }
        if started {
            words.push(word);
        }
        words
    }

    #[test]
    fn rerecord_fixtures_script_uses_cheapest_models() {
        let script = include_str!("../../scripts/rerecord-fixtures.sh");

        // Every CLI call goes through a shim that allows only these.
        let shims: Vec<Vec<String>> = script
            .lines()
            .filter(|line| line.starts_with("write_shim "))
            .map(shell_words)
            .collect();
        let expected = [
            [
                "write_shim",
                "claude",
                "",
                "--model",
                "haiku",
                "--effort",
                "low",
            ],
            [
                "write_shim",
                "codex",
                "exec",
                "-m --model",
                "gpt-6-luna",
                "-c --config",
                "model_reasoning_effort=\"low\"",
            ],
            [
                "write_shim",
                "agy",
                "",
                "--model",
                "gemini-3.8-flash-low",
                "--effort",
                "low",
            ],
        ];
        assert_eq!(shims, expected);
        for cheapest in [
            "claude --model haiku --effort low",
            "codex exec -m gpt-6-luna -c model_reasoning_effort=\"low\"",
            "agy --model gemini-3.8-flash-low --effort low",
        ] {
            assert!(script.contains(cheapest), "{cheapest}");
        }

        // No other line names a model, and the shims come first on PATH.
        let code: Vec<&str> = script
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#') && !line.starts_with("write_shim "))
            .collect();
        for line in &code {
            for model in ["opus", "sonnet", "gpt-", "gemini-3", "--model ", "-m "] {
                assert!(!line.contains(model), "{line}");
            }
        }
        assert!(code.contains(&"export PATH=\"$shims:$PATH\""));
        assert!(code.contains(&"unset ARGONAUTS_CODEX_MODEL"));

        // It spends nothing unless asked to, and never works inside this repository.
        assert!(script.contains("pass --live to go ahead"));
        assert!(script.contains("must be outside"));
        assert!(script.contains("plugin | login | logout) refuse"));

        // It re-records each adapter's three recordings, into <scratch>/live/.
        assert!(script.contains("run_dir=\"$scratch/live/rerecord-"));
        for recording in [
            "argonauts-adapter-claude-code record_read_only record_write record_shell",
            "argonauts-adapter-codex record_1_read_only_ok record_2_build_hello record_3_resume_ls",
            "argonauts-adapter-antigravity-cli record_readonly_ok record_build_hello record_build_shell",
        ] {
            assert!(script.contains(recording), "{recording}");
        }
    }
}
