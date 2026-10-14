//! The merge agent: bringing in a branch that moved while the run worked, fixing what that
//! broke, and the message the change lands with.

pub const MAIN_MOVED: &str = include_str!("merge/main_moved.md");
pub const COMMIT_MESSAGE: &str = include_str!("merge/commit_message.md");
pub const AFTER_MAIN: &str = include_str!("merge/after_main.md");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_written() {
        for template in [MAIN_MOVED, COMMIT_MESSAGE, AFTER_MAIN] {
            assert!(!template.contains('\u{2014}'), "no em dashes");
        }
    }
}
