//! Pictures every screen must hold before its rows can show them, such as the logos, which rows
//! draw through the kitty protocol's placeholders on whichever screen is up. kitty and Ghostty
//! keep one store of pictures for each screen and empty the alternate one each time it is
//! entered, so these go out on a screen right before the first cells drawn there, and again after
//! every entry to the alternate screen.

/// The escapes that send the kept pictures, and how much of them each screen holds.
#[derive(Debug, Default)]
pub(super) struct Kept {
    escapes: Vec<u8>,
    /// Bytes of `escapes` the main screen holds, from the first: pictures kept later go out
    /// after the ones a screen has.
    main: usize,
    alternate: usize,
    on_alternate: bool,
    /// No row draws them now, as with `ui.logos` off, so no screen is sent them until one does.
    unused: bool,
}

impl Kept {
    pub fn add(&mut self, escapes: &[u8]) {
        self.escapes.extend_from_slice(escapes);
    }

    /// The logos' pictures, plain and numbered, drawn anew in place of all kept before, as after
    /// a font size change. Sending a picture again under its id replaces it, so both screens get
    /// these again.
    pub fn renew_logos(&mut self, escapes: &[u8]) {
        self.escapes = escapes.to_vec();
        self.let_go();
    }

    /// Whether rows draw the pictures now. A screen that missed some while none did gets them
    /// once rows draw them again.
    pub fn used(&mut self, used: bool) {
        self.unused = !used;
    }

    /// What the screen shown lacks, which it holds once these are written.
    pub fn due(&mut self) -> &[u8] {
        if self.unused {
            return &[];
        }
        let held = if self.on_alternate {
            &mut self.alternate
        } else {
            &mut self.main
        };
        let from = std::mem::replace(held, self.escapes.len());
        &self.escapes[from..]
    }

    /// `CSI ? 1049 h` went out, which empties the alternate screen's store on the way in.
    pub fn entered_alternate(&mut self) {
        self.on_alternate = true;
        self.alternate = 0;
    }

    /// Back on the main screen, whose store is as the app left it.
    pub fn left_alternate(&mut self) {
        self.on_alternate = false;
    }

    /// Neither screen may hold them any more, as after a program you ran drew on the terminal or
    /// a write that may not have arrived whole.
    pub fn let_go(&mut self) {
        self.main = 0;
        self.alternate = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_screen_gets_the_pictures_once_and_the_alternate_again_on_every_entry() {
        let mut kept = Kept::default();
        assert!(kept.due().is_empty());
        kept.add(b"logos");
        assert_eq!(kept.due(), b"logos");
        assert!(kept.due().is_empty());

        kept.entered_alternate();
        assert_eq!(kept.due(), b"logos");
        kept.add(b"+one");
        assert_eq!(kept.due(), b"+one");
        kept.left_alternate();
        // The main screen held the first ones all along.
        assert_eq!(kept.due(), b"+one");

        kept.entered_alternate();
        assert_eq!(kept.due(), b"logos+one");
        kept.left_alternate();
        assert!(kept.due().is_empty());
    }

    #[test]
    fn letting_go_sends_them_again_on_both_screens() {
        let mut kept = Kept::default();
        kept.add(b"logos");
        let _ = kept.due();
        kept.entered_alternate();
        let _ = kept.due();
        kept.let_go();
        assert_eq!(kept.due(), b"logos");
        kept.left_alternate();
        assert_eq!(kept.due(), b"logos");
    }

    #[test]
    fn renewed_pictures_replace_all_kept_before_and_go_to_both_screens_again() {
        let mut kept = Kept::default();
        kept.renew_logos(b"small");
        kept.add(b"+one");
        assert_eq!(kept.due(), b"small+one");

        kept.renew_logos(b"large+ONE");
        assert_eq!(kept.due(), b"large+ONE");
        kept.entered_alternate();
        assert_eq!(kept.due(), b"large+ONE");
        assert!(kept.due().is_empty());
    }

    #[test]
    fn pictures_no_row_draws_go_to_no_screen_and_follow_once_rows_draw_them() {
        let mut kept = Kept::default();
        kept.add(b"logos");
        let _ = kept.due();
        kept.used(false);
        kept.entered_alternate();
        assert!(kept.due().is_empty());
        kept.left_alternate();
        kept.entered_alternate();
        assert!(kept.due().is_empty());

        kept.used(true);
        assert_eq!(kept.due(), b"logos");
        kept.left_alternate();
        // The main screen kept its own while they went unused.
        assert!(kept.due().is_empty());
    }
}
