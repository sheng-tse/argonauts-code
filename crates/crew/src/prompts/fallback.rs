//! Fallbacks: the summary of a stopped session that a fallback reads in its note, and the
//! judge's pick among fallbacks that did the same task at once.

pub const SUMMARY: &str = argonauts_core::template!("prompts/fallback/summary.md");
pub const PICK: &str = argonauts_core::template!("prompts/fallback/pick.md");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_written() {
        for template in [SUMMARY, PICK] {
            assert!(!template.contains("Not written yet"));
            assert!(!template.contains('\u{2014}'), "no em dashes");
        }
    }
}
