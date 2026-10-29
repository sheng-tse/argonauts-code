mod cut;
pub mod marks;
mod parts;
mod sanitize;
mod width;
mod wrap;

use ratatui::text::Line;

pub use cut::{
    At, cut, cut_spans, fit_lines, truncate_line, truncate_spans, truncate_spans_at_word,
};
pub use parts::{KEEP, Part, fit_parts, full_width, join_parts, parts_that_fit, width_of};
pub use sanitize::{sanitize, sanitize_line};
pub use width::{
    ambiguous_width, is_ambiguous, set_ambiguous_width, spans_width, str_width, width_with,
};
pub use wrap::{hard_wrap_spans, wrap_code_spans, wrap_spans, wrap_text};

/// One row per line, with trailing spaces trimmed.
pub fn plain_text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(|line| {
            let row: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            row.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
