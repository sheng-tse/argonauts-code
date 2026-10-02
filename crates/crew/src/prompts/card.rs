//! A card's owner: building, fixing a merge conflict, and the role prompt.

pub const OWNER: &str = include_str!("card/owner.md");
/// The owner's opening line when other agents own other cards of the run.
pub const OWNER_OF_ONE_CARD: &str = "You own one card of a larger change. Other agents own the \
    other cards, each in its own git worktree, and a conductor coordinates the run.\n\n";
/// The opening line when the run has this one card.
pub const OWNER_OF_THE_CHANGE: &str = "You make this change on your own, in its own git \
    worktree, and a conductor coordinates the run.\n\n";
pub const BUILD: &str = include_str!("card/build.md");
pub const RESOLVE_CONFLICT: &str = include_str!("card/resolve_conflict.md");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_written() {
        for template in [
            OWNER,
            OWNER_OF_ONE_CARD,
            OWNER_OF_THE_CHANGE,
            BUILD,
            RESOLVE_CONFLICT,
        ] {
            assert!(!template.contains("Not written yet"));
            assert!(!template.contains('\u{2014}'), "no em dashes");
        }
    }
}
