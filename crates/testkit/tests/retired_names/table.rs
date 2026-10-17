//! `retired-names.toml` as the checks read it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The table's place in the workspace, as the report names it.
pub const TABLE: &str = "crates/testkit/retired-names.toml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    #[serde(default)]
    pub word: Vec<Word>,
    #[serde(default)]
    pub phrase: Vec<Phrase>,
    #[serde(default)]
    pub pattern: Vec<Pattern>,
    #[serde(default)]
    pub allow: Vec<Allow>,
    #[serde(default)]
    pub unread: Vec<Unread>,
    pub recordings: Recordings,
    pub private: Private,
    pub serde_keys: SerdeKeys,
    pub slash_commands: SlashCommands,
    pub fixture_folders: FixtureFolders,
    pub living_docs: LivingDocs,
    pub report_example: ReportExample,
}

/// Retired single words, in every listed form, with the name that replaced them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Word {
    pub old: Vec<String>,
    pub new: String,
    /// Where the word keeps a meaning of its own, as path patterns.
    #[serde(default)]
    pub allowed_in: Vec<String>,
}

/// Retired runs of words. A word with a plain sense of its own is banned only this way.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Phrase {
    pub old: Vec<String>,
    pub new: String,
}

/// Retired spellings a word list can't catch, as a regular expression over the raw line.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pattern {
    pub old: String,
    pub new: String,
    /// The only files the pattern is read in. Everywhere when empty.
    #[serde(default)]
    pub only_in: Vec<String>,
}

/// One exact text that may spell a retired name, in the files a path pattern names, with the
/// reason.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allow {
    pub path: String,
    pub text: String,
    pub why: String,
}

/// A file whose words aren't read at all, with the reason.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unread {
    pub path: String,
    pub why: String,
}

/// What a CLI itself wrote. Its words are the CLI's, so only its name is checked.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recordings {
    pub endings_under_fixtures: Vec<String>,
    pub also: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Private {
    pub neutral_homes: Vec<String>,
    pub neutral_mail_endings: Vec<String>,
    pub file_endings: Vec<String>,
    pub home_fix: String,
    pub mail_fix: String,
    pub scratch_fix: String,
    pub banned: Vec<BannedText>,
    pub setup: Setup,
}

/// The placeholders a recording has in place of the recording machine's own setup.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setup {
    /// What a synced skill's name starts with, `anthropic-skills:` included.
    pub synced_skill: String,
    pub plugin: String,
    /// A claude.ai connector's name, `claude.ai ` included.
    pub connector: String,
    /// What a connector's tools start with.
    pub connector_tool: String,
    pub plan: String,
    pub organization: String,
    pub fix: String,
    /// Every skill, command and agent an init message may list without its machine's setup.
    pub known: Vec<String>,
}

/// A real name that must never be in a file, kept as its length and hash so the table
/// doesn't hold it either.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BannedText {
    pub what: String,
    /// What to write in its place.
    pub fix: String,
    pub length: usize,
    pub hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerdeKeys {
    pub not_ours: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlashCommands {
    pub table: String,
    pub variant_of: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureFolders {
    pub models: Vec<String>,
    pub numbered: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LivingDocs {
    pub files: Vec<String>,
    /// What a doc may still spell: a product of someone else's by its own name, and a dated
    /// file by the name it was given.
    #[serde(default)]
    pub allow: Vec<DocsAllow>,
}

/// One text the living docs keep, with the reason.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocsAllow {
    pub text: String,
    pub why: String,
}

/// The picture of a failing run: three places, and the lines they print.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportExample {
    pub places: Vec<ExamplePlace>,
    pub prints: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExamplePlace {
    pub path: String,
    pub line: usize,
    pub word: String,
}

pub fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn load() -> Table {
    let path = workspace().join(TABLE);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("{TABLE}: {e}"))
}

impl Table {
    /// The replacement for a retired word or phrase, spelled as the table spells it.
    pub fn replacement(&self, old: &str) -> Option<&str> {
        let words = self
            .word
            .iter()
            .map(|entry| (&entry.old, entry.new.as_str()));
        let phrases = self
            .phrase
            .iter()
            .map(|entry| (&entry.old, entry.new.as_str()));
        words
            .chain(phrases)
            .find(|(forms, _)| forms.iter().any(|form| form == old))
            .map(|(_, new)| new)
    }
}

/// Whether a path fits a pattern: `*` stands for anything inside one folder name, `**` for
/// anything at all.
pub fn fits(pattern: &str, path: &str) -> bool {
    fn step(pattern: &[u8], path: &[u8]) -> bool {
        match pattern {
            [] => path.is_empty(),
            [b'*', b'*', rest @ ..] => {
                let rest = rest.strip_prefix(b"/").unwrap_or(rest);
                (0..=path.len()).any(|at| step(rest, &path[at..]))
            }
            [b'*', rest @ ..] => (0..=path.len())
                .take_while(|at| *at == 0 || path[at - 1] != b'/')
                .any(|at| step(rest, &path[at..])),
            [first, rest @ ..] => path.first() == Some(first) && step(rest, &path[1..]),
        }
    }
    step(pattern.as_bytes(), path.as_bytes())
}

pub fn fits_any(patterns: &[String], path: &str) -> bool {
    patterns.iter().any(|pattern| fits(pattern, path))
}
