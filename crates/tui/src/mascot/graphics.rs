//! The terminals that draw pictures, and the escapes that put one in the cells at the cursor.
//! Ghostty, Kitty and WezTerm take the kitty graphics protocol; iTerm2 takes its own. tmux and
//! screen pass neither through, so under them the header draws its block cells.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use super::{ROWS, WIDTH};

/// The kitty protocol's payload comes in chunks of at most this many base64 bytes.
const CHUNK: usize = 4096;

/// How a terminal takes a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Graphics {
    /// The kitty graphics protocol: Ghostty, Kitty and WezTerm.
    Kitty,
    /// iTerm2's inline images.
    Iterm,
}

impl Graphics {
    /// The protocol the terminal named by `var` takes, if any. Pictures go only straight to the
    /// terminal, never through a multiplexer.
    pub fn detect(var: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let set = |name: &str| var(name).is_some_and(|value| !value.is_empty());
        let term = var("TERM").unwrap_or_default();
        if set("TMUX") || set("STY") || term.starts_with("screen") || term.starts_with("tmux") {
            return None;
        }
        let program = var("TERM_PROGRAM").unwrap_or_default();
        let kitty = matches!(program.as_str(), "ghostty" | "WezTerm" | "kitty")
            || matches!(term.as_str(), "xterm-kitty" | "xterm-ghostty" | "wezterm")
            || set("KITTY_WINDOW_ID")
            || set("GHOSTTY_RESOURCES_DIR")
            || set("WEZTERM_PANE");
        if kitty {
            return Some(Self::Kitty);
        }
        let iterm = program == "iTerm.app" || var("LC_TERMINAL").as_deref() == Some("iTerm2");
        iterm.then_some(Self::Iterm)
    }

    /// From the app's own environment.
    pub fn from_process() -> Option<Self> {
        Self::detect(|name| std::env::var(name).ok())
    }
}

/// The escapes that draw `png` in the mascot's cells from the cursor, leaving the cursor where
/// it was. `id` names it for the kitty protocol, so it can be swapped or taken away later.
pub fn draw(graphics: Graphics, png: &[u8], id: u32) -> Vec<u8> {
    draw_over(graphics, png, id, (WIDTH, ROWS))
}

/// Like [`draw`], over `cells`, columns by rows, such as a map's.
pub fn draw_over(
    graphics: Graphics,
    png: &[u8],
    id: u32,
    (columns, rows): (usize, usize),
) -> Vec<u8> {
    match graphics {
        Graphics::Kitty => kitty_send(
            png,
            &format!("a=T,f=100,q=2,C=1,c={columns},r={rows},i={id}"),
        ),
        Graphics::Iterm => iterm(png, (columns, rows)),
    }
}

/// Sends `png` to a kitty-protocol terminal under `id` without showing it, for a frame that
/// [`show`] puts on screen later.
pub fn store(png: &[u8], id: u32) -> Vec<u8> {
    kitty_send(png, &format!("a=t,f=100,q=2,i={id}"))
}

/// Takes the picture stored under `id` off the screen, keeping it for its next turn.
pub fn hide(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=i,q=2,i={id}\x1b\\").into_bytes()
}

/// Shows the picture stored under `id` in the mascot's cells from the cursor, and takes the
/// one under `before` off the screen, keeping it for its next turn.
pub fn show(id: u32, before: Option<u32>) -> Vec<u8> {
    let mut out = format!("\x1b_Ga=p,q=2,C=1,c={WIDTH},r={ROWS},i={id}\x1b\\");
    if let Some(before) = before.filter(|before| *before != id) {
        out.push_str(&format!("\x1b_Ga=d,d=i,q=2,i={before}\x1b\\"));
    }
    out.into_bytes()
}

/// Frees the pictures under `ids`, from the screen and from the terminal's memory.
pub fn forget(ids: impl IntoIterator<Item = u32>) -> Vec<u8> {
    ids.into_iter()
        .map(|id| format!("\x1b_Ga=d,d=I,q=2,i={id}\x1b\\"))
        .collect::<String>()
        .into_bytes()
}

/// The kitty protocol's escapes for `png` with `keys`, its payload in chunks.
fn kitty_send(png: &[u8], keys: &str) -> Vec<u8> {
    let data = STANDARD.encode(png);
    let chunks: Vec<&str> = data
        .as_bytes()
        .chunks(CHUNK)
        // Base64 is ASCII, so every chunk is whole characters.
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
        .collect();
    let mut out = String::with_capacity(data.len() + 64 * chunks.len());
    for (at, chunk) in chunks.iter().enumerate() {
        let more = u8::from(at + 1 < chunks.len());
        if at == 0 {
            out.push_str(&format!("\x1b_G{keys},m={more};{chunk}\x1b\\"));
        } else {
            out.push_str(&format!("\x1b_Gm={more};{chunk}\x1b\\"));
        }
    }
    out.into_bytes()
}

/// iTerm2's inline image over `cells`. It moves the cursor, so the cursor is kept.
fn iterm(png: &[u8], (columns, rows): (usize, usize)) -> Vec<u8> {
    format!(
        "\x1b7\x1b]1337;File=inline=1;size={};width={columns};height={rows}:{}\x07\x1b8",
        png.len(),
        STANDARD.encode(png)
    )
    .into_bytes()
}

/// The PNGs inside `escapes`, in order, for checking what was sent.
#[cfg(test)]
pub(crate) fn pictures_in(escapes: &[u8]) -> Vec<Vec<u8>> {
    let text = String::from_utf8_lossy(escapes);
    let mut pictures = Vec::new();
    let mut data = String::new();
    for part in text.split("\x1b_G").skip(1) {
        let Some((keys, rest)) = part.split_once(';') else {
            continue;
        };
        let payload = rest.split("\x1b\\").next().unwrap_or_default();
        data.push_str(payload);
        if keys.split(',').any(|key| key == "m=0") {
            pictures.push(STANDARD.decode(&data).unwrap_or_default());
            data.clear();
        }
    }
    for part in text.split("\x1b]1337;File=").skip(1) {
        let payload = part
            .split_once(':')
            .map(|(_, rest)| rest.split('\x07').next().unwrap_or_default())
            .unwrap_or_default();
        pictures.push(STANDARD.decode(payload).unwrap_or_default());
    }
    pictures
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn ghostty_kitty_and_wezterm_take_the_kitty_protocol_and_iterm2_its_own() {
        let kitty = Some(Graphics::Kitty);
        assert_eq!(Graphics::detect(env(&[("TERM_PROGRAM", "ghostty")])), kitty);
        assert_eq!(Graphics::detect(env(&[("TERM", "xterm-ghostty")])), kitty);
        assert_eq!(Graphics::detect(env(&[("TERM", "xterm-kitty")])), kitty);
        assert_eq!(Graphics::detect(env(&[("KITTY_WINDOW_ID", "1")])), kitty);
        assert_eq!(Graphics::detect(env(&[("TERM_PROGRAM", "WezTerm")])), kitty);
        assert_eq!(
            Graphics::detect(env(&[("TERM_PROGRAM", "iTerm.app")])),
            Some(Graphics::Iterm)
        );
        assert_eq!(
            Graphics::detect(env(&[("LC_TERMINAL", "iTerm2")])),
            Some(Graphics::Iterm)
        );
        assert_eq!(
            Graphics::detect(env(&[("TERM_PROGRAM", "Apple_Terminal")])),
            None
        );
        assert_eq!(Graphics::detect(env(&[("TERM", "xterm-256color")])), None);
        // A multiplexer passes no pictures through.
        let tmux = [
            ("TERM_PROGRAM", "ghostty"),
            ("TMUX", "/tmp/tmux-501/default,1,0"),
        ];
        assert_eq!(Graphics::detect(env(&tmux)), None);
        let screen = [("TERM", "screen-256color"), ("KITTY_WINDOW_ID", "1")];
        assert_eq!(Graphics::detect(env(&screen)), None);
    }

    #[test]
    fn a_kitty_picture_goes_in_chunks_into_nine_by_three_cells_and_leaves_the_cursor() {
        let png = vec![7u8; 5000];
        let sent = String::from_utf8(draw(Graphics::Kitty, &png, 42)).unwrap();
        let parts: Vec<&str> = sent
            .split("\x1b\\")
            .filter(|part| !part.is_empty())
            .collect();
        // 5000 bytes are 6668 base64 characters: two chunks.
        assert_eq!(parts.len(), 2);
        assert!(parts[0].starts_with("\x1b_Ga=T,f=100,q=2,C=1,c=9,r=3,i=42,m=1;"));
        assert!(parts[1].starts_with("\x1b_Gm=0;"));
        assert_eq!(pictures_in(sent.as_bytes()), [png]);
    }

    #[test]
    fn an_iterm2_picture_names_its_cells_and_puts_the_cursor_back() {
        let sent = String::from_utf8(draw(Graphics::Iterm, b"png", 1)).unwrap();
        assert_eq!(
            sent,
            "\x1b7\x1b]1337;File=inline=1;size=3;width=9;height=3:cG5n\x07\x1b8"
        );
    }

    #[test]
    fn a_frame_shows_and_the_one_before_leaves() {
        assert_eq!(
            String::from_utf8(show(11, Some(10))).unwrap(),
            "\x1b_Ga=p,q=2,C=1,c=9,r=3,i=11\x1b\\\x1b_Ga=d,d=i,q=2,i=10\x1b\\"
        );
        assert_eq!(
            String::from_utf8(show(11, None)).unwrap(),
            "\x1b_Ga=p,q=2,C=1,c=9,r=3,i=11\x1b\\"
        );
        assert_eq!(
            String::from_utf8(forget([3, 4])).unwrap(),
            "\x1b_Ga=d,d=I,q=2,i=3\x1b\\\x1b_Ga=d,d=I,q=2,i=4\x1b\\"
        );
    }
}
