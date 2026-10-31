//! Pictures in the input box, as Claude Code takes them. A dropped file arrives as its path,
//! typed or pasted in; one that names a picture that exists becomes an `[Image #N]` chip, and
//! so does a picture copied to the clipboard and pasted with Ctrl+V. The chips are numbered
//! across the conversation, and Backspace takes one whole, as it takes a long paste's label.
//! Each chip's picture is kept in the pictures folder from the moment its chip goes in, so a
//! file that goes soon after, as a screenshot from the corner preview does, still gets sent.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use argonauts_core::look::pictures::{Picture, has_picture_extension, media_type};

use super::{Composer, ComposerEvent, Saved, undo};

/// The pictures folder, in the system's temporary folder: pictures pasted from the clipboard,
/// and copies of the ones dropped in.
const PICTURES_DIR: &str = "argonauts-pictures";

fn pictures_dir() -> PathBuf {
    std::env::temp_dir().join(PICTURES_DIR)
}

/// What Ctrl+V reads a picture from: the clipboard, or a fake in tests. `holds` says at
/// once whether there's a picture, from the clipboard's types alone; `save` writes it out,
/// which for a large screenshot takes a while, so it runs off the key thread.
#[derive(Clone, Copy)]
pub(super) struct Clipboard {
    pub(super) holds: fn() -> bool,
    pub(super) save: fn(&Path) -> Option<PathBuf>,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self {
            holds: clipboard_holds_picture,
            save: clipboard_picture,
        }
    }
}

/// How a chip's picture is copied into the pictures folder, from the file as it was opened
/// when its chip went in: a plain copy, or a fake in tests.
#[derive(Clone, Copy)]
pub(super) struct Copier(pub(super) fn(File, &Path) -> std::io::Result<u64>);

impl Default for Copier {
    fn default() -> Self {
        Self(copy_file)
    }
}

impl std::fmt::Debug for Copier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Copier")
    }
}

/// A send waits this long at most for a copy still being made, then sends the file itself.
const COPY_WAIT: Duration = Duration::from_secs(10);

/// Pictures Ctrl+V is saving and chips' copies being made, off the key thread, shared by the
/// composer's copies.
#[derive(Debug, Clone, Default)]
pub(super) struct Arriving(Arc<Shared>);

#[derive(Debug, Default)]
struct Shared {
    saving: Mutex<Saving>,
    /// Told each time a copy is done.
    copied: Condvar,
}

#[derive(Debug, Default)]
struct Saving {
    on_the_way: usize,
    arrived: Vec<PathBuf>,
    /// The files this composer put in the pictures folder, removed when the conversation ends.
    saved: Vec<PathBuf>,
    /// Chips' copies still being made.
    copying: Vec<Kept>,
    /// Chips' copies that couldn't be made, whose chips send their files instead.
    uncopied: Vec<Kept>,
}

/// A chip's copy in the pictures folder, and the file it's made from.
#[derive(Debug, Clone)]
struct Kept {
    copy: PathBuf,
    from: PathBuf,
}

impl Drop for Saving {
    fn drop(&mut self) {
        for file in &self.saved {
            let _ = std::fs::remove_file(file);
        }
    }
}

impl Arriving {
    fn lock(&self) -> std::sync::MutexGuard<'_, Saving> {
        self.0
            .saving
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn copied(&self, copy: &Path, ok: bool) {
        let mut saving = self.lock();
        if let Some(at) = saving.copying.iter().position(|kept| kept.copy == copy) {
            let kept = saving.copying.remove(at);
            saving.saved.push(kept.copy.clone());
            if !ok {
                saving.uncopied.push(kept);
            }
        }
        drop(saving);
        self.0.copied.notify_all();
    }

    /// Waits up to `wait` for the copies still being made, then gives the ones that weren't
    /// made in time or at all.
    fn not_copied(&self, wait: Duration) -> Vec<Kept> {
        let saving = self.lock();
        let (mut saving, _) = self
            .0
            .copied
            .wait_timeout_while(saving, wait, |saving| !saving.copying.is_empty())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut missing = std::mem::take(&mut saving.uncopied);
        missing.extend(saving.copying.iter().cloned());
        missing
    }
}

impl std::fmt::Debug for Clipboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Clipboard")
    }
}

impl Composer {
    /// The pictures whose chips the prompt just sent held, for the main session's CLI.
    pub fn take_sent_pictures(&mut self) -> Vec<Picture> {
        std::mem::take(&mut self.sent_pictures)
    }

    /// A paste that is only paths to pictures that exist goes in as their chips. False leaves
    /// it to go in as text.
    pub(super) fn paste_pictures(&mut self, pasted: &str) -> bool {
        if self.shell || self.field {
            return false;
        }
        let Some(paths) = picture_paths(pasted) else {
            return false;
        };
        let before = self.saved();
        for path in paths {
            self.insert_picture(path);
        }
        self.edited_from(before, undo::EditKind::Other, false);
        self.edited();
        true
    }

    /// Ctrl+V: a picture copied to the clipboard, or a picture file copied in Finder. Its chip
    /// goes in once the picture is saved, which happens off the key thread so typing never
    /// waits on a large screenshot.
    pub(super) fn paste_clipboard(&mut self) -> ComposerEvent {
        if !(self.clipboard.holds)() {
            return ComposerEvent::Unhandled;
        }
        let save = self.clipboard.save;
        let dir = pictures_dir();
        let arriving = self.arriving.clone();
        arriving.lock().on_the_way += 1;
        std::thread::spawn(move || {
            let saved = save(&dir);
            let mut saving = arriving.lock();
            saving.on_the_way -= 1;
            if let Some(path) = saved {
                if path.starts_with(&dir) {
                    saving.saved.push(path.clone());
                }
                saving.arrived.push(path);
            }
        });
        ComposerEvent::Moved
    }

    /// The pictures Ctrl+V saved since the last look go in as their chips. True when any did.
    pub fn take_arrived_pictures(&mut self) -> bool {
        let arrived = std::mem::take(&mut self.arriving.lock().arrived);
        if arrived.is_empty() {
            return false;
        }
        let before: Saved = self.saved();
        for path in arrived {
            self.insert_picture(path);
        }
        self.edited_from(before, undo::EditKind::Other, false);
        self.edited();
        true
    }

    /// A picture Ctrl+V asked for is still being saved.
    pub fn pictures_arriving(&self) -> bool {
        self.arriving.lock().on_the_way > 0
    }

    /// The next chip, with a space after it, as Claude Code leaves one.
    fn insert_picture(&mut self, path: PathBuf) {
        self.next_picture += 1;
        let picture = Picture {
            number: self.next_picture,
            path: self.kept(path),
        };
        let label = picture.label();
        let at = self.draft.cursor;
        if at > 0 && !self.draft.text[..at].ends_with([' ', '\n']) {
            self.draft.insert(" ");
        }
        self.draft.insert_element(&label, label.clone());
        self.draft.insert(" ");
        self.pictures.push(picture);
    }

    /// Where the picture's copy goes in the pictures folder, since a screenshot dropped from
    /// the corner preview is deleted within seconds. The file is opened at once, which keeps
    /// it whole for the copy even once it's gone, and copied off the key thread, since a large
    /// one on another disk takes a while. The file itself when it's already there or can't be
    /// opened.
    fn kept(&self, path: PathBuf) -> PathBuf {
        let dir = pictures_dir();
        if path.starts_with(&dir) {
            return path;
        }
        let (Ok(file), Some(copy)) = (File::open(&path), copy_name(&dir, &path)) else {
            return path;
        };
        let kept = Kept {
            copy: copy.clone(),
            from: path,
        };
        self.arriving.lock().copying.push(kept);
        let (arriving, copier, to) = (self.arriving.clone(), self.copier, copy.clone());
        std::thread::spawn(move || {
            let copied = std::fs::create_dir_all(&dir).and_then(|()| (copier.0)(file, &to));
            arriving.copied(&to, copied.is_ok());
        });
        copy
    }

    /// The pictures whose chips `text`, the prompt as sent, still holds. A chip whose copy
    /// isn't made sends its file instead.
    pub(super) fn pictures_in(&mut self, text: &str) -> Vec<Picture> {
        for kept in self.arriving.not_copied(COPY_WAIT) {
            for picture in self.pictures.iter_mut().filter(|p| p.path == kept.copy) {
                picture.path = kept.from.clone();
            }
        }
        self.pictures
            .iter()
            .filter(|picture| text.contains(&picture.label()))
            .cloned()
            .collect()
    }

    /// The numbers of this conversation's pictures whose chips `text` holds, for the rows
    /// under a message that waited or joined a running turn.
    pub fn picture_numbers_in(&self, text: &str) -> Vec<u32> {
        argonauts_core::look::pictures::numbers_in(text)
            .into_iter()
            .filter(|number| {
                self.pictures
                    .iter()
                    .any(|picture| picture.number == *number)
            })
            .collect()
    }

    /// `/clear` started a new conversation, so chips count from 1 again, as Claude Code's do.
    pub fn start_conversation(&mut self) {
        self.resume_conversation(std::iter::empty());
    }

    /// Another conversation took the screen: its chips count on after the highest its
    /// `messages` hold. A chip still in the draft keeps its picture and its number.
    pub fn resume_conversation<'a>(&mut self, messages: impl IntoIterator<Item = &'a str>) {
        let draft = self.draft.text.clone();
        self.pictures
            .retain(|picture| draft.contains(&picture.label()));
        let held = self.pictures.iter().map(|picture| picture.number);
        let earlier = messages
            .into_iter()
            .flat_map(argonauts_core::look::pictures::numbers_in);
        self.next_picture = held.chain(earlier).max().unwrap_or(0);
    }
}

/// Each path in `pasted` when every one names a picture that exists: one per line, or several
/// on a line as a drop of more than one file gives them, with their escapes undone.
pub fn picture_paths(pasted: &str) -> Option<Vec<PathBuf>> {
    let words = split_paths(pasted.trim_ascii());
    if words.is_empty() {
        return None;
    }
    words
        .iter()
        .map(|word| picture_path(word))
        .collect::<Option<Vec<_>>>()
}

/// One pasted path as Claude Code reads it: the quotes around it go, `\ ` and the other
/// backslash escapes are undone, and a `file://` address becomes its path.
pub fn picture_path(word: &str) -> Option<PathBuf> {
    let word = word.trim_ascii();
    let unquoted = ['"', '\'']
        .iter()
        .find_map(|quote| {
            word.strip_prefix(*quote)
                .and_then(|rest| rest.strip_suffix(*quote))
        })
        .unwrap_or(word);
    let unescaped = unescape(unquoted);
    let path = match unescaped.strip_prefix("file://") {
        Some(address) => PathBuf::from(percent_decoded(address.trim_start_matches("localhost"))),
        None => PathBuf::from(unescaped),
    };
    let path = expand_home(path);
    (has_picture_extension(&path) && path.is_file() && media_type(&path).is_some()).then_some(path)
}

/// Splits on ASCII whitespace that isn't escaped or quoted, so `a\ b.png 'c d.png'` is two
/// paths. Other spaces belong to the name: a terminal escapes only ASCII ones in a drop, and
/// macOS names a screenshot with a narrow no-break space before its PM.
fn split_paths(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' if quote.is_none() => {
                word.push(c);
                word.extend(chars.next());
            }
            '"' | '\'' if quote == Some(c) => {
                quote = None;
                word.push(c);
            }
            '"' | '\'' if quote.is_none() && word.is_empty() => {
                quote = Some(c);
                word.push(c);
            }
            c if c.is_ascii_whitespace() && quote.is_none() => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// `\x` is `x`, and `\\` a backslash.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            c => out.push(c),
        }
    }
    out
}

fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = bytes
            .get(at + 1..at + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[at], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A name of its own in `dir` for a copy of `path`, with its extension.
fn copy_name(dir: &Path, path: &Path) -> Option<PathBuf> {
    static COPIES: AtomicU32 = AtomicU32::new(0);
    let extension = path.extension()?.to_str()?;
    let number = COPIES.fetch_add(1, Ordering::Relaxed);
    Some(dir.join(format!(
        "dropped-{}-{number}.{extension}",
        std::process::id()
    )))
}

fn copy_file(mut from: File, to: &Path) -> std::io::Result<u64> {
    std::io::copy(&mut from, &mut File::create(to)?)
}

fn expand_home(path: PathBuf) -> PathBuf {
    match (path.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(home)) => Path::new(&home).join(rest),
        _ => path,
    }
}

/// Whether the clipboard holds a picture or a copied file, from the types it lists alone,
/// which come back at once however large the picture is.
pub fn clipboard_holds_picture() -> bool {
    cfg!(target_os = "macos")
        && osascript(&["-e", "clipboard info"])
            .is_some_and(|info| info.contains("PNGf") || info.contains("furl"))
}

/// A picture on the clipboard, saved into `dir` as Claude Code saves one, or the picture file
/// copied in Finder. macOS only: elsewhere the terminal pastes a copied file's path itself.
pub fn clipboard_picture(dir: &Path) -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    if let Some(saved) = saved_clipboard_png(dir) {
        return Some(saved);
    }
    let copied = osascript(&["-e", "get POSIX path of (the clipboard as «class furl»)"])?;
    picture_path(copied.trim())
}

/// `the clipboard as «class PNGf»`, written to a file of its own in `dir`. The types are read
/// first, since asking for the picture itself prints all of it.
fn saved_clipboard_png(dir: &Path) -> Option<PathBuf> {
    if !osascript(&["-e", "clipboard info"]).is_some_and(|info| info.contains("PNGf")) {
        return None;
    }
    std::fs::create_dir_all(dir).ok()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    let path = dir.join(format!("pasted-{}-{stamp}.png", std::process::id()));
    let quoted = path
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    osascript(&[
        "-e",
        "set png_data to (the clipboard as «class PNGf»)",
        "-e",
        &format!("set fp to open for access POSIX file \"{quoted}\" with write permission"),
        "-e",
        "write png_data to fp",
        "-e",
        "close access fp",
    ])?;
    (media_type(&path).is_some()).then_some(path)
}

fn osascript(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("osascript")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests;
