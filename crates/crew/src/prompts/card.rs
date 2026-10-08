//! A task's worker: the report it ends with, and fixing a merge conflict. What it may touch is
//! its frame, `frames/workers.md`.

pub const BUILD: &str = include_str!("card/build.md");
pub const RESOLVE_CONFLICT: &str = include_str!("card/resolve_conflict.md");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_written() {
        for template in [BUILD, RESOLVE_CONFLICT] {
            assert!(!template.contains("Not written yet"));
            assert!(!template.contains('\u{2014}'), "no em dashes");
        }
    }
}
