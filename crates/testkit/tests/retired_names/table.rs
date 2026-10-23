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
    /// The files under a fixtures or test data folder that were written here, not recorded.
    pub made_here: Vec<String>,
    pub bundled_text: BundledText,
}

/// What a recording keeps of a file a CLI ships with itself: a placeholder, never the text.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundledText {
    /// The folder a CLI unpacks the skills it bundles into.
    pub folder: String,
    /// The tools whose result is the text of the file they read.
    pub read_tools: Vec<String>,
    /// How the text a Skill call launches starts, followed by the skill's folder.
    pub skill_preamble: String,
    /// The line that stands in for all but the file's first heading.
    pub note: String,
    pub fix: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Private {
    pub neutral_homes: Vec<String>,
    pub neutral_mail_endings: Vec<String>,
    /// An address with one of these as its whole name takes no mail, so it names no one.
    pub no_reply: Vec<String>,
    /// GitHub's domain for the address of a login that hides its own.
    pub github_no_reply: String,
    pub file_endings: Vec<String>,
    /// Files whose addresses are their authors' own, in the copyright lines of the licenses
    /// they hold. Home and scratch folders are still looked for there.
    #[serde(default)]
    pub authors_addresses_in: Vec<String>,
    pub home_fix: String,
    pub mail_fix: String,
    pub scratch_fix: String,
    pub temp_fix: String,
    /// The one plan a recording or a sample of a CLI's account names.
    pub neutral_plan: String,
    pub plan_fix: String,
    /// Keys that hold Codex's plan, read in every file.
    pub plan_keys: Vec<String>,
    /// Keys that hold any CLI's plan, read in recordings.
    pub recorded_plan_keys: Vec<String>,
    /// What a CLI prints before the plan in its own words.
    pub plan_labels: Vec<String>,
    pub time_fix: String,
    /// File names an OS writes into a folder.
    pub machine_files: Vec<String>,
    pub machine_file_fix: String,
    /// The chunks of a PNG that hold text or Exif.
    pub picture_text_chunks: Vec<String>,
    pub picture_text_fix: String,
    pub ids: Ids,
    pub banned: Vec<BannedText>,
    pub contact: Contact,
    pub setup: Setup,
}

/// The keys of the ids that tie a recording to an account or an install.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ids {
    pub keys: Vec<String>,
    pub fix: String,
}

/// The placeholders a recording has in place of the recording machine's own setup.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setup {
    /// What a synced skill's name starts with, `anthropic-skills:` included.
    pub synced_skill: String,
    /// What a synced skill says of itself, in the commands an initialize reply lists.
    pub synced_skill_description: String,
    pub plugin: String,
    /// A claude.ai connector's name, `claude.ai ` included.
    pub connector: String,
    /// What a connector's tools start with.
    pub connector_tool: String,
    /// What each of a connector's tools is named after its connector's placeholder and `__`.
    pub connector_tool_name: String,
    pub organization: String,
    /// The keys an account's organization is under.
    pub organization_keys: Vec<String>,
    /// The key beside which a machine name is the recording machine's.
    pub machine_name_beside: String,
    pub machine_name_key: String,
    pub machine_names: Vec<String>,
    pub machine_name_fix: String,
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

/// The maintainer's address, kept as its length and hash like a banned text. It is a real
/// address on purpose, where the community files say to send a report, and passes nowhere
/// else.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contact {
    pub length: usize,
    pub hash: String,
    /// The files that give it, as path patterns.
    pub given_in: Vec<String>,
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

/// The docs that describe the product as it is, from the table kept beside them in the
/// folder `ARGONAUTS_DOCS` names.
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

/// The living docs' own table, in the docs folder, since the names it lists are the docs'.
pub const LIVING_DOCS: &str = "living-docs.toml";

/// The living docs' table in `docs`, the folder `ARGONAUTS_DOCS` names.
pub fn living_docs(docs: &Path) -> LivingDocs {
    let path = docs.join(LIVING_DOCS);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
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
