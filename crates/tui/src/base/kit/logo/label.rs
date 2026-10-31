//! A model as a row names it: its logo and its name as one unit, so the two always agree. The
//! name drops its maker's word (`6 Luna` after Codex's logo) only where the logo's picture
//! really draws; beside a colour dot, or with nothing in the logo's place, it reads whole
//! (`GPT 6 Luna`), since nothing else says who made the model. A CLI's name is never
//! shortened: `Claude Code` stays whole after its logo.
//!
//! A row that names a worker takes the logo of the model it runs as `DisplayNames::running`
//! gives it, as its name does, so a session that names no model shows its settings' model's logo
//! from its start.

use argonauts_core::base::names::ModelRef;
use argonauts_core::look::display_names::{self, DisplayNames};
use argonauts_core::look::logos::{Logo, logo_for};
use ratatui::style::Style;
use ratatui::text::Span;

use super::{as_drawn, logo_before, logo_or, logo_spans, picture_of};
use crate::base::text::{spans_width, str_width};
use crate::base::theme::Theme;

/// What stands in a logo's place where its picture doesn't draw.
#[derive(Debug, Clone, PartialEq)]
pub enum NoPicture {
    /// The serving agent's colour dot, so the names still line up with the rows that draw a
    /// picture.
    Dot,
    /// Nothing: the name starts where the logo would.
    Nothing,
    /// A mark of the row's own, such as `● ` for the CLI in use, which carries its blank.
    Mark(Span<'static>),
}

/// A logo and the name after it.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelLabel {
    /// The logo's cells and the blank after them, or what stands in their place.
    pub logo: Vec<Span<'static>>,
    /// The name as it reads after `logo`.
    pub name: String,
    /// Whether the logo's picture draws, which is what lets the name drop its maker's word.
    pub pictured: bool,
    /// Whose logo it is.
    pub whose: Logo,
}

impl ModelLabel {
    /// `worker`'s label: the logo of whoever serves the model it runs, then that model's name,
    /// or its CLI's where nothing says which model it runs.
    pub fn of(names: &DisplayNames, worker: &ModelRef, theme: &Theme, without: NoPicture) -> Self {
        let worker = names.running(worker);
        let logo = logo_for(&worker.agent, worker.model.as_deref());
        let name = names.after_logo(&as_drawn(&logo, theme), &worker);
        Self::beside(logo, name, theme, without)
    }

    /// A CLI's label: its logo, then its whole name.
    pub fn cli(names: &DisplayNames, cli: &str, theme: &Theme, without: NoPicture) -> Self {
        Self::cli_named(cli, &names.cli(cli), theme, without)
    }

    /// [`ModelLabel::cli`] for a row that already holds the CLI's name, as a status reading
    /// does.
    pub fn cli_named(cli: &str, name: &str, theme: &Theme, without: NoPicture) -> Self {
        Self::whole(&logo_for(cli, None), name, theme, without)
    }

    /// `name` beside `logo`, kept whole: a CLI's or a provider's name, or one its row has
    /// already worded for the logo before it.
    pub fn whole(logo: &Logo, name: &str, theme: &Theme, without: NoPicture) -> Self {
        Self::beside(logo.clone(), name.to_owned(), theme, without)
    }

    /// The label of a model a row already holds by its whole name, beside `logo`.
    pub fn named(logo: &Logo, name: &str, theme: &Theme, without: NoPicture) -> Self {
        let name = display_names::after_logo(&as_drawn(logo, theme), name);
        Self::beside(logo.clone(), name, theme, without)
    }

    fn beside(logo: Logo, name: String, theme: &Theme, without: NoPicture) -> Self {
        let pictured = picture_of(&logo, theme).is_some();
        let cells = match without {
            NoPicture::Dot => {
                let mut cells = logo_spans(&logo, theme);
                cells.push(Span::raw(" "));
                cells
            }
            NoPicture::Nothing => logo_before(&logo, theme),
            NoPicture::Mark(mark) => logo_or(&logo, theme, mark),
        };
        Self {
            logo: cells,
            name,
            pictured,
            whose: logo,
        }
    }

    /// The name where the logo's picture draws, which says who serves the model, so a GPT
    /// model reads `5.6 Luna` after Codex's. `None` where no picture draws, for a row that
    /// names the model its own way without one, such as `GPT 5.6 Luna on Codex`.
    pub fn beside_picture(self) -> Option<String> {
        self.pictured.then_some(self.name)
    }

    /// Cells the label takes: the logo's, then the name's.
    pub fn width(&self) -> usize {
        spans_width(&self.logo) + str_width(&self.name)
    }

    /// The label drawn, its name in `style`.
    pub fn spans(&self, style: Style) -> Vec<Span<'static>> {
        let mut spans = self.logo.clone();
        spans.push(Span::styled(self.name.clone(), style));
        spans
    }
}
