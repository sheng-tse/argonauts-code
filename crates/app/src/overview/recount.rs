//! A second count of each CLI's tokens, written apart from the import on purpose. Claude Code's
//! and Codex's adapters count their own files by the plainest rules, so a rule the import gets
//! wrong shows up as a difference; every other CLI is counted from the usage ledger's lines.
//!
//! Doctor puts its totals beside the Overview's, and the import's tests check that the two agree
//! to the token on every folder they read.

use std::collections::BTreeMap;
use std::path::Path;

use argonauts_adapter_claude_code::tokens as claude_code;
use argonauts_adapter_codex::tokens as codex;
use time::Date;

use crate::usage::usage_ledger::LedgerLine;

pub(crate) const CLAUDE_CODE: &str = "claude-code";
pub(crate) const CODEX: &str = "codex";

/// Tokens by day, each day on UTC's clock for the CLIs read from their own files and on yours
/// for the ledger's, as each kind of record dates its work.
pub(crate) type Days = BTreeMap<Date, u64>;

/// Where the second count reads from. A CLI folder left out isn't read.
pub(crate) struct Folders<'a> {
    pub claude: Option<&'a Path>,
    pub codex: Option<&'a Path>,
    /// The `zstd` program, for Codex's compressed rollouts.
    pub zstd: Option<&'a Path>,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Recount {
    /// Each CLI's id to its tokens by day.
    pub clis: BTreeMap<String, Days>,
    /// Session files the count couldn't open, such as a compressed rollout without `zstd`.
    pub unopened: u32,
}

impl Recount {
    pub fn total(&self, cli: &str) -> u64 {
        self.clis.get(cli).map_or(0, |days| days.values().sum())
    }
}

/// Counts every CLI again: Claude Code and Codex from their folders, the rest from `ledger`. A
/// CLI whose own files gave tokens isn't counted from the ledger too, as its files already
/// hold what ran here.
pub(crate) fn recount(folders: &Folders, ledger: &[LedgerLine]) -> Recount {
    let mut out = Recount::default();
    if let Some(dir) = folders.claude {
        let days = claude_code::by_day(dir, &mut out.unopened);
        if !days.is_empty() {
            out.clis.insert(CLAUDE_CODE.to_owned(), days);
        }
    }
    if let Some(dir) = folders.codex {
        let days = codex::by_day(dir, folders.zstd, &mut out.unopened);
        if !days.is_empty() {
            out.clis.insert(CODEX.to_owned(), days);
        }
    }
    let own_files: Vec<String> = out.clis.keys().cloned().collect();
    for line in ledger {
        if own_files.contains(&line.agent) {
            continue;
        }
        let tokens = line.input + line.output;
        if tokens > 0 {
            let days = out.clis.entry(line.agent.clone()).or_default();
            *days.entry(line.day).or_default() += tokens;
        }
    }
    out
}
