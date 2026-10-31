//! The words Claude Code treats specially in a prompt, as 2.1.289 has them.
//!
//! `ultrathink` asks for deeper reasoning on the turn. Claude Code finds it with
//! `/\bultrathink\b/gi`, draws each letter in its rainbow with a shimmer in the input, and draws
//! the echo of your message in the same rainbow, held still. The CLI reacts to the word in the
//! prompt itself, so it goes out as typed.
//!
//! Three more words get their own treatment in 2.1.289, and none fits a session Argonauts Code
//! runs: `ultraplan` launches a cloud session from the interactive screen only (and is off unless
//! Anthropic's server turns it on), `ultrareview` only adds a footer note for accounts that have
//! the cloud review, and `ultracode` asks for a workflow only from a prompt Claude Code saw a
//! person type into its own input, which a prompt sent over `stream-json` isn't shown to be.

use argonauts_core::look::keywords::{Keyword, KeywordLook};

pub const KEYWORDS: &[Keyword] = &[Keyword::new("ultrathink", KeywordLook::Rainbow, true)];

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use argonauts_core::agents::cli_commands::CliCommandSet;
    use argonauts_core::agents::cli_sessions::CliScan;
    use argonauts_core::look::keywords::find;
    use argonauts_core::{Adapter, SessionRequest};

    use super::*;
    use crate::ClaudeCodeAdapter;

    const PROMPT: &str = "please ultrathink about this";

    #[test]
    fn the_adapter_offers_its_table() {
        assert_eq!(ClaudeCodeAdapter.keywords(), KEYWORDS);
        let scan = CliScan::new("/nonexistent/repo", "/nonexistent/home");
        let set = CliCommandSet::of(&ClaudeCodeAdapter, &scan).unwrap();
        assert_eq!(set.keywords, KEYWORDS);
        let found = find(PROMPT, ClaudeCodeAdapter.keywords());
        assert_eq!(found.len(), 1);
        assert_eq!(&PROMPT[found[0].range.clone()], "ultrathink");
    }

    #[test]
    fn the_prompt_sent_keeps_the_word() {
        let adapter = ClaudeCodeAdapter;
        let line = adapter.input_line(PROMPT).unwrap();
        let sent: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(sent["message"]["content"], PROMPT);

        for keep_input_open in [false, true] {
            let spec = adapter
                .command(&SessionRequest {
                    workdir: PathBuf::from("/tmp/worktree"),
                    prompt: PROMPT.into(),
                    keep_input_open,
                    ..SessionRequest::default()
                })
                .unwrap();
            let stdin = spec.stdin.unwrap_or_default();
            assert!(stdin.contains(PROMPT), "{stdin}");
        }
    }
}
