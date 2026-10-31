//! Small file helpers the replay set shares: JSON on disk, throwaway folders, and placing a
//! hidden test inside a checkout without following anything the checkout's commit planted.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde::de::DeserializeOwned;

use argonauts_core::base::clock;
use argonauts_core::create_private_dir_all;

use crate::store::error::{LearningError, Result};

/// A folder deleted when it goes out of scope, so an early return or an error never leaves a
/// checkout with hidden tests in it behind.
pub(super) struct Scratch(pub PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Unique within this machine: the process, the time, and a counter.
pub(super) fn nonce() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = clock::now_utc().unix_timestamp_nanos();
    format!(
        "{}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

pub(super) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|err| LearningError::Parse {
                path: path.to_owned(),
                message: err.to_string(),
            }),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

/// Written beside the target and renamed over it, so a reader never sees half a file.
pub(super) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = parent_of(path)?;
    create_private_dir_all(parent)?;
    let temp = parent.join(format!(".{}.tmp", nonce()));
    let bytes = serde_json::to_vec_pretty(value)?;
    let written = write_new(&temp, &bytes).and_then(|()| Ok(fs::rename(&temp, path)?));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

pub(super) fn append_json_line<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    create_private_dir_all(parent_of(path)?)?;
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&line)?;
    Ok(())
}

pub(super) fn read_json_lines<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|err| LearningError::Parse {
                path: path.to_owned(),
                message: err.to_string(),
            })
        })
        .collect()
}

/// Every `*.json` in `dir`, by file name.
pub(super) fn read_json_dir<T: DeserializeOwned>(dir: &Path) -> Result<Vec<T>> {
    let mut paths = match fs::read_dir(dir) {
        Ok(read) => read
            .map(|item| item.map(|item| item.path()))
            .collect::<std::io::Result<Vec<_>>>()?,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    paths.retain(|path| path.extension().is_some_and(|ext| ext == "json"));
    paths.sort();
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        if let Some(value) = read_json(&path)? {
            out.push(value);
        }
    }
    Ok(out)
}

pub(super) fn remove_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(fs::remove_dir_all(path)?),
        Ok(_) => Ok(fs::remove_file(path)?),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// A path that stays inside whatever folder it's joined to: relative, with no `..`.
pub(super) fn safe_relative(path: &Path) -> bool {
    path.components().next().is_some()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Writes `bytes` at `relative` under `root`. A link or file where a folder belongs is replaced
/// by a real folder, and whatever sits at the target is removed first, because the checkout's
/// commit came from an agent that could plant a link to carry a hidden test out of it.
pub(super) fn place(root: &Path, relative: &Path, bytes: &[u8]) -> Result<()> {
    if !safe_relative(relative) {
        return Err(LearningError::Refused(format!(
            "{} isn't a path inside the repository",
            relative.display()
        )));
    }
    let mut dir = root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        if components.peek().is_none() {
            let target = dir.join(component);
            remove_if_present(&target)?;
            return write_new(&target, bytes);
        }
        dir.push(component);
        match fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                fs::remove_file(&dir)?;
                create_private_dir_all(&dir)?;
            }
            Err(err) if err.kind() == ErrorKind::NotFound => create_private_dir_all(&dir)?,
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

/// The bytes of a regular file under `root`, refusing links so a test can't name a file
/// outside the checkout.
pub(super) fn read_regular(root: &Path, relative: &Path) -> Result<Vec<u8>> {
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component);
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err(LearningError::Refused(format!(
                "{} is a link",
                relative.display()
            )));
        }
    }
    Ok(fs::read(path)?)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

fn parent_of(path: &Path) -> Result<&Path> {
    path.parent()
        .ok_or_else(|| LearningError::Refused(format!("{} has no parent folder", path.display())))
}
