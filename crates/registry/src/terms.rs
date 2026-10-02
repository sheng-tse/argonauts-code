/// Shown once in `argonauts init`. It names no vendor on purpose.
pub const TERMS_NOTE: &str =
    "Automated use may be subject to your plan's terms; API keys work with every adapter.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_note_text_is_neutral() {
        assert_eq!(TERMS_NOTE.matches('.').count(), 1);
        assert!(TERMS_NOTE.ends_with('.'));
        for vendor in [
            "Claude",
            "Anthropic",
            "OpenAI",
            "Codex",
            "Google",
            "Gemini",
            "agy",
            "Antigravity",
            "OpenCode",
            "OpenRouter",
        ] {
            assert!(!TERMS_NOTE.contains(vendor), "{vendor}");
        }
    }
}
