//! Handoffs: the summary of a stopped session that a replacement reads in its note.

pub const SUMMARY: &str = include_str!("handoff/summary.md");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_written() {
        assert!(!SUMMARY.contains("Not written yet"));
        assert!(!SUMMARY.contains('\u{2014}'), "no em dashes");
    }
}
