//! The provider behind a model is the maker that trained it. Reviews, judging and approvals
//! keep apart by provider, so `gpt` and `gpt-oss`, or `gemini` and `gemma`, count as one.

use crate::base::text::shown_name;

/// The provider a registry can't tell.
pub const UNKNOWN_PROVIDER: &str = "unknown";

/// Providers whose name isn't their id with a capital letter.
const NAMES: &[(&str, &str)] = &[
    ("openai", "OpenAI"),
    ("xai", "xAI"),
    ("deepseek", "DeepSeek"),
    ("ibm", "IBM"),
    ("nvidia", "NVIDIA"),
    ("minimax", "MiniMax"),
    ("bigcode", "BigCode"),
    ("tii", "TII"),
    ("lg", "LG"),
    ("huggingface", "Hugging Face"),
    ("shanghai-ai-lab", "Shanghai AI Lab"),
];

/// A provider as people write it, `OpenAI` for `openai`.
pub fn provider_name(provider: &str) -> String {
    NAMES
        .iter()
        .find(|(id, _)| *id == provider)
        .map_or_else(|| shown_name(provider), |(_, name)| (*name).to_owned())
}

/// Whether `provider` names one, rather than nothing or a provider no one could tell.
pub fn is_known(provider: &str) -> bool {
    !provider.is_empty() && provider != UNKNOWN_PROVIDER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_goes_by_its_own_spelling() {
        assert_eq!(provider_name("openai"), "OpenAI");
        assert_eq!(provider_name("anthropic"), "Anthropic");
        assert_eq!(provider_name("huggingface"), "Hugging Face");
    }

    #[test]
    fn unknown_and_empty_providers_are_not_known() {
        assert!(is_known("google"));
        assert!(!is_known(UNKNOWN_PROVIDER));
        assert!(!is_known(""));
    }
}
