//! The logos' pictures, which rows draw through placeholders: the terminal keeps them for every
//! screen from the first frame that can draw one, in a terminal that takes them, or from the
//! frame `ui.logos` turns on in, and gets them anew, drawn for the new cells, after a font size
//! change that calls for another picture size.

use std::io::Write;

use super::Ui;
use crate::kit::logo::{self, LogoSize};

/// What the terminal was given of the logos' pictures.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeptLogos {
    #[default]
    Nothing,
    /// Drawn at this size, the one the terminal's cells called for.
    DrawnAt(LogoSize),
}

impl<W: Write> Ui<W> {
    /// Before a frame draws: the logos' pictures, plain and numbered, at the size the cells the
    /// terminal reports now call for, where the terminal wasn't given that size yet. A font size
    /// change within one size sends nothing. While `ui.logos` is off no screen is sent them,
    /// and every screen gets them once it is on again.
    pub(super) fn keep_logos(&mut self) {
        let cell = self.picture_cell();
        let size = LogoSize::for_terminal(cell);
        let turned_on = logo::held_pictures(cell);
        let drawn = self.theme.logo_pictures && logo::takes_logos();
        let due = drawn && self.logos != KeptLogos::DrawnAt(size);
        if let Some(mut pictures) = turned_on.or_else(|| due.then(|| logo::send_pictures(cell))) {
            pictures.extend(logo::resend_numbered(cell));
            self.terminal.keep_logos(&pictures);
            self.logos = KeptLogos::DrawnAt(size);
        }
        self.terminal
            .rows_draw_kept_pictures(self.theme.logo_pictures);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use argonauts_core::logos::{Logo, LogoMark};
    use argonauts_core::names::AgentId;
    use ratatui::layout::{Position, Size};

    use super::Ui;
    use crate::bottom::{RunRowKind, WorkflowRow, WorkflowRowState};
    use crate::kit::logo;
    use crate::model::ViewModel;
    use crate::terminal::InlineTerminal;
    use crate::theme::Theme;

    /// The placement of Claude Code's logo numbered 2, on a dark screen.
    fn second_seat() -> String {
        let id = LogoMark::ClaudeCode.picture_id(Some(2), false);
        format!("a=p,U=1,i={id},")
    }

    /// A council with Claude Code's model in its second seat, drawn numbered, in a terminal
    /// whose cells are `cell` pixels in size.
    fn seated(cell: (u16, u16)) -> Ui<Vec<u8>> {
        let terminal = InlineTerminal::new(Vec::new(), Size::new(60, 16), Position::new(0, 2));
        let theme = logo::golden::pictured(&Theme::dark());
        let mut ui = Ui::new(
            terminal,
            theme,
            ViewModel::new("conversation"),
            Instant::now(),
        );
        ui.use_pictures(None, Some(cell));
        let mut seat = WorkflowRow::new("run-01#s-001", "Opus 5.5 · 2", WorkflowRowState::Running);
        seat.kind = RunRowKind::Work;
        seat.depth = 1;
        seat.logo = Some(Logo {
            agent: AgentId("claude-code".to_owned()),
            mark: Some(LogoMark::ClaudeCode),
        });
        seat.number = Some(2);
        ui.model.bottom.workflows.rows = vec![
            WorkflowRow::new("run-01", "Rate limit the API", WorkflowRowState::Running),
            seat,
        ];
        ui
    }

    /// The plain logos' pictures as they go out for cells `cell` pixels in size.
    fn drawn_for(cell: (u16, u16)) -> String {
        String::from_utf8_lossy(&logo::send_pictures(Some(cell))).into_owned()
    }

    fn written_since(ui: &Ui<Vec<u8>>, from: usize) -> String {
        String::from_utf8_lossy(&ui.terminal().writer()[from..]).into_owned()
    }

    #[test]
    fn the_logos_go_out_again_only_when_the_cells_call_for_another_size() {
        logo::set_takes_logos(true);
        let t0 = Instant::now();
        let mut ui = seated((10, 20));
        ui.frame(t0).unwrap();
        let first = written_since(&ui, 0);
        assert!(first.contains(&drawn_for((10, 20))));
        assert_eq!(first.matches(&second_seat()).count(), 1);

        // A larger font that keeps the pictures' size sends nothing again.
        let from = ui.terminal().writer().len();
        ui.use_pictures(None, Some((12, 24)));
        ui.frame(t0 + Duration::from_secs(1)).unwrap();
        assert!(!written_since(&ui, from).contains("a=p,U=1,"));

        // One that calls for the double size sends every logo again, the numbered one too, each
        // once and drawn for the new cells.
        let from = ui.terminal().writer().len();
        ui.use_pictures(None, Some((20, 40)));
        ui.frame(t0 + Duration::from_secs(2)).unwrap();
        let after = written_since(&ui, from);
        assert!(after.contains(&drawn_for((20, 40))));
        assert_eq!(after.matches(&second_seat()).count(), 1, "{after:?}");
        let larger = logo::resend_numbered(Some((20, 40)));
        assert!(after.contains(&*String::from_utf8_lossy(&larger)));
        assert_ne!(larger, logo::resend_numbered(Some((10, 20))));
        logo::set_takes_logos(false);
    }
}
