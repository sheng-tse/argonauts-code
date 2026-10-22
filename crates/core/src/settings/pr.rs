//! Following the pull requests Argonauts Code opened (`[pr]`): a failed check or a review
//! comment goes back to the task's builder and reviewer, round after round, until it passes or
//! the rounds run out and it asks you.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PrSettings {
    /// How many rounds of fixes a pull request gets from its checks and comments before
    /// Argonauts Code asks you what to do
    pub max_rounds: u32,
    /// Review bots whose comments go back to the builder as a maintainer's do, such as
    /// `coderabbitai[bot]`. Comments by anyone else who can't write to the repository are left
    /// out and counted
    pub review_bots: Vec<String>,
}

impl Default for PrSettings {
    fn default() -> Self {
        Self {
            max_rounds: 3,
            review_bots: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pull_request_gets_three_rounds_of_fixes() {
        assert_eq!(PrSettings::default().max_rounds, 3);
        let five: PrSettings = toml::from_str("max_rounds = 5").unwrap();
        assert_eq!(five.max_rounds, 5);
        assert!(toml::from_str::<PrSettings>("max_rounds = -1").is_err());
    }

    #[test]
    fn no_review_bot_is_trusted_until_you_name_it() {
        assert!(PrSettings::default().review_bots.is_empty());
        let named: PrSettings = toml::from_str("review_bots = [\"coderabbitai[bot]\"]").unwrap();
        assert_eq!(named.review_bots, ["coderabbitai[bot]"]);
    }
}
