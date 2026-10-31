//! The manifest's `errors` patterns and `exit_codes`, compiled once.

use std::collections::HashMap;

use anyhow::{Context, anyhow};
use argonauts_core::ErrorCategory;
use argonauts_core::models::registry::CliManifest;
use regex::Regex;

pub(crate) struct ErrorRules {
    patterns: Vec<(Regex, ErrorCategory, bool)>,
    exit_codes: HashMap<i32, ErrorCategory>,
}

impl ErrorRules {
    /// Refuses a pattern that doesn't compile or an exit code that isn't a number, so
    /// `command` can say so before anything runs.
    pub(crate) fn compile(manifest: &CliManifest) -> anyhow::Result<Self> {
        let patterns = manifest
            .errors
            .iter()
            .map(|rule| {
                Regex::new(&rule.pattern)
                    .map(|regex| (regex, rule.category, rule.retryable))
                    .with_context(|| format!("error pattern {:?}", rule.pattern))
            })
            .collect::<anyhow::Result<_>>()?;
        let exit_codes = manifest
            .exit_codes
            .iter()
            .map(|(code, category)| {
                code.trim()
                    .parse()
                    .map(|code| (code, *category))
                    .map_err(|_| anyhow!("exit code {code:?} isn't a number"))
            })
            .collect::<anyhow::Result<_>>()?;
        Ok(Self {
            patterns,
            exit_codes,
        })
    }

    /// Keeps the rules that compile. `command` has already refused a manifest with any other.
    pub(crate) fn lenient(manifest: &CliManifest) -> Self {
        Self {
            patterns: manifest
                .errors
                .iter()
                .filter_map(|rule| {
                    let regex = Regex::new(&rule.pattern).ok()?;
                    Some((regex, rule.category, rule.retryable))
                })
                .collect(),
            exit_codes: manifest
                .exit_codes
                .iter()
                .filter_map(|(code, category)| Some((code.trim().parse().ok()?, *category)))
                .collect(),
        }
    }

    /// The first pattern that matches, as (category, retryable).
    pub(crate) fn classify(&self, text: &str) -> Option<(ErrorCategory, bool)> {
        self.patterns
            .iter()
            .find(|(regex, _, _)| regex.is_match(text))
            .map(|(_, category, retryable)| (*category, *retryable))
    }

    pub(crate) fn exit_category(&self, code: i32) -> Option<ErrorCategory> {
        self.exit_codes.get(&code).copied()
    }
}
