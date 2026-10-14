//! The merger: bringing in a branch that moved while the run worked, fixing what that
//! broke, and the message the change lands with.

pub const MAIN_MOVED: &str = argonauts_core::template!("prompts/merge/main-moved.md");
pub const COMMIT_MESSAGE: &str = argonauts_core::template!("prompts/merge/commit-message.md");
pub const AFTER_MAIN: &str = argonauts_core::template!("prompts/merge/after-main.md");

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
