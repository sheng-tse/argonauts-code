//! What Summaries keeps in `~/.argonauts/behaviors/`, so no summary is ever made twice:
//!
//! - `summaries.jsonl`: one line per summary, keyed by the action (run, session, tool call),
//!   the summarizer and the prompt's version, with the key of the text it summarized;
//! - `vectors.jsonl`: one line per summary text and embedder;
//! - `layout.json`: the map's place for each action, and which summarizer, prompt and embedder
//!   made it.
//!
//! Lines are appended as they're made, so an update that stops keeps what it finished. Only
//! pruning a deleted run's entries writes the files again.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use argonauts_core::base::layout::user_files;
use argonauts_core::{Paths, create_private_dir_all, write_private_file};
use serde::{Deserialize, Serialize};

use super::action::ActionKey;
use super::project::Frame;

const SUMMARIES: &str = "summaries.jsonl";
const VECTORS: &str = "vectors.jsonl";
const LAYOUT: &str = "layout.json";

/// One summary as it's kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SummaryLine {
    #[serde(flatten)]
    pub action: ActionKey,
    pub summarizer: String,
    pub prompt: String,
    /// The key of the text the summarizer read, so an identical action reuses it.
    pub text: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct VectorLine {
    pub embedder: String,
    /// The key of the summary's words.
    pub summary: String,
    pub vector: Vec<f32>,
}

/// Who made a map: one summarizer, one prompt and one embedder for every action on it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct MapMaker {
    pub summarizer: String,
    pub prompt: String,
    pub embedder: String,
}

/// An action's place on the map, each axis in 0..1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Placed {
    #[serde(flatten)]
    pub action: ActionKey,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Layout {
    #[serde(flatten)]
    pub maker: MapMaker,
    /// Where the fit's positions lie, so the next fit keeps these places.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<Frame>,
    pub points: Vec<Placed>,
}

/// The summaries one summarizer wrote with one prompt.
#[derive(Debug, Clone, Default)]
pub(crate) struct MapSummaries {
    pub by_action: HashMap<ActionKey, String>,
    /// By the key of the text summarized.
    pub by_text: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(paths: &Paths) -> Store {
        Store::at(&paths.home().join(user_files::BEHAVIORS))
    }

    pub fn at(dir: &Path) -> Store {
        Store {
            dir: dir.to_owned(),
        }
    }

    /// Every summary kept, oldest first. A torn last line is skipped.
    pub fn summary_lines(&self) -> Vec<SummaryLine> {
        read_lines(&self.dir.join(SUMMARIES))
    }

    pub fn summaries(&self, summarizer: &str, prompt: &str) -> MapSummaries {
        let mut kept = MapSummaries::default();
        for line in self.summary_lines() {
            if line.summarizer != summarizer || line.prompt != prompt {
                continue;
            }
            kept.by_text.insert(line.text, line.summary.clone());
            kept.by_action.insert(line.action, line.summary);
        }
        kept
    }

    pub fn add_summaries(&self, lines: &[SummaryLine]) -> io::Result<()> {
        append(&self.dir, SUMMARIES, lines)
    }

    /// The vectors `embedder` made, by the key of the summary's words.
    pub fn vectors(&self, embedder: &str) -> HashMap<String, Vec<f32>> {
        read_lines::<VectorLine>(&self.dir.join(VECTORS))
            .into_iter()
            .filter(|line| line.embedder == embedder)
            .map(|line| (line.summary, line.vector))
            .collect()
    }

    pub fn add_vectors(&self, lines: &[VectorLine]) -> io::Result<()> {
        append(&self.dir, VECTORS, lines)
    }

    pub fn layout(&self) -> Option<Layout> {
        let bytes = fs::read(self.dir.join(LAYOUT)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Who made the kept map, read without keeping its places.
    pub fn maker(&self) -> Option<MapMaker> {
        let bytes = fs::read(self.dir.join(LAYOUT)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Written whole beside the old one, then moved over it, so a reader never sees half.
    pub fn save_layout(&self, layout: &Layout) -> io::Result<()> {
        create_private_dir_all(&self.dir)?;
        let next = self.dir.join(format!("{LAYOUT}.next"));
        write_private_file(
            &next,
            &serde_json::to_vec(layout).map_err(io::Error::other)?,
        )?;
        fs::rename(next, self.dir.join(LAYOUT))
    }

    /// Every run with a summary or a dot kept, each by its project's state folder and its id.
    pub fn runs(&self) -> HashSet<(String, String)> {
        let summarized = self.summary_lines().into_iter().map(|line| line.action);
        let placed = self
            .layout()
            .into_iter()
            .flat_map(|layout| layout.points)
            .map(|point| point.action);
        summarized
            .chain(placed)
            .map(|action| (action.repo, action.run))
            .collect()
    }

    /// Drops every entry of the runs that are `gone`, each run known by its project's state
    /// folder and its id, and returns how many summaries went. The vectors stay: they're keyed
    /// by words, not by a run.
    pub fn prune(&self, gone: &HashSet<(String, String)>) -> io::Result<usize> {
        if gone.is_empty() {
            return Ok(0);
        }
        let stays = |action: &ActionKey| !gone.contains(&(action.repo.clone(), action.run.clone()));
        let lines = self.summary_lines();
        let kept: Vec<&SummaryLine> = lines.iter().filter(|line| stays(&line.action)).collect();
        let dropped = lines.len() - kept.len();
        if dropped > 0 {
            rewrite(&self.dir, SUMMARIES, &kept)?;
        }
        if let Some(mut layout) = self.layout() {
            let before = layout.points.len();
            layout.points.retain(|point| stays(&point.action));
            if layout.points.len() != before {
                self.save_layout(&layout)?;
            }
        }
        Ok(dropped)
    }
}

fn read_lines<T: for<'de> Deserialize<'de>>(path: &Path) -> Vec<T> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn append<T: Serialize>(dir: &Path, name: &str, lines: &[T]) -> io::Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    create_private_dir_all(dir)?;
    let mut text = String::new();
    for line in lines {
        text.push_str(&serde_json::to_string(line).map_err(io::Error::other)?);
        text.push('\n');
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(dir.join(name))?;
    file.write_all(text.as_bytes())?;
    file.flush()
}

fn rewrite<T: Serialize>(dir: &Path, name: &str, lines: &[T]) -> io::Result<()> {
    let next = dir.join(format!("{name}.next"));
    let _ = fs::remove_file(&next);
    append(dir, &format!("{name}.next"), lines)?;
    if lines.is_empty() {
        write_private_file(&next, b"")?;
    }
    fs::rename(next, dir.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COUNCIL: &str = "council-3f9a1c2b4d5e";

    fn key(run: &str, call: &str) -> ActionKey {
        ActionKey {
            repo: COUNCIL.to_owned(),
            run: run.to_owned(),
            session: "s-1".to_owned(),
            call: call.to_owned(),
        }
    }

    fn line(run: &str, call: &str, summarizer: &str, summary: &str) -> SummaryLine {
        SummaryLine {
            action: key(run, call),
            summarizer: summarizer.to_owned(),
            prompt: "1".to_owned(),
            text: format!("text-{call}"),
            summary: summary.to_owned(),
        }
    }

    #[test]
    fn summaries_are_kept_per_summarizer_and_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(&dir.path().join("behaviors"));
        store
            .add_summaries(&[
                line("run-1", "c1", "ollama/qwen", "Read git history"),
                line("run-1", "c2", "claude-code/haiku", "Ran the tests"),
            ])
            .unwrap();
        let qwen = store.summaries("ollama/qwen", "1");
        assert_eq!(qwen.by_action.len(), 1);
        assert_eq!(qwen.by_action[&key("run-1", "c1")], "Read git history");
        assert_eq!(qwen.by_text["text-c1"], "Read git history");
        assert!(store.summaries("ollama/qwen", "2").by_action.is_empty());
    }

    #[test]
    fn a_torn_last_line_is_skipped_and_the_rest_read() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        store
            .add_summaries(&[line("run-1", "c1", "ollama/qwen", "Read")])
            .unwrap();
        let path = dir.path().join(SUMMARIES);
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str("{\"run\":\"run-1\",\"sess");
        fs::write(&path, text).unwrap();
        assert_eq!(store.summary_lines().len(), 1);
    }

    #[test]
    fn a_deleted_runs_summaries_and_places_go_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        store
            .add_summaries(&[
                line("run-1", "c1", "ollama/qwen", "Read"),
                line("run-2", "c1", "ollama/qwen", "Wrote"),
            ])
            .unwrap();
        let maker = MapMaker {
            summarizer: "ollama/qwen".to_owned(),
            prompt: "1".to_owned(),
            embedder: "words".to_owned(),
        };
        let points = ["run-1", "run-2"]
            .iter()
            .map(|run| Placed {
                action: key(run, "c1"),
                x: 0.5,
                y: 0.5,
            })
            .collect();
        store
            .save_layout(&Layout {
                maker,
                frame: None,
                points,
            })
            .unwrap();
        assert_eq!(
            store.runs(),
            [
                (COUNCIL.to_owned(), "run-1".to_owned()),
                (COUNCIL.to_owned(), "run-2".to_owned())
            ]
            .into()
        );
        let gone: HashSet<(String, String)> = [(COUNCIL.to_owned(), "run-1".to_owned())].into();
        assert_eq!(store.prune(&gone).unwrap(), 1);
        let left = store.summary_lines();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].action.run, "run-2");
        let layout = store.layout().unwrap();
        assert_eq!(layout.points.len(), 1);
        assert_eq!(layout.points[0].action.run, "run-2");
        assert_eq!(store.prune(&HashSet::new()).unwrap(), 0, "nothing gone");
        let gone: HashSet<(String, String)> = [(COUNCIL.to_owned(), "run-2".to_owned())].into();
        assert_eq!(store.prune(&gone).unwrap(), 1);
        assert!(store.summary_lines().is_empty());
    }

    #[test]
    fn an_entry_kept_without_its_project_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let path = dir.path().join(SUMMARIES);
        let older = serde_json::json!({"run": "run-1", "session": "s-1", "call": "c1",
            "summarizer": "ollama/qwen", "prompt": "1", "text": "t", "summary": "Read"});
        fs::write(&path, format!("{older}\n")).unwrap();
        store
            .add_summaries(&[line("run-1", "c2", "ollama/qwen", "Wrote")])
            .unwrap();
        let gone: HashSet<(String, String)> = [(String::new(), "run-1".to_owned())].into();
        assert_eq!(
            store.prune(&gone).unwrap(),
            1,
            "the line that names no project"
        );
        let left = store.summary_lines();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].action.repo, COUNCIL);
    }

    #[test]
    fn its_files_are_yours_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(&dir.path().join("behaviors"));
        store
            .add_summaries(&[line("run-1", "c1", "ollama/qwen", "Read")])
            .unwrap();
        let mode = fs::metadata(dir.path().join("behaviors").join(SUMMARIES))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
