//! A council talking on this screen: its keys, and its bubbles as pictures under the words of
//! the rows they print on.

use std::io::{self, Write};
use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Position;

use super::{Ui, UiAction};
use crate::base::draw::picture::Picture;
use crate::base::mascot::graphics::Graphics;
use crate::council::screens::pick::starts_picking;

impl<W: Write> Ui<W> {
    /// Shift+↑ while a council talks here picks its newest message or task, and while one is
    /// picked every key but a control chord is the picker's. Esc while you write a reply drops
    /// the reply and keeps what you wrote. False for any other key.
    pub(super) fn council_key(&mut self, key: KeyEvent, actions: &mut Vec<UiAction>) -> bool {
        let bottom = &self.model.bottom;
        if bottom.replying.is_some()
            && key.code == KeyCode::Esc
            && key.modifiers.is_empty()
            && bottom.composer_has_focus()
        {
            self.model.bottom.replying = None;
            self.dirty = true;
            return true;
        }
        let picking = bottom.council.picking.is_some();
        if picking && key.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        let starts = bottom.council.sits && starts_picking(&key) && bottom.composer_has_focus();
        if !picking && !starts {
            return false;
        }
        actions.push(UiAction::CouncilKey(key));
        self.dirty = true;
        true
    }

    /// The terminal draws kitty pictures under its words, so the council's bubbles are drawn
    /// as pictures there.
    pub fn draw_pictures_under_text(&mut self, yes: bool) {
        self.pictures_under_text = yes;
    }

    /// The pixels a cell while pictures go under the words.
    pub(super) fn under_text_cell(&self) -> Option<(u16, u16)> {
        let (graphics, cell) = self.picture_terminal()?;
        (self.pictures_under_text && graphics == Graphics::Kitty).then_some(cell)
    }

    /// `pictures` go under the rows the next history to print holds, each by its cell's rows
    /// among them: the inline screen puts each once as its rows print, and the full screen
    /// keeps them to put where their rows show.
    pub(super) fn print_council_pictures(&mut self, pictures: Vec<(Range<usize>, Picture)>) {
        if pictures.is_empty() {
            return;
        }
        let Some(cell) = self.under_text_cell() else {
            return;
        };
        let width = self.terminal.screen_size().width;
        if let Some(scrollback) = &self.scrollback {
            let at = scrollback.printed_len();
            for (rows, picture) in pictures {
                let rows = at + rows.start..at + rows.end;
                self.council_pictures.keep(rows, picture, width, cell);
            }
            return;
        }
        let at = self.terminal.pending_history().len();
        for (rows, picture) in pictures {
            let row = at + rows.start + usize::from(picture.row);
            let escapes = self.council_pictures.printed(&picture);
            self.terminal.send_on_history_row(row, picture.col, escapes);
        }
    }

    /// After the full screen drew a frame: each kept picture goes where its rows show now.
    pub(super) fn place_council_pictures(&mut self) -> io::Result<()> {
        let (Some(scrollback), Some((_, cell))) = (&self.scrollback, self.picture_terminal())
        else {
            return Ok(());
        };
        let width = self.terminal.screen_size().width;
        let picked = self.model.bottom.council.picked.as_ref();
        let placing = self
            .council_pictures
            .frame(width, cell, picked, |first, rows| {
                scrollback.printed_rows_at(first, rows)
            });
        for (at, escapes) in placing {
            let at = at.map(|(column, row)| Position::new(column, row));
            self.terminal.send_escapes(at, &escapes)?;
        }
        Ok(())
    }

    /// Before another program has the terminal or the app ends, the pictures leave it.
    pub(super) fn let_go_of_council_pictures(&mut self) -> io::Result<()> {
        let escapes = self.council_pictures.let_go();
        if escapes.is_empty() {
            return Ok(());
        }
        self.terminal.send_escapes(None, &escapes)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use ratatui::layout::Size;

    use super::*;
    use crate::app::model::ViewModel;
    use crate::base::draw::render::{Clock, RenderCtx};
    use crate::base::theme::{Glyphs, Theme};
    use crate::cells::cell::{Cell, HistoryCell};
    use crate::council::tests::story;
    use crate::council::{Look, Spacing, TalkCell};
    use crate::terminal::InlineTerminal;

    const COLUMNS: u16 = 100;
    const ROWS: u16 = 30;
    const CELL: (u16, u16) = (10, 20);

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn ui(fullscreen: bool) -> (Ui<Vec<u8>>, Instant) {
        let terminal =
            InlineTerminal::new(Vec::new(), Size::new(COLUMNS, ROWS), Position::new(0, 0));
        let at = Instant::now();
        let mut ui = Ui::new(terminal, theme(), ViewModel::new("conversation"), at);
        if fullscreen {
            ui.use_fullscreen();
        }
        ui.use_pictures(Some(Graphics::Kitty), Some(CELL));
        ui.draw_pictures_under_text(true);
        (ui, at)
    }

    fn bubble() -> TalkCell {
        let item = story::luna_on_buckets(false);
        TalkCell::new(
            item,
            Look::Pictures,
            story::faces(),
            &mut Spacing::default(),
        )
    }

    /// A cell of plain rows.
    #[derive(Debug)]
    struct Rows(usize);

    impl HistoryCell for Rows {
        fn lines(&self, _ctx: &RenderCtx<'_>) -> Vec<ratatui::text::Line<'static>> {
            (0..self.0)
                .map(|n| ratatui::text::Line::from(format!("Row {n}")))
                .collect()
        }

        fn continues_previous(&self) -> bool {
            true
        }
    }

    fn frame(ui: &mut Ui<Vec<u8>>, at: &mut Instant) -> String {
        *at += Duration::from_millis(100);
        ui.frame(*at).expect("a frame");
        String::from_utf8_lossy(&std::mem::take(ui.terminal_mut().writer_mut())).into_owned()
    }

    /// The cell the cursor went to right before `keys` first went out, 0-based.
    fn sent_at(sent: &str, keys: &str) -> Option<(u16, u16)> {
        let at = sent.find(keys)?;
        let start = sent[..at].rfind("\x1b[")?;
        let (row, column) = sent[start + 2..at].strip_suffix('H')?.split_once(';')?;
        Some((
            column.parse::<u16>().ok()? - 1,
            row.parse::<u16>().ok()? - 1,
        ))
    }

    fn picture(cell: &TalkCell) -> Picture {
        let theme = theme();
        let ctx = RenderCtx::new(COLUMNS, &theme, Clock::still(Instant::now()));
        cell.picture(&ctx, CELL).expect("a bubble draws a picture")
    }

    /// Inline, a bubble's picture goes under its words as its rows print, at the cell where
    /// it starts, below the text and below cells with a background of their own.
    #[test]
    fn inline_a_bubble_goes_under_its_rows_as_they_print() {
        let (mut ui, mut at) = ui(false);
        let cell = bubble();
        let picture = picture(&cell);
        ui.model
            .history
            .push(Cell::from(Box::new(cell) as Box<dyn HistoryCell>));
        let sent = frame(&mut ui, &mut at);
        assert_eq!(
            sent_at(&sent, "\x1b_Ga=t,"),
            Some((picture.col, picture.row)),
            "{sent:?}"
        );
        assert!(sent.contains(",z=-1073741825\x1b\\"), "{sent:?}");
        // Printed once: the next frames leave it to the terminal's scrolling.
        ui.model
            .history
            .push(Cell::from(Box::new(Rows(3)) as Box<dyn HistoryCell>));
        assert!(!frame(&mut ui, &mut at).contains("\x1b_G"));
    }

    /// Without a terminal that draws under its words, nothing goes out.
    #[test]
    fn no_picture_goes_out_where_the_terminal_draws_none_under_its_words() {
        let (mut ui, mut at) = ui(false);
        ui.draw_pictures_under_text(false);
        ui.model
            .history
            .push(Cell::from(Box::new(bubble()) as Box<dyn HistoryCell>));
        assert!(!frame(&mut ui, &mut at).contains("\x1b_G"));
    }

    /// On the full screen the picture goes where its rows show in each frame: whole, then cut
    /// at the top as they scroll up past the screen's edge, then taken off once they're gone.
    /// A screen of another width takes it out of the terminal.
    #[test]
    fn on_the_full_screen_a_bubble_follows_its_rows_and_is_cut_at_the_edge() {
        let (mut ui, mut at) = ui(true);
        let cell = bubble();
        let picture = picture(&cell);
        ui.model
            .history
            .push(Cell::from(Box::new(cell) as Box<dyn HistoryCell>));
        let sent = frame(&mut ui, &mut at);
        assert!(sent.contains("\x1b_Ga=t,f=100,q=2,i="), "{sent:?}");
        let whole = format!("x=0,y=0,w={},h={}", picture.size.0, picture.size.1);
        assert!(sent.contains(&whole), "{sent:?}");
        // The image data goes first, then it is put where the cursor went.
        let (column, _) = sent_at(&sent, "\x1b_Ga=t,").expect("placed");
        assert_eq!(column, picture.col);

        // Nothing moved, so nothing goes out again.
        assert!(!frame(&mut ui, &mut at).contains("\x1b_G"));

        // A row at a time pushes it up past the top, where it is cut a row at a time.
        let push_row = |ui: &mut Ui<Vec<u8>>| {
            let row: Box<dyn HistoryCell> = Box::new(Rows(1));
            ui.model.history.push(Cell::from(row));
        };
        let mut cut = None;
        for _ in 0..ROWS {
            push_row(&mut ui);
            let sent = frame(&mut ui, &mut at);
            if sent.contains(&format!("x=0,y={},w=", CELL.1)) {
                cut = Some(sent);
                break;
            }
        }
        let cut = cut.expect("cut at the top");
        assert!(!cut.contains("\x1b_Ga=t"), "sent once: {cut:?}");
        assert_eq!(sent_at(&cut, "\x1b_Ga=p,"), Some((picture.col, 0)));

        let mut gone = false;
        for _ in 0..ROWS {
            push_row(&mut ui);
            if frame(&mut ui, &mut at).contains("\x1b_Ga=d,d=i,q=2,i=") {
                gone = true;
                break;
            }
        }
        assert!(gone, "taken off once its rows are gone");

        ui.resize(Size::new(COLUMNS - 10, ROWS), None);
        let sent = frame(&mut ui, &mut at);
        assert!(sent.contains("\x1b_Ga=d,d=I,q=2,i="), "{sent:?}");
    }

    /// A font size change keeps the columns and rows but not the pixels a cell, so on the full
    /// screen the bubbles drawn for the old cells leave the terminal.
    #[test]
    fn on_the_full_screen_a_font_size_change_takes_the_bubbles_out() {
        let (mut ui, mut at) = ui(true);
        ui.model
            .history
            .push(Cell::from(Box::new(bubble()) as Box<dyn HistoryCell>));
        assert!(frame(&mut ui, &mut at).contains("\x1b_Ga=p,"));
        ui.measure_cells_with(|| Some((CELL.0 * 2, CELL.1 * 2)));
        ui.resize(Size::new(COLUMNS, ROWS), None);
        let sent = frame(&mut ui, &mut at);
        assert!(sent.contains("\x1b_Ga=d,d=I,q=2,i="), "{sent:?}");
        assert!(!frame(&mut ui, &mut at).contains("\x1b_Ga=p,"));
    }

    fn key(
        ui: &mut Ui<Vec<u8>>,
        code: KeyCode,
        modifiers: KeyModifiers,
        at: Instant,
    ) -> Vec<UiAction> {
        let key = crossterm::event::Event::Key(KeyEvent::new(code, modifiers));
        ui.handle_event(key, at)
    }

    /// Shift+↑ goes to the council while one talks here, and then every key does until the
    /// pick ends; with none, it is the input's.
    #[test]
    fn shift_up_goes_to_the_council_while_one_talks() {
        let (mut ui, at) = ui(false);
        let shift_up = |ui: &mut Ui<Vec<u8>>| key(ui, KeyCode::Up, KeyModifiers::SHIFT, at);
        assert!(
            !shift_up(&mut ui)
                .iter()
                .any(|action| matches!(action, UiAction::CouncilKey(_)))
        );
        ui.model.bottom.council.sits = true;
        assert!(matches!(
            shift_up(&mut ui).as_slice(),
            [UiAction::CouncilKey(_)]
        ));
        ui.model.bottom.council.picking = Some("↑ ↓  to pick another".to_owned());
        let r = key(&mut ui, KeyCode::Char('r'), KeyModifiers::NONE, at);
        assert!(matches!(r.as_slice(), [UiAction::CouncilKey(_)]));
        assert!(ui.model.bottom.composer.is_empty(), "r went to the picker");
    }

    /// Esc while you write a reply drops the reply's row and keeps the draft.
    #[test]
    fn esc_drops_the_reply_and_keeps_what_you_wrote() {
        let (mut ui, at) = ui(false);
        ui.model.bottom.replying = Some(crate::council::screens::reply::ReplyBar {
            message: argonauts_core::base::names::MessageId::numbered(1),
            face: None,
            quote: "Keep /health open.".to_owned(),
        });
        key(&mut ui, KeyCode::Char('n'), KeyModifiers::NONE, at);
        let esc = key(&mut ui, KeyCode::Esc, KeyModifiers::NONE, at);
        assert!(esc.is_empty(), "{esc:?}");
        assert!(ui.model.bottom.replying.is_none());
        assert_eq!(ui.model.bottom.composer.text(), "n");
    }

    /// The id of the picture `keys` first put.
    fn id_of(sent: &str, keys: &str) -> Option<String> {
        // After the escape that opens them.
        let at = sent.find(keys)? + "\x1b_G".len();
        let rest = &sent[at..];
        let keys = rest.split(['\x1b', ';']).next()?;
        let id = keys.split(',').find(|key| key.starts_with("i="))?;
        Some(id.to_owned())
    }

    /// On the full screen, picking a printed message swaps its bubble for the ringed one where
    /// it shows, and putting it down swaps it back, its image sent once.
    #[test]
    fn a_picked_message_is_ringed_where_it_printed() {
        let (mut ui, mut at) = ui(true);
        let id = argonauts_core::base::names::MessageId::numbered(4);
        let cell = bubble().for_message(id.clone());
        ui.model
            .history
            .push(Cell::from(Box::new(cell) as Box<dyn HistoryCell>));
        let sent = frame(&mut ui, &mut at);
        let plain = id_of(&sent, "\x1b_Ga=p,").expect("put");

        ui.model.bottom.council.picked = Some(id);
        ui.dirty = true;
        let sent = frame(&mut ui, &mut at);
        let ringed = id_of(&sent, "\x1b_Ga=p,").expect("ringed");
        assert_ne!(ringed, plain);
        assert!(
            sent.contains(&format!("\x1b_Ga=t,f=100,q=2,{ringed}")),
            "{sent:?}"
        );
        assert!(
            sent.contains(&format!("\x1b_Ga=d,d=i,q=2,{plain},p=1")),
            "{sent:?}"
        );

        ui.model.bottom.council.picked = None;
        ui.dirty = true;
        let sent = frame(&mut ui, &mut at);
        assert_eq!(id_of(&sent, "\x1b_Ga=p,").as_deref(), Some(plain.as_str()));
        assert!(!sent.contains("\x1b_Ga=t"), "sent once: {sent:?}");
        assert!(
            sent.contains(&format!("\x1b_Ga=d,d=i,q=2,{ringed},p=1")),
            "{sent:?}"
        );
    }
}
