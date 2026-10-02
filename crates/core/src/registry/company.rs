//! The company behind a model family. Families keep reviews and judging apart; companies keep
//! an approver or a reviewer from judging work its own maker trained for, since `gpt` and
//! `gpt-oss`, or `gemini` and `gemma`, are different families from one company.

/// Family prefixes and the company that makes them, checked in order.
const COMPANIES: &[(&str, &str)] = &[
    ("claude", "anthropic"),
    ("gpt", "openai"),
    ("gemini", "google"),
    ("gemma", "google"),
    ("glm", "zhipu"),
    ("deepseek", "deepseek"),
    ("qwen", "alibaba"),
    ("llama", "meta"),
    ("mistral", "mistral"),
    ("codestral", "mistral"),
    ("devstral", "mistral"),
    ("grok", "xai"),
    ("kimi", "moonshot"),
];

/// The company that makes `family`, lowercase. A family this table doesn't know is its own
/// company, so it only ever counts as the same company as itself. Empty for an empty family.
pub fn company_of(family: &str) -> String {
    let family = family.trim().to_ascii_lowercase();
    COMPANIES
        .iter()
        .find(|(prefix, _)| family.starts_with(prefix))
        .map_or(family.clone(), |(_, company)| (*company).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_of_one_maker_share_a_company() {
        assert_eq!(company_of("gpt"), company_of("gpt-oss"));
        assert_eq!(company_of("Gemini"), company_of("gemma"));
        assert_eq!(company_of("claude"), "anthropic");
        assert_ne!(company_of("claude"), company_of("gpt"));
    }

    #[test]
    fn an_unknown_family_is_its_own_company() {
        assert_eq!(company_of("mine"), "mine");
        assert_ne!(company_of("mine"), company_of("theirs"));
        assert_eq!(company_of(" "), "");
    }
}
