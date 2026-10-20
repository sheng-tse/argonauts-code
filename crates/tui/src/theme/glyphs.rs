use argonauts_core::settings::ui::Glyphs as GlyphChoice;

/// Every symbol the UI draws, so a terminal without the Unicode set can swap them. None has an
/// emoji form, since a font without that glyph falls back to the emoji font, whose glyph spills
/// into the next cell. The few East Asian ambiguous ones are Claude Code's shapes and the box
/// lines, which have no one-cell form, and `ui.ambiguous_width = 2` swaps them out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glyphs {
    /// Starts assistant text, tool calls, and notices: Claude Code's `●`, and on macOS its `⏺`.
    pub bullet: &'static str,
    /// Hangs results under a tool call: `  ⎿  `.
    pub result: &'static str,
    pub prompt: &'static str,
    pub turn_end: &'static str,
    pub recap: &'static str,
    pub question: &'static str,
    /// Starts a note typed under a question option.
    pub note: &'static str,
    /// Precedes a mode that stops and asks: `ǁ supervised mode on`. Claude Code's `⏸` has an
    /// emoji form, and this Latin letter draws the same pause sign from the text font itself.
    pub mode_ask: &'static str,
    /// Precedes a mode that goes ahead without you: `⏵⏵ auto mode on`.
    pub mode_go: &'static str,
    pub rule: &'static str,
    pub dashed: &'static str,
    pub ellipsis: &'static str,
    /// Precedes token counts: `↓ 3.6k tokens`.
    pub tokens: &'static str,
    /// Points at the agent panel: `← 3 agents`.
    pub agents: &'static str,
    /// The working glyph's frames, forward and back: Claude Code's, with `·`, `✳`, and `✽`
    /// swapped for `∙`, `✲`, and `✼`, which draw the same in one cell everywhere.
    pub spinner: &'static [&'static str],
    /// Terminal title frames while working, two half circles that alternate about once a second.
    /// The title bar doesn't draw in cells, so these keep Claude Code's glyphs.
    pub title_busy: &'static [&'static str],
    pub title_idle: &'static str,
    /// One frame holds the circle still; more frames turn it like the title glyph.
    pub agent_working: &'static [&'static str],
    /// Waiting on other work, such as a task it depends on.
    pub agent_waiting: &'static str,
    /// Waiting on a timer, such as a usage reset.
    pub agent_timer: &'static str,
    pub agent_needs_you: &'static str,
    pub agent_done: &'static str,
    pub agent_failed: &'static str,
    /// Progress bar cells for terminals where the bar can't be drawn with background colors.
    pub bar_fill: &'static str,
    pub bar_track: &'static str,
    pub todo_pending: &'static str,
    pub todo_active: &'static str,
    pub todo_done: &'static str,
    pub quote: &'static str,
    /// Tree rows under an item, such as each agent under `3 agents started`.
    pub tree_mid: &'static str,
    pub tree_end: &'static str,
    pub hunk_gap: &'static str,
    pub ready: &'static str,
    pub not_ready: &'static str,
    pub checkbox_off: &'static str,
    pub checkbox_on: &'static str,
}

impl Glyphs {
    pub const UNICODE: Glyphs = Glyphs {
        bullet: "●",
        result: "⎿",
        prompt: "❯",
        turn_end: "✻",
        recap: "※",
        question: "◆",
        note: "✎",
        mode_ask: "ǁ",
        mode_go: "⏵⏵",
        rule: "─",
        dashed: "╌",
        ellipsis: "…",
        tokens: "↓",
        agents: "←",
        spinner: &["∙", "✢", "✲", "✶", "✻", "✼", "✻", "✶", "✲", "✢"],
        title_busy: &["◐", "◑"],
        title_idle: "✳",
        agent_working: &["◐"],
        agent_waiting: "○",
        agent_timer: "✢",
        agent_needs_you: "◆",
        agent_done: "●",
        agent_failed: "●",
        bar_fill: "█",
        bar_track: "░",
        todo_pending: "☐",
        todo_active: "◧",
        todo_done: "☒",
        quote: "▎",
        tree_mid: "├",
        tree_end: "└",
        hunk_gap: "⋮",
        ready: "✓",
        not_ready: "✗",
        checkbox_off: "☐",
        checkbox_on: "☒",
    };

    pub const ASCII: Glyphs = Glyphs {
        bullet: "*",
        result: "L",
        prompt: ">",
        turn_end: "*",
        recap: "#",
        question: "?",
        note: ">",
        mode_ask: "||",
        mode_go: ">>",
        rule: "-",
        dashed: "-",
        ellipsis: "...",
        tokens: "v",
        agents: "<",
        spinner: &[".", "o", "O", "o"],
        title_busy: &["o", "O"],
        title_idle: "*",
        agent_working: &[">"],
        agent_waiting: "o",
        agent_timer: "+",
        agent_needs_you: "?",
        agent_done: "*",
        agent_failed: "x",
        bar_fill: "#",
        bar_track: ".",
        todo_pending: "[ ]",
        todo_active: "[>]",
        todo_done: "[x]",
        quote: "|",
        tree_mid: "|",
        tree_end: "`",
        hunk_gap: ":",
        ready: "ok",
        not_ready: "no",
        checkbox_off: "[ ]",
        checkbox_on: "[x]",
    };

    /// The Unicode set as Claude Code draws it on macOS, where items start with `⏺`. Its fonts
    /// have it in text form, one cell wide, as Ghostty draws it from Iosevka.
    pub const MACOS: Glyphs = Glyphs {
        bullet: "⏺",
        ..Self::UNICODE
    };

    /// The Unicode set for this platform: [`Glyphs::MACOS`] on macOS, as Claude Code chooses.
    pub fn unicode() -> &'static Glyphs {
        if cfg!(target_os = "macos") {
            &Self::MACOS
        } else {
            &Self::UNICODE
        }
    }

    /// The set for `ui.glyphs` and `ui.ambiguous_width`. Where ambiguous glyphs take two cells,
    /// most of the Unicode set (`●`, `◆`, `…`, `○`) would push frames and columns out of line,
    /// and the views pick their row marks by the set in use, so only the ASCII set keeps every
    /// glyph one cell.
    pub fn for_settings(choice: GlyphChoice, ambiguous_width: u8) -> Self {
        match choice {
            GlyphChoice::Unicode if ambiguous_width != 2 => Self::unicode().clone(),
            GlyphChoice::Unicode | GlyphChoice::Ascii => Self::ASCII,
        }
    }

    /// The ASCII set, which draws frames, bars, and marks from plain characters too.
    pub fn is_ascii(&self) -> bool {
        self.rule.is_ascii()
    }

    /// Every glyph with its field name, spinner and title frames one by one, in declaration
    /// order.
    pub fn named(&self) -> Vec<(&'static str, &'static str)> {
        let mut all = vec![
            ("bullet", self.bullet),
            ("result", self.result),
            ("prompt", self.prompt),
            ("turn_end", self.turn_end),
            ("recap", self.recap),
            ("question", self.question),
            ("note", self.note),
            ("mode_ask", self.mode_ask),
            ("mode_go", self.mode_go),
            ("rule", self.rule),
            ("dashed", self.dashed),
            ("ellipsis", self.ellipsis),
            ("tokens", self.tokens),
            ("agents", self.agents),
        ];
        all.extend(self.spinner.iter().map(|frame| ("spinner", *frame)));
        all.extend(self.title_busy.iter().map(|frame| ("title_busy", *frame)));
        all.push(("title_idle", self.title_idle));
        all.extend(
            self.agent_working
                .iter()
                .map(|frame| ("agent_working", *frame)),
        );
        all.extend([
            ("agent_waiting", self.agent_waiting),
            ("agent_timer", self.agent_timer),
            ("agent_needs_you", self.agent_needs_you),
            ("agent_done", self.agent_done),
            ("agent_failed", self.agent_failed),
            ("bar_fill", self.bar_fill),
            ("bar_track", self.bar_track),
            ("todo_pending", self.todo_pending),
            ("todo_active", self.todo_active),
            ("todo_done", self.todo_done),
            ("quote", self.quote),
            ("tree_mid", self.tree_mid),
            ("tree_end", self.tree_end),
            ("hunk_gap", self.hunk_gap),
        ]);
        all.extend([
            ("ready", self.ready),
            ("not_ready", self.not_ready),
            ("checkbox_off", self.checkbox_off),
            ("checkbox_on", self.checkbox_on),
        ]);
        all
    }
}

impl Default for Glyphs {
    fn default() -> Self {
        Self::UNICODE
    }
}

#[cfg(test)]
mod tests {
    use std::ops::RangeInclusive;

    use unicode_width::UnicodeWidthChar;

    use super::*;
    use crate::text::{is_ambiguous, str_width, width_with};

    /// Characters with the Emoji property below U+10000, from Unicode's emoji-data.txt. Even
    /// without the emoji variation selector, a font that lacks one of them sends it to the emoji
    /// font, as `⏸` was in the terminal that drew it two cells wide.
    #[rustfmt::skip]
    const EMOJI: &[RangeInclusive<u32>] = &[
        0x00A9..=0x00A9, 0x00AE..=0x00AE, 0x203C..=0x203C, 0x2049..=0x2049, 0x2122..=0x2122,
        0x2139..=0x2139, 0x2194..=0x2199, 0x21A9..=0x21AA, 0x231A..=0x231B, 0x2328..=0x2328,
        0x23CF..=0x23CF, 0x23E9..=0x23F3, 0x23F8..=0x23FA, 0x24C2..=0x24C2, 0x25AA..=0x25AB,
        0x25B6..=0x25B6, 0x25C0..=0x25C0, 0x25FB..=0x25FE, 0x2600..=0x2604, 0x260E..=0x260E,
        0x2611..=0x2611, 0x2614..=0x2615, 0x2618..=0x2618, 0x261D..=0x261D, 0x2620..=0x2620,
        0x2622..=0x2623, 0x2626..=0x2626, 0x262A..=0x262A, 0x262E..=0x262F, 0x2638..=0x263A,
        0x2640..=0x2640, 0x2642..=0x2642, 0x2648..=0x2653, 0x265F..=0x2660, 0x2663..=0x2663,
        0x2665..=0x2666, 0x2668..=0x2668, 0x267B..=0x267B, 0x267E..=0x267F, 0x2692..=0x2697,
        0x2699..=0x2699, 0x269B..=0x269C, 0x26A0..=0x26A1, 0x26A7..=0x26A7, 0x26AA..=0x26AB,
        0x26B0..=0x26B1, 0x26BD..=0x26BE, 0x26C4..=0x26C5, 0x26C8..=0x26C8, 0x26CE..=0x26CF,
        0x26D1..=0x26D1, 0x26D3..=0x26D4, 0x26E9..=0x26EA, 0x26F0..=0x26F5, 0x26F7..=0x26FA,
        0x26FD..=0x26FD, 0x2702..=0x2702, 0x2705..=0x2705, 0x2708..=0x270D, 0x270F..=0x270F,
        0x2712..=0x2712, 0x2714..=0x2714, 0x2716..=0x2716, 0x271D..=0x271D, 0x2721..=0x2721,
        0x2728..=0x2728, 0x2733..=0x2734, 0x2744..=0x2744, 0x2747..=0x2747, 0x274C..=0x274C,
        0x274E..=0x274E, 0x2753..=0x2755, 0x2757..=0x2757, 0x2763..=0x2764, 0x2795..=0x2797,
        0x27A1..=0x27A1, 0x27B0..=0x27B0, 0x27BF..=0x27BF, 0x2934..=0x2935, 0x2B05..=0x2B07,
        0x2B1B..=0x2B1C, 0x2B50..=0x2B50, 0x2B55..=0x2B55, 0x3030..=0x3030, 0x303D..=0x303D,
        0x3297..=0x3297, 0x3299..=0x3299,
    ];

    /// The ambiguous glyphs kept: Claude Code's own shapes and the box and block lines, which
    /// nothing draws the same in one cell. Any other ambiguous glyph is swapped for one that
    /// isn't, as `·` and `✽` were in the spinner.
    const KEPT_AMBIGUOUS: &str = "●※◆─…↓←◐○█▎├└▀";

    /// The window title isn't drawn in cells.
    const TITLE: [&str; 2] = ["title_busy", "title_idle"];

    fn has_emoji_form(c: char) -> bool {
        // ASCII digits, `#`, and `*` have one too, but only inside a keycap sequence.
        !c.is_ascii() && EMOJI.iter().any(|range| range.contains(&u32::from(c)))
    }

    #[test]
    fn every_glyph_is_one_cell_in_every_terminal() {
        for (name, glyph) in Glyphs::UNICODE.named() {
            if TITLE.contains(&name) {
                continue;
            }
            for c in glyph.chars() {
                assert!(!has_emoji_form(c), "{name}: {c:?} has an emoji form");
                // Past the first plane, few monospace fonts have the glyph at all.
                assert!(u32::from(c) < 0x10000, "{name}: {c:?}");
                // Wide, zero-width, and combining characters, and variation selectors.
                assert_eq!(c.width(), Some(1), "{name}: {c:?}");
                assert!(
                    c.width_cjk() == Some(1) || KEPT_AMBIGUOUS.contains(c),
                    "{name}: {c:?} is two cells where ambiguous glyphs are wide"
                );
            }
        }
    }

    #[test]
    fn the_rule_catches_what_broke_and_keeps_what_works() {
        for c in ['⏸', '⏺', '✳', '◻', '◼', '✔'] {
            assert!(has_emoji_form(c), "{c:?}");
        }
        for c in ['⏵', 'ǁ', '✲', '☐', '◧', '☒', '●', '#', '*'] {
            assert!(!has_emoji_form(c), "{c:?}");
        }
        assert_eq!('⏵'.width_cjk(), Some(1));
        assert_eq!('·'.width_cjk(), Some(2));
    }

    /// Layouts count columns by characters, so every glyph must be one column per character.
    #[test]
    fn unicode_glyphs_are_one_column_per_char() {
        let g = Glyphs::UNICODE;
        for (name, glyph) in g.named() {
            assert_eq!(str_width(glyph), glyph.chars().count(), "{name}: {glyph:?}");
        }
        assert_eq!(str_width(g.mode_ask), 1);
        assert_eq!(str_width(g.mode_go), 2);
    }

    #[test]
    fn ascii_glyphs_are_one_cell() {
        for (name, glyph) in Glyphs::ASCII.named() {
            assert!(glyph.is_ascii(), "{name}: {glyph:?}");
            // Every character one cell, whatever the terminal does with ambiguous glyphs.
            for cells in [1, 2] {
                assert_eq!(width_with(glyph, cells), glyph.chars().count(), "{name}");
            }
        }
    }

    #[test]
    fn named_lists_every_glyph() {
        // The fields that hold one glyph each.
        let singles = 33;
        for set in [Glyphs::UNICODE, Glyphs::ASCII] {
            let frames = set.spinner.len() + set.title_busy.len() + set.agent_working.len();
            assert_eq!(set.named().len(), singles + frames);
        }
    }

    /// Claude Code's macOS set is the other but for its bullet, which is one cell in its text
    /// form.
    #[test]
    fn the_macos_set_changes_only_the_bullet() {
        let mut both = Glyphs::MACOS.clone();
        assert_eq!(
            both.bullet.chars().map(|c| c.width()).collect::<Vec<_>>(),
            [Some(1)]
        );
        both.bullet = Glyphs::UNICODE.bullet;
        assert_eq!(both, Glyphs::UNICODE);
    }

    #[test]
    fn two_cell_ambiguous_glyphs_get_the_ascii_set() {
        let unicode = Glyphs::for_settings(GlyphChoice::Unicode, 1);
        assert_eq!(&unicode, Glyphs::unicode());
        assert!(unicode.named().iter().any(|(_, glyph)| is_ambiguous(glyph)));
        for choice in [GlyphChoice::Unicode, GlyphChoice::Ascii] {
            let narrow = Glyphs::for_settings(choice, 2);
            assert_eq!(narrow, Glyphs::ASCII);
            assert!(!narrow.named().iter().any(|(_, glyph)| is_ambiguous(glyph)));
        }
        assert_eq!(Glyphs::for_settings(GlyphChoice::Ascii, 1), Glyphs::ASCII);
    }
}
