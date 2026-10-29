//! The repository's own words that hold a name: a tracked path's, as `codex` in
//! `crates/codex-adapter/`, and an identifier's in its code, as `Claude` in `ClaudeSession`.
//! The talk may be about them, so they stay as written.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use regex::Regex;

use super::Lexicon;

/// The CLIs' own instruction and settings files, and Argonauts' own folder. Most repositories
/// hold them whatever they are about, and they name a CLI or a model by design.
const NOT_THE_TOPIC: &[&str] = &[
    "**/CLAUDE.md",
    "**/CLAUDE.local.md",
    "**/AGENTS.md",
    "**/GEMINI.md",
    "**/opencode.json",
    "**/opencode.jsonc",
    "**/.claude/**",
    "**/.codex/**",
    "**/.gemini/**",
    "**/.opencode/**",
    "**/.argonauts/**",
];

/// How long the search of the code may take. Past it, the paths' words alone count.
const SEARCH_TIME: Duration = Duration::from_secs(3);

/// What each checkout gave. A run's base checkout stays at one commit, so it's searched once.
static FOUND: Mutex<BTreeMap<PathBuf, Arc<[String]>>> = Mutex::new(BTreeMap::new());

/// The words of `checkout`'s tracked paths and of its code's identifiers that hold one of
/// `lexicon`'s names, with their phrases: `claude-code` gives `claude` and `claude code`.
/// Empty outside a git checkout.
pub(super) fn words(checkout: &Path, lexicon: &Lexicon) -> Arc<[String]> {
    if let Some(found) = found().get(checkout) {
        return found.clone();
    }
    let Some(paths) = tracked_paths(checkout) else {
        return Arc::from([]);
    };
    let mut words = BTreeSet::new();
    for segment in paths.iter().flat_map(|path| path.split('/')) {
        add_words(segment, lexicon, &mut words);
    }
    for identifier in identifiers(checkout, lexicon) {
        // A name on its own, as `GPT-5` in a sentence, is no identifier.
        let whole = lexicon
            .every_name(&identifier)
            .iter()
            .any(|hit| hit.range == (0..identifier.len()));
        if !whole {
            add_words(&identifier, lexicon, &mut words);
        }
    }
    let words: Arc<[String]> = words.into_iter().collect();
    found().insert(checkout.to_owned(), words.clone());
    words
}

fn found() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, Arc<[String]>>> {
    FOUND.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `token`'s words and their phrase, each kept when it holds a name.
fn add_words(token: &str, lexicon: &Lexicon, words: &mut BTreeSet<String>) {
    let split = split_words(token);
    let phrase = (split.len() > 1).then(|| split.join(" "));
    for word in split.into_iter().map(str::to_owned).chain(phrase) {
        if !words.contains(&word) && !lexicon.every_name(&word).is_empty() {
            words.insert(word);
        }
    }
}

/// `token` cut into words at `-`, `_`, `.` and every other mark, and where a capital starts
/// a word: `ClaudeSession` reads `Claude Session`, `HTTPServer` reads `HTTP Server`. Digits
/// stay with the letters before them, as a version does in `gpt4`.
fn split_words(token: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = token.char_indices().collect();
    let mut words = Vec::new();
    let mut start: Option<usize> = None;
    for (n, &(at, c)) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if let Some(from) = start.take() {
                words.push(&token[from..at]);
            }
            continue;
        }
        let Some(from) = start else {
            start = Some(at);
            continue;
        };
        let before = chars[n - 1].1;
        let after = chars.get(n + 1).map(|&(_, c)| c);
        let starts_a_word = c.is_uppercase()
            && (before.is_lowercase()
                || before.is_ascii_digit()
                || (before.is_uppercase() && after.is_some_and(char::is_lowercase)));
        if starts_a_word {
            words.push(&token[from..at]);
            start = Some(at);
        }
    }
    if let Some(from) = start {
        words.push(&token[from..]);
    }
    words
}

/// What git tracks in `checkout`, leaving out the CLIs' own files. `None` when git can't say.
fn tracked_paths(checkout: &Path) -> Option<Vec<String>> {
    let output = git(checkout)
        .args(["ls-files", "-z", "--"])
        .args(pathspecs())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    Some(
        String::from_utf8_lossy(&output.stdout)
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// Every identifier in the tracked code that holds a one-word name, as `codex_adapter` or
/// `ClaudeSession`. git finds the lines that hold a name as plain words, which is fast, and
/// the identifiers are cut from them here; the search stops at `SEARCH_TIME`.
fn identifiers(checkout: &Path, lexicon: &Lexicon) -> BTreeSet<String> {
    let stems: BTreeSet<String> = lexicon
        .names
        .iter()
        .filter(|name| {
            !name.case_sensitive && name.words.chars().all(|c| c.is_ascii_alphanumeric())
        })
        .map(|name| name.words.to_ascii_lowercase())
        .collect();
    if stems.is_empty() {
        return BTreeSet::new();
    }
    let alternatives = stems
        .iter()
        .map(|stem| regex::escape(stem))
        .collect::<Vec<_>>();
    let Ok(identifier) = Regex::new(&format!(
        "(?i)[a-z0-9_-]*(?:{})[a-z0-9_-]*",
        alternatives.join("|")
    )) else {
        return BTreeSet::new();
    };
    let mut search = git(checkout);
    search.args(["grep", "-I", "-h", "-i", "-F", "--no-color"]);
    for stem in &stems {
        search.arg("-e").arg(stem);
    }
    let child = search
        .arg("--")
        .args(pathspecs())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return BTreeSet::new();
    };
    let Some(stdout) = child.stdout.take() else {
        return BTreeSet::new();
    };
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let mut found = BTreeSet::new();
        for line in BufReader::new(stdout).split(b'\n') {
            let Ok(line) = line else { break };
            let line = String::from_utf8_lossy(&line);
            for token in identifier.find_iter(&line) {
                if !found.contains(token.as_str()) {
                    found.insert(token.as_str().to_owned());
                }
            }
        }
        let _ = send.send(found);
    });
    let found = receive.recv_timeout(SEARCH_TIME).unwrap_or_default();
    // Past its time the search is stopped, which ends the reading thread too.
    let _ = child.kill();
    let _ = child.wait();
    found
}

fn git(checkout: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(checkout).stdin(Stdio::null());
    command
}

fn pathspecs() -> impl Iterator<Item = String> {
    std::iter::once(".".to_owned()).chain(
        NOT_THE_TOPIC
            .iter()
            .map(|glob| format!(":(exclude,glob){glob}")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_splits_at_marks_and_capitals() {
        assert_eq!(split_words("ClaudeSession"), ["Claude", "Session"]);
        assert_eq!(split_words("HTTPServer"), ["HTTP", "Server"]);
        assert_eq!(split_words("claude-code"), ["claude", "code"]);
        assert_eq!(
            split_words("gpt4_tokenizer.rs"),
            ["gpt4", "tokenizer", "rs"]
        );
        assert_eq!(split_words("Qwen3Coder"), ["Qwen3", "Coder"]);
        assert!(split_words("--").is_empty());
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn write(dir: &Path, path: &str, text: &str) {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    #[test]
    fn paths_and_identifiers_give_words_and_the_clis_own_files_dont() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git_in(root, &["init", "-q"]);
        write(root, "crates/codex-adapter/src/lib.rs", "pub fn run() {}\n");
        write(root, "src/session.rs", "pub struct ClaudeSession;\n");
        write(root, "README.md", "Drafted with GPT-5 and Kimi.\n");
        write(root, "AGENTS.md", "Use qwen_runner for local work.\n");
        write(root, ".argonauts/config.toml", "main = \"opus-fast\"\n");
        write(root, ".claude/agents/gemma-helper.md", "Helps.\n");
        write(root, "untracked/llama_box.rs", "pub struct LlamaBox;\n");
        git_in(
            root,
            &[
                "add",
                "crates",
                "src",
                "README.md",
                "AGENTS.md",
                ".argonauts",
                ".claude",
            ],
        );

        let lexicon = Lexicon::built_in();
        let words = words(root, &lexicon);
        let lexicon = words.iter().fold(Lexicon::built_in(), |lexicon, word| {
            lexicon.with_topic(word)
        });
        for topic in ["the codex adapter", "the Claude session"] {
            assert!(lexicon.find(topic).is_empty(), "{topic}: {words:?}");
        }
        for named in ["GPT", "Kimi", "qwen", "opus", "gemma", "llama"] {
            assert!(!lexicon.find(named).is_empty(), "{named}: {words:?}");
        }
        assert!(Arc::ptr_eq(
            &super::words(root, &Lexicon::built_in()),
            &words
        ));
    }

    #[test]
    fn outside_git_there_are_no_words() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "codex/lib.rs", "pub struct ClaudeSession;\n");
        assert!(words(dir.path(), &Lexicon::built_in()).is_empty());
    }
}
