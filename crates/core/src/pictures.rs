//! Pictures sent with a prompt: a file dropped or pasted into the input box, or a picture
//! pasted from the clipboard, shown there as an `[Image #N]` chip.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The kinds of picture the CLIs take, by file extension, as Claude Code reads a dropped path.
pub const EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Picture {
    /// The chip's number, counted across the conversation.
    pub number: u32,
    pub path: PathBuf,
}

impl Picture {
    /// `[Image #3]`, as the input box and the prompt's rows show it.
    pub fn label(&self) -> String {
        label(self.number)
    }

    /// Such as `image/png`, from the file's first bytes, or `None` when they aren't a picture
    /// the CLIs take.
    pub fn media_type(&self) -> Option<&'static str> {
        media_type(&self.path)
    }
}

/// The largest picture sent as an image: the API takes 5 MB of base64, which grows a file by a
/// third.
pub const LARGEST_SENT: u64 = 3_750_000;

impl Picture {
    /// The picture as an image block of a user message, as Claude Code's stream input takes
    /// it. `None` when the file can't be read, isn't a picture, or is too large to send.
    pub fn image_block(&self) -> Option<serde_json::Value> {
        use base64::Engine as _;
        let media_type = self.media_type()?;
        let size = std::fs::metadata(&self.path).ok()?.len();
        if size > LARGEST_SENT {
            return None;
        }
        let bytes = std::fs::read(&self.path).ok()?;
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        Some(serde_json::json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data},
        }))
    }
}

/// `text` with a line for each picture a CLI can't take as an image: its chip and its file,
/// so the agent can still open it.
pub fn with_paths(text: &str, pictures: &[&Picture]) -> String {
    if pictures.is_empty() {
        return text.to_owned();
    }
    let mut out = text.trim_end().to_owned();
    out.push_str("\n\nThese pictures came as files, since they couldn't be sent as images:");
    for picture in pictures {
        out.push_str(&format!("\n{} {}", picture.label(), picture.path.display()));
    }
    out
}

pub fn label(number: u32) -> String {
    format!("[Image #{number}]")
}

/// The numbers of the `[Image #N]` chips in `text`, in order, each once.
pub fn numbers_in(text: &str) -> Vec<u32> {
    let mut numbers = Vec::new();
    for (at, _) in text.match_indices("[Image #") {
        let rest = &text[at + "[Image #".len()..];
        let Some((digits, _)) = rest.split_once(']') else {
            continue;
        };
        if let Ok(number) = digits.parse::<u32>()
            && !numbers.contains(&number)
        {
            numbers.push(number);
        }
    }
    numbers
}

/// Whether `path` names a picture by its extension.
pub fn has_picture_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

/// The picture's media type from its first bytes, so a renamed file isn't sent as what it
/// isn't.
pub fn media_type(path: &Path) -> Option<&'static str> {
    use std::io::Read;
    let mut head = [0u8; 12];
    let mut file = std::fs::File::open(path).ok()?;
    let read = file.read(&mut head).ok()?;
    let head = &head[..read];
    if head.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if head.starts_with(b"GIF8") {
        Some("image/gif")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_is_known_by_its_bytes_and_its_extension() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("dot.PNG");
        std::fs::write(&png, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();
        assert!(has_picture_extension(&png));
        assert_eq!(media_type(&png), Some("image/png"));
        let fake = dir.path().join("notes.png");
        std::fs::write(&fake, "just text").unwrap();
        assert_eq!(media_type(&fake), None);
        assert!(!has_picture_extension(&dir.path().join("notes.txt")));
        assert_eq!(label(2), "[Image #2]");
    }

    #[test]
    fn the_chips_in_a_message_are_found_in_order_once_each() {
        let text = "compare [Image #3] with [Image #1], then [Image #3] again [Image #x] [Image #2";
        assert_eq!(numbers_in(text), [3, 1]);
        assert!(numbers_in("no pictures here").is_empty());
    }

    #[test]
    fn a_picture_goes_as_an_image_block_and_one_that_cant_as_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dot.png");
        std::fs::write(&path, [0x89, b'P', b'N', b'G', 1, 2, 3]).unwrap();
        let picture = Picture { number: 1, path };
        let block = picture.image_block().unwrap();
        assert_eq!(block["type"], "image");
        assert_eq!(block["source"]["media_type"], "image/png");
        assert_eq!(block["source"]["data"], "iVBORwECAw==");
        let gone = Picture {
            number: 2,
            path: dir.path().join("gone.png"),
        };
        assert_eq!(gone.image_block(), None);
        let text = with_paths("what's off here? [Image #2]", &[&gone]);
        assert!(
            text.starts_with("what's off here? [Image #2]\n\n"),
            "{text}"
        );
        assert!(
            text.ends_with(&format!("[Image #2] {}", gone.path.display())),
            "{text}"
        );
    }
}
