//! A card's owner: building, fixing a merge conflict, and the role prompt.

pub const OWNER: &str = include_str!("card/owner.md");
pub const BUILD: &str = include_str!("card/build.md");
pub const RESOLVE_CONFLICT: &str = include_str!("card/resolve_conflict.md");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_written() {
        for template in [OWNER, BUILD, RESOLVE_CONFLICT] {
            assert!(!template.contains("Not written yet"));
            assert!(!template.contains('\u{2014}'), "no em dashes");
        }
    }
}
