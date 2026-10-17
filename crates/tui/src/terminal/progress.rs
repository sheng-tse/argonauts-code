//! OSC 9;4, the progress sequence that Ghostty, iTerm2, ConEmu, and Windows Terminal draw in a
//! tab or title bar, as Claude Code sends it while it works. A terminal that doesn't know it
//! ignores it.

use std::fmt;

use crossterm::Command;

/// Work goes on with no end in sight, or the bar comes off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetProgress {
    pub working: bool,
}

impl Command for SetProgress {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        // 3 is the bar with no amount, 0 takes it off.
        let state = if self.working { 3 } else { 0 };
        write!(f, "\x1b]9;4;{state}\x07")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Err(std::io::Error::other("progress needs ANSI support"))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use crossterm::queue;

    use super::*;

    #[test]
    fn working_shows_a_bar_with_no_amount_and_done_takes_it_off() {
        let mut out = Vec::new();
        queue!(out, SetProgress { working: true }).unwrap();
        queue!(out, SetProgress { working: false }).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b]9;4;3\x07\x1b]9;4;0\x07"
        );
    }
}
