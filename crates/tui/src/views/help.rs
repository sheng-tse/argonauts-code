//! `/help`: every key, from the keymap in effect, and every command under the categories the
//! `/` menu uses, the main agent CLI's after Argonauts' own.
use argonauts_core::vendor_commands::VendorCommandSet;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::commands::help_sections;
use crate::keymap::Keymap;
use crate::text::{str_width, wrap_text};
use crate::theme::{Role, Theme};

use super::layout::left;
use super::list::{self, Nav};
use super::pager::position_mark;
use super::panes::{self, Footer, Header, Marks, Side};
use super::{View, ViewCtx, ViewOutcome};

/// Keys and command names past this are cut, so the descriptions keep their room.
const MAX_NAME: usize = 28;

#[derive(Debug)]
pub struct HelpView {
    pub keymap: Keymap,
    /// The main agent CLI's own commands.
    pub vendor: Option<VendorCommandSet>,
    pub offset: usize,
    page: usize,
    rows: usize,
}

impl HelpView {
    pub fn new(keymap: Keymap) -> Self {
        Self {
            keymap,
            vendor: None,
            offset: 0,
            page: 1,
            rows: 0,
        }
    }

    pub fn with_commands(mut self, vendor: Option<VendorCommandSet>) -> Self {
        self.vendor = vendor;
        self
    }

    /// `(name, description)` pairs under their headings: the keys, then each category.
    fn sections(&self) -> Vec<(String, Vec<(String, String)>)> {
        let keys: Vec<(String, String)> = self
            .keymap
            .rows()
            .into_iter()
            .map(|(keys, action)| (keys, action.describe().to_owned()))
            .collect();
        let mut sections = vec![("Keys".to_owned(), keys)];
        sections.extend(
            help_sections(self.vendor.as_ref())
                .into_iter()
                .map(|section| (section.heading, section.rows)),
        );
        sections
    }

    fn command_count(&self) -> usize {
        self.sections()[1..]
            .iter()
            .map(|(_, rows)| rows.len())
            .sum()
    }

    fn lines(&self, width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let sections = self.sections();
        let name_width = sections
            .iter()
            .flat_map(|(_, rows)| rows.iter().map(|(name, _)| str_width(name)))
            .max()
            .unwrap_or(0)
            .min(MAX_NAME)
            .min(width / 2);
        let dim = theme.style(Role::Dim);
        let mut lines = Vec::new();
        for (heading, rows) in sections {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled(heading, theme.style(Role::Label))));
            let rest = vec![Span::raw(" ".repeat(name_width + 2))];
            for (name, description) in rows {
                // A name too long for its column takes a row of its own.
                let first = if str_width(&name) > name_width {
                    lines.extend(wrap_text(&name, theme.style(Role::Text), width, &[], &[]));
                    rest.clone()
                } else {
                    vec![
                        Span::styled(left(&name, name_width), theme.style(Role::Text)),
                        Span::raw("  "),
                    ]
                };
                lines.extend(wrap_text(&description, dim, width, &first, &rest));
            }
        }
        lines
    }

    fn scroll(&mut self, nav: Nav) {
        self.offset = list::scroll(self.offset, nav, self.rows, self.page);
    }
}

impl View for HelpView {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let header = Header {
            title: Line::from(Span::styled("Help", theme.style(Role::Label))),
            description: "The keys in effect, and every command by category".to_owned(),
            summary: panes::count(self.command_count(), "command"),
        };
        let panes = panes::split(area, 0);
        panes::draw_header(buf, panes.header, &header, theme);
        panes::draw_box(buf, &panes, ["", ""], Side::Right, theme);

        let lines = self.lines(usize::from(panes.right.width), theme);
        let height = usize::from(panes.right.height);
        self.page = height.max(1);
        self.rows = lines.len();
        self.offset = self.offset.min(lines.len().saturating_sub(height));
        panes::put_lines(buf, panes.right, &lines[self.offset..]);

        let marks = Marks::for_theme(theme);
        let mark = position_mark(self.offset, height, lines.len(), marks.down);
        let hints = Footer::hints(&[("↑↓ scroll", 0), (panes::ESC_BACK, 0)]);
        panes::draw_footer(buf, panes.footer, hints, &mark, theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            return ViewOutcome::Close;
        }
        match list::nav_key(&key, self.page) {
            Some(nav) => {
                self.scroll(nav);
                ViewOutcome::Consumed
            }
            None => ViewOutcome::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_follow_the_keys_under_their_categories_in_menu_order() {
        let help = HelpView::new(Keymap::default());
        let sections = help.sections();
        let headings: Vec<&str> = sections
            .iter()
            .map(|(heading, _)| heading.as_str())
            .collect();
        assert_eq!(
            headings,
            [
                "Keys",
                "Session",
                "Work",
                "Runs",
                "Crew",
                "Approvals and limits",
                "Learning",
                "Display",
                "Setup and help",
            ]
        );
        let commands: usize = help.sections()[1..]
            .iter()
            .map(|(_, rows)| rows.len())
            .sum();
        assert_eq!(
            commands,
            argonauts_core::input::SLASH_COMMANDS.len()
                + argonauts_core::input::SHARED_COMMANDS.len()
        );
        assert_eq!(help.command_count(), commands);
        let work = &help.sections()[2].1;
        assert!(
            work.iter()
                .any(|(name, _)| name == "/follow-up [run] <request>")
        );
    }

    #[test]
    fn the_main_agents_commands_follow_argonauts_own() {
        let set = crate::commands::tests::claude_commands();
        let help = HelpView::new(Keymap::default()).with_commands(Some(set));
        let sections = help.sections();
        let headings: Vec<&str> = sections.iter().map(|(h, _)| h.as_str()).collect();
        assert_eq!(
            &headings[headings.len() - 2..],
            ["Claude Code", "Open Claude Code for these"]
        );
        let plain = HelpView::new(Keymap::default()).command_count();
        assert_eq!(help.command_count(), plain + 5);
    }
}
