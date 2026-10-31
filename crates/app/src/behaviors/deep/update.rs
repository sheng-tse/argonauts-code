//! An update of Summaries, and opening it. An update prunes what deleted runs left,
//! summarizes only actions without a summary, embeds only summaries without a vector, and
//! places only the new dots, the old ones staying where they were. A first map is placed as
//! its summaries come, a share at a time, so it fills in while it goes. A map with dots is
//! replaced only by a whole new one: its update places once, when every summary came, and a
//! stopped or failed one leaves it as it was. Opening reads what's kept and calls no model.

use std::collections::{HashMap, HashSet};
use std::io;

use tokio_util::sync::CancellationToken;

use super::action::{ActionKey, DeepAction, text_key};
use super::embed::{Embedder, PER_EMBEDDING_REQUEST, word_vectors};
use super::project::{Frame, ToPlace, project};
use super::prompt::{PER_REQUEST, PROMPT_VERSION};
use super::records::Recorded;
use super::store::{Layout, MapMaker, Placed, Store, VectorLine};
use super::summaries::{Progress, Summarized, needed, summarize};
use super::summarize::Summarizer;
use argonauts_core::behaviors::deep_map::FewerDots;
use argonauts_core::settings::behaviors::WORD_VECTORS;

/// The fewest texts summarized between two placements of a first map, and how many
/// placements it makes at most before its last.
const PLACE_AFTER_LEAST: usize = 4 * PER_REQUEST;
const PLACEMENTS: usize = 8;

/// What turns the summaries into vectors for this update.
pub(crate) enum EmbedWith<'a> {
    Model(&'a dyn Embedder),
    /// Word vectors, made from the summaries themselves, as the settings ask.
    Words,
    /// The embedding model the settings would use doesn't answer now, for this reason.
    Missing(String),
}

/// Whether an update may go ahead as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Before {
    Go,
    /// The map was made by another summarizer or prompt. One map never mixes two, so this one
    /// would summarize every action again, which asks first.
    Redo {
        made_by: String,
        actions: usize,
    },
}

/// Whether summarizing with `summarizer` keeps the map that's there.
pub(crate) fn before(store: &Store, summarizer: &str) -> Before {
    match store.layout() {
        Some(layout)
            if !layout.points.is_empty()
                && (layout.maker.summarizer != summarizer
                    || layout.maker.prompt != PROMPT_VERSION) =>
        {
            Before::Redo {
                made_by: layout.maker.summarizer,
                actions: layout.points.len(),
            }
        }
        _ => Before::Go,
    }
}

/// What an update did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Updated {
    pub pruned: usize,
    pub summarized: Summarized,
    /// Summaries embedded now.
    pub embedded: usize,
    /// Dots on the map, and how many of them are new.
    pub placed: usize,
    pub new_places: usize,
    /// Why the embedder wasn't used: a new map was placed by word vectors instead, and a map
    /// an embedding model placed took no new dots.
    pub embedder_failed: Option<String>,
    /// The folders and logs that couldn't be read, each with why. Their runs' dots stay.
    pub unread: Vec<String>,
    /// A whole new map that wasn't swapped in, since it had fewer dots than the map there.
    pub fewer: Option<FewerDots>,
}

/// What one placement did.
#[derive(Debug, Default)]
struct Placing {
    embedded: usize,
    placed: usize,
    new_places: usize,
    embedder_failed: Option<String>,
    fewer: Option<FewerDots>,
}

/// Updates the map from `recorded`, every action recorded, summarizing those of them `wanted`
/// says. A first map's dots are placed after each share of the summaries, `placed` hearing of
/// it. Stopping or failing keeps every summary it finished, and places nothing more, so a map
/// that was there stays as it was.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn update(
    recorded: &Recorded,
    wanted: impl Fn(&DeepAction) -> bool,
    store: &Store,
    summarizer: &dyn Summarizer,
    embed_with: EmbedWith<'_>,
    stop: &CancellationToken,
    progress: &mut (dyn FnMut(Progress) + Send),
    placed: &mut (dyn FnMut() + Send),
) -> io::Result<Updated> {
    let pruned = store.prune(&recorded.gone(&store.runs()))?;
    let scope: Vec<DeepAction> = recorded
        .actions
        .iter()
        .filter(|a| wanted(a))
        .cloned()
        .collect();
    let summarizer_id = summarizer.id().to_string();
    let replaces = store
        .layout()
        .is_some_and(|layout| !layout.points.is_empty());
    let share = match replaces {
        true => None,
        false => {
            let to_send = needed(&scope, store, &summarizer_id).texts.len();
            Some(to_send.div_ceil(PLACEMENTS).max(PLACE_AFTER_LEAST))
        }
    };
    let mut summarized: Option<Summarized> = None;
    let mut done = Placing::default();
    loop {
        let part = summarize(&scope, store, summarizer, stop, progress, share).await?;
        let more = part.paused;
        let total = summarized.get_or_insert_with(|| Summarized {
            already: part.already,
            ..Summarized::default()
        });
        total.reused += part.reused;
        total.written += part.written;
        total.requests += part.requests;
        total.missing = part.missing;
        total.stopped = part.stopped;
        total.failed = part.failed;
        if total.stopped || (replaces && total.failed.is_some()) {
            break;
        }
        let placing = place(&recorded.actions, store, &summarizer_id, &embed_with, stop).await?;
        done.embedded += placing.embedded;
        done.new_places += placing.new_places;
        done.placed = placing.placed;
        done.embedder_failed = done.embedder_failed.or(placing.embedder_failed);
        if placing.fewer.is_some() {
            done.fewer = placing.fewer;
            break;
        }
        if !more || total.failed.is_some() {
            break;
        }
        if placing.new_places > 0 {
            placed();
        }
    }
    if done.placed == 0 {
        done.placed = store.layout().map_or(0, |layout| layout.points.len());
    }
    Ok(Updated {
        pruned,
        summarized: summarized.unwrap_or_default(),
        embedded: done.embedded,
        placed: done.placed,
        new_places: done.new_places,
        embedder_failed: done.embedder_failed,
        unread: recorded.unread.clone(),
        fewer: done.fewer,
    })
}

/// Places every summarized action of `recorded` not on the map yet, the others staying where
/// they were, and keeps the map. A dot whose action wasn't read this time, as when its folder
/// couldn't be, stays where it was. A map keeps the embedder that placed it: when that one
/// doesn't answer, nothing new is placed and the summaries wait for the next update. A new map
/// whose embedding model doesn't answer is placed by word vectors, and says why.
async fn place(
    recorded: &[DeepAction],
    store: &Store,
    summarizer_id: &str,
    embed_with: &EmbedWith<'_>,
    stop: &CancellationToken,
) -> io::Result<Placing> {
    let kept = store.summaries(summarizer_id, PROMPT_VERSION);
    let on_map: Vec<(&DeepAction, &String)> = recorded
        .iter()
        .filter_map(|action| Some((action, kept.by_action.get(&action.key)?)))
        .collect();
    let texts: Vec<String> = on_map
        .iter()
        .map(|(_, summary)| (*summary).clone())
        .collect();
    let last = store
        .layout()
        .filter(|layout| layout.maker.summarizer == summarizer_id)
        .filter(|layout| layout.maker.prompt == PROMPT_VERSION);
    let as_it_was = |why: String| Placing {
        placed: last.as_ref().map_or(0, |layout| layout.points.len()),
        embedder_failed: Some(why),
        ..Placing::default()
    };
    let words = || (word_vectors(&texts), WORD_VECTORS.to_owned());
    let map_embedder = last.as_ref().map(|layout| layout.maker.embedder.as_str());
    let mut embedder_failed = None;
    let mut embedded = 0;
    let (vectors, embedder) = match (embed_with, map_embedder) {
        (_, Some(WORD_VECTORS)) | (EmbedWith::Words, None) => words(),
        (EmbedWith::Missing(why), None) => {
            embedder_failed = Some(why.clone());
            words()
        }
        (EmbedWith::Missing(why), Some(_)) => return Ok(as_it_was(why.clone())),
        (EmbedWith::Words, Some(_)) => words(),
        (EmbedWith::Model(model), map_embedder) => match model_vectors(&texts, store, *model).await
        {
            Ok((vectors, made)) => {
                embedded = made;
                (vectors, model.id())
            }
            Err(why) if map_embedder.is_some() => return Ok(as_it_was(why)),
            Err(why) => {
                embedder_failed = Some(why);
                words()
            }
        },
    };
    if stop.is_cancelled() {
        return Ok(Placing {
            embedded,
            placed: last.as_ref().map_or(0, |layout| layout.points.len()),
            ..Placing::default()
        });
    }
    let maker = MapMaker {
        summarizer: summarizer_id.to_owned(),
        prompt: PROMPT_VERSION.to_owned(),
        embedder,
    };
    let last = last.filter(|layout| layout.maker == maker);
    let old: HashMap<&ActionKey, (f32, f32)> = last
        .iter()
        .flat_map(|layout| &layout.points)
        .map(|point| (&point.action, (point.x, point.y)))
        .collect();
    let rows: Vec<Row> = on_map
        .iter()
        .zip(vectors)
        .map(|((action, _), vector)| Row {
            vector,
            group: group_of(action),
            old: old.get(&action.key).copied(),
        })
        .collect();
    let new_places = rows.iter().filter(|row| row.old.is_none()).count();
    // Every dot is where it was, so the map is kept as it is, frame and all.
    if new_places == 0 && last.is_some() {
        return Ok(Placing {
            embedded,
            placed: store.layout().map_or(0, |layout| layout.points.len()),
            embedder_failed,
            ..Placing::default()
        });
    }
    let frame: Option<Frame> = last.as_ref().and_then(|layout| layout.frame);
    let watching = stop.clone();
    // The fit is seconds of arithmetic on a big map, kept off the threads that answer you.
    let placement = tokio::task::spawn_blocking(move || {
        let points: Vec<ToPlace<'_>> = rows
            .iter()
            .map(|row| ToPlace {
                vector: &row.vector,
                group: &row.group,
                old: row.old,
            })
            .collect();
        project(&points, frame, &|| watching.is_cancelled())
    })
    .await
    .map_err(io::Error::other)?;
    let on_screen = store.layout().map_or(0, |layout| layout.points.len());
    let Some(placement) = placement else {
        return Ok(Placing {
            embedded,
            placed: on_screen,
            ..Placing::default()
        });
    };
    let mut points: Vec<Placed> = on_map
        .iter()
        .zip(placement.places)
        .map(|((action, _), (x, y))| Placed {
            action: action.key.clone(),
            x,
            y,
        })
        .collect();
    let placed_now: HashSet<&ActionKey> = on_map.iter().map(|(action, _)| &action.key).collect();
    let unread: Vec<Placed> = last
        .iter()
        .flat_map(|layout| &layout.points)
        .filter(|point| !placed_now.contains(&point.action))
        .cloned()
        .collect();
    points.extend(unread);
    let layout = Layout {
        maker,
        frame: Some(placement.frame),
        points,
    };
    // A save never leaves fewer dots than the map has: the dots of runs that are gone left
    // it before, when the update pruned them.
    if layout.points.len() < on_screen {
        return Ok(Placing {
            embedded,
            placed: on_screen,
            embedder_failed,
            fewer: Some(FewerDots {
                made: layout.points.len(),
                shown: on_screen,
            }),
            ..Placing::default()
        });
    }
    if layout.points.is_empty() {
        return Ok(Placing {
            embedded,
            embedder_failed,
            ..Placing::default()
        });
    }
    store.save_layout(&layout)?;
    Ok(Placing {
        embedded,
        placed: layout.points.len(),
        new_places,
        embedder_failed,
        fewer: None,
    })
}

/// An action to place, as the fit is handed it.
struct Row {
    vector: Vec<f32>,
    group: String,
    old: Option<(f32, f32)>,
}

/// The map compares models, so their groups are oversampled to fair weight: the CLI and the
/// model it ran.
fn group_of(action: &DeepAction) -> String {
    format!("{}/{}", action.cli, action.model.as_deref().unwrap_or(""))
}

/// A vector for each of `texts`: kept ones read back, the rest embedded and kept.
async fn model_vectors(
    texts: &[String],
    store: &Store,
    embedder: &dyn Embedder,
) -> Result<(Vec<Vec<f32>>, usize), String> {
    let id = embedder.id();
    let mut kept = store.vectors(&id);
    let mut missing: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for text in texts {
        let key = text_key(text);
        if !kept.contains_key(&key) && seen.insert(key) {
            missing.push(text.clone());
        }
    }
    let mut embedded = 0;
    for chunk in missing.chunks(PER_EMBEDDING_REQUEST) {
        let vectors = embedder.embed(chunk).await?;
        let lines: Vec<VectorLine> = chunk
            .iter()
            .zip(vectors)
            .map(|(text, vector)| VectorLine {
                embedder: id.clone(),
                summary: text_key(text),
                vector,
            })
            .collect();
        store.add_vectors(&lines).map_err(|err| err.to_string())?;
        embedded += lines.len();
        for line in lines {
            kept.insert(line.summary, line.vector);
        }
    }
    let vectors = texts
        .iter()
        .map(|text| kept.get(&text_key(text)).cloned().unwrap_or_default())
        .collect::<Vec<_>>();
    let width = vectors.iter().map(Vec::len).max().unwrap_or(0);
    if vectors.iter().any(|vector| vector.len() != width) {
        return Err(format!("{id} gave vectors of different lengths"));
    }
    Ok((vectors, embedded))
}

/// Summaries' side of the tab: what it opens with, and what `d` and the command run,
/// with the summarizer and embedder chosen for this map.
pub(crate) struct Deep<'a> {
    pub store: &'a Store,
    pub summarizer: &'a dyn Summarizer,
    pub embed_with: EmbedWith<'a>,
}

impl Deep<'_> {
    pub async fn update(
        self,
        recorded: &Recorded,
        wanted: impl Fn(&DeepAction) -> bool,
        stop: &CancellationToken,
        progress: &mut (dyn FnMut(Progress) + Send),
        placed: &mut (dyn FnMut() + Send),
    ) -> io::Result<Updated> {
        update(
            recorded,
            wanted,
            self.store,
            self.summarizer,
            self.embed_with,
            stop,
            progress,
            placed,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::super::action::sample::shell;
    use super::super::embed::fakes::Letters;
    use super::super::summarize::fakes::{Echo, Refuses};
    use super::*;

    const QWEN: &str = "ollama/qwen3-coder:30b";

    fn recorded() -> Vec<DeepAction> {
        let mut all = Vec::new();
        for (run, model, cli) in [
            ("run-1", "haiku", "claude-code"),
            ("run-2", "gpt-6-luna", "codex"),
        ] {
            for at in 0..8 {
                let mut action = shell(run, &format!("c{at}"), &format!("{model} step {at}"));
                action.model = Some(model.to_owned());
                action.cli = cli.to_owned();
                all.push(action);
            }
        }
        all
    }

    async fn update_with(
        recorded: &[DeepAction],
        store: &Store,
        summarizer: &dyn Summarizer,
        embedder: Option<&dyn Embedder>,
    ) -> Updated {
        let deep = Deep {
            store,
            summarizer,
            embed_with: embedder.map_or(EmbedWith::Words, EmbedWith::Model),
        };
        deep.update(
            &Recorded::from(recorded.to_vec()),
            |_| true,
            &CancellationToken::new(),
            &mut |_| {},
            &mut || {},
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn the_fallback_word_vectors_place_every_summarized_action() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let echo = Echo::new(QWEN);
        let done = update_with(&recorded(), &store, &echo, None).await;
        assert_eq!((done.placed, done.new_places, done.embedded), (16, 16, 0));
        let layout = store.layout().unwrap();
        assert_eq!(layout.maker.embedder, WORD_VECTORS);
        assert!(
            store.vectors(WORD_VECTORS).is_empty(),
            "word vectors are made, not kept"
        );
    }

    #[tokio::test]
    async fn a_second_update_embeds_and_places_only_whats_new() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let letters = Letters::new();
        let mut all = recorded();
        let first = update_with(&all[..12], &store, &Echo::new(QWEN), Some(&letters)).await;
        assert_eq!((first.embedded, first.placed), (12, 12));
        let before = store.layout().unwrap();
        all.truncate(16);
        let echo = Echo::new(QWEN);
        let second = update_with(&all, &store, &echo, Some(&letters)).await;
        assert_eq!(second.summarized.already, 12);
        assert_eq!(
            (second.embedded, second.new_places, second.placed),
            (4, 4, 16)
        );
        assert_eq!(letters.texts.load(Ordering::SeqCst), 16);
        let after = store.layout().unwrap();
        for old in &before.points {
            let now = after
                .points
                .iter()
                .find(|p| p.action == old.action)
                .unwrap();
            assert_eq!((now.x, now.y), (old.x, old.y), "{:?} moved", old.action);
        }
    }

    #[tokio::test]
    async fn one_summarizer_per_map_so_another_asks_before_redoing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let all = recorded();
        let qwen = Echo::new(QWEN);
        assert_eq!(before(&store, QWEN), Before::Go);
        update_with(&all, &store, &qwen, None).await;
        assert_eq!(before(&store, QWEN), Before::Go);
        assert_eq!(
            before(&store, "claude-code/haiku"),
            Before::Redo {
                made_by: QWEN.to_owned(),
                actions: 16
            }
        );
        // Redone, the map is the new summarizer's alone, and the old summaries stay kept.
        let haiku = Echo::new("claude-code/haiku");
        let done = update_with(&all, &store, &haiku, None).await;
        assert_eq!((done.summarized.already, done.summarized.written), (0, 16));
        let map = store.layout().unwrap();
        assert_eq!(map.maker.summarizer, "claude-code/haiku");
        assert_eq!(map.points.len(), 16);
        assert_eq!(store.summaries(QWEN, PROMPT_VERSION).by_action.len(), 16);
    }

    #[tokio::test]
    async fn a_deleted_run_leaves_the_map() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let mut all = recorded();
        update_with(&all, &store, &Echo::new(QWEN), None).await;
        all.retain(|action| action.key.run == "run-2");
        assert_eq!(store.summary_lines().len(), 16);
        let refuses = Refuses(QWEN.parse().unwrap());
        let done = update_with(&all, &store, &refuses, None).await;
        assert_eq!(done.pruned, 8);
        assert_eq!(store.summary_lines().len(), 8);
        assert_eq!(store.layout().unwrap().points.len(), 8);
    }

    struct Broken;

    impl Embedder for Broken {
        fn id(&self) -> String {
            "ollama/nomic-embed-text".to_owned()
        }
        fn embed<'a>(
            &'a self,
            _texts: &'a [String],
        ) -> argonauts_core::base::future::BoxFuture<'a, Result<Vec<Vec<f32>>, String>> {
            Box::pin(async { Err("Ollama isn't running".to_owned()) })
        }
    }

    #[tokio::test]
    async fn a_new_maps_failing_embedder_places_it_by_word_vectors() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let done = update_with(&recorded(), &store, &Echo::new(QWEN), Some(&Broken)).await;
        assert_eq!(
            done.embedder_failed.as_deref(),
            Some("Ollama isn't running")
        );
        assert_eq!(store.layout().unwrap().maker.embedder, WORD_VECTORS);
        // Once placed by its words, the map keeps them, so no dot moves when a model answers.
        let letters = Letters::new();
        let more = update_with(&recorded(), &store, &Echo::new(QWEN), Some(&letters)).await;
        assert_eq!(more.new_places, 0);
        assert_eq!(store.layout().unwrap().maker.embedder, WORD_VECTORS);
    }

    #[tokio::test]
    async fn a_maps_own_embedder_failing_places_nothing_new_and_moves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let letters = Letters::new();
        let all = recorded();
        update_with(&all[..12], &store, &Echo::new(QWEN), Some(&letters)).await;
        let before = store.layout().unwrap();
        let done = update_with(&all, &store, &Echo::new(QWEN), Some(&Broken)).await;
        assert_eq!(done.summarized.written, 4, "the summaries are kept");
        assert_eq!((done.new_places, done.placed), (0, 12));
        assert_eq!(
            done.embedder_failed.as_deref(),
            Some("Ollama isn't running")
        );
        assert_eq!(store.layout().unwrap(), before, "the map is as it was");
        let missing = update(
            &Recorded::from(all.clone()),
            |_| true,
            &store,
            &Echo::new(QWEN),
            EmbedWith::Missing("no embedding model answers on this machine".to_owned()),
            &CancellationToken::new(),
            &mut |_| {},
            &mut || {},
        )
        .await
        .unwrap();
        assert_eq!(missing.new_places, 0);
        assert_eq!(store.layout().unwrap(), before);
        let back = update_with(&all, &store, &Echo::new(QWEN), Some(&letters)).await;
        assert_eq!(
            (back.new_places, back.placed),
            (4, 16),
            "once it answers again"
        );
    }

    #[tokio::test]
    async fn the_dots_are_placed_as_the_summaries_come() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let all: Vec<DeepAction> = (0..90)
            .map(|at| shell("run-1", &format!("c{at}"), &format!("step {at}")))
            .collect();
        let mut placements = 0;
        let mut sizes = Vec::new();
        let done = update(
            &Recorded::from(all.clone()),
            |_| true,
            &store,
            &Echo::new(QWEN),
            EmbedWith::Words,
            &CancellationToken::new(),
            &mut |_| {},
            &mut || {
                placements += 1;
                sizes.push(store.layout().map_or(0, |layout| layout.points.len()));
            },
        )
        .await
        .unwrap();
        assert_eq!(placements, 2, "after each share but the last");
        assert_eq!(sizes, [40, 80]);
        assert_eq!((done.placed, done.new_places), (90, 90));
    }

    fn failing(id: &str, from: usize) -> Echo {
        let mut echo = Echo::new(id);
        echo.fail_from = Some(from);
        echo
    }

    #[tokio::test]
    async fn a_redo_that_fails_at_once_leaves_the_map_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let all = recorded();
        update_with(&all, &store, &Echo::new(QWEN), None).await;
        let before = store.layout().unwrap();
        let haiku = failing("claude-code/haiku", 0);
        let done = update_with(&all, &store, &haiku, None).await;
        assert!(done.summarized.failed.is_some());
        assert_eq!((done.new_places, done.placed), (0, 16));
        assert_eq!(store.layout().unwrap(), before, "the map is as it was");
        assert_eq!(store.summaries(QWEN, PROMPT_VERSION).by_action.len(), 16);
    }

    #[tokio::test]
    async fn a_redo_that_fails_part_way_keeps_its_summaries_for_the_next_try() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let all: Vec<DeepAction> = (0..90)
            .map(|at| shell("run-1", &format!("c{at}"), &format!("step {at}")))
            .collect();
        update_with(&all, &store, &Echo::new(QWEN), None).await;
        let before = store.layout().unwrap();
        let mut placements = 0;
        let haiku = failing("claude-code/haiku", 5);
        let done = update(
            &Recorded::from(all.clone()),
            |_| true,
            &store,
            &haiku,
            EmbedWith::Words,
            &CancellationToken::new(),
            &mut |_| {},
            &mut || placements += 1,
        )
        .await
        .unwrap();
        assert_eq!(done.summarized.written, 50);
        assert_eq!((placements, done.new_places), (0, 0));
        assert_eq!(store.layout().unwrap(), before, "no share replaced it");
        let next = Echo::new("claude-code/haiku");
        let again = update_with(&all, &store, &next, None).await;
        assert_eq!(
            again.summarized.already, 50,
            "the next try starts where it stopped"
        );
        assert_eq!(next.requests(), 4);
        let map = store.layout().unwrap();
        assert_eq!(
            (map.maker.summarizer.as_str(), map.points.len()),
            ("claude-code/haiku", 90)
        );
    }

    #[tokio::test]
    async fn reading_no_runs_at_all_drops_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        update_with(&recorded(), &store, &Echo::new(QWEN), None).await;
        let before = store.layout().unwrap();
        let refuses = Refuses(QWEN.parse().unwrap());
        let done = update_with(&[], &store, &refuses, None).await;
        assert_eq!((done.pruned, done.placed), (0, 16));
        assert_eq!(store.layout().unwrap(), before);
        assert_eq!(store.summary_lines().len(), 16);
    }

    #[tokio::test]
    async fn a_first_map_that_fails_before_its_first_dots_saves_no_map() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let done = update_with(&recorded(), &store, &failing(QWEN, 0), None).await;
        assert!(done.summarized.failed.is_some());
        assert!(store.layout().is_none(), "an empty map is never saved");
    }

    #[tokio::test]
    async fn a_stopped_update_places_nothing_more() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let stop = CancellationToken::new();
        let all = recorded();
        let done = update(
            &Recorded::from(all.clone()),
            |_| true,
            &store,
            &Echo::new(QWEN),
            EmbedWith::Words,
            &stop,
            &mut |progress| {
                if progress.done >= 10 {
                    stop.cancel();
                }
            },
            &mut || {},
        )
        .await
        .unwrap();
        assert!(done.summarized.stopped);
        assert_eq!(done.summarized.written, 10, "what came back is kept");
        assert_eq!(done.new_places, 0);
        assert!(
            store.layout().is_none(),
            "nothing embedded or placed after the stop"
        );
    }
    /// A run of `project` with one call per command.
    fn run_on_disk(home: &std::path::Path, project: &str, run: &str, commands: &[&str]) {
        let dir = home.join("state").join(project).join("runs").join(run);
        std::fs::create_dir_all(&dir).unwrap();
        let lines: String = commands
            .iter()
            .enumerate()
            .map(|(at, command)| {
                let line = serde_json::json!({"seq": at, "ts": "2026-10-17T07:17:01Z",
                    "run_id": run, "source": "codex", "session": "s-001",
                    "kind": {"type": "agent", "data": {"type": "tool_started",
                        "id": format!("t{at}"), "name": "Bash",
                        "input": {"command": command}, "parent": null}}});
                format!("{line}\n")
            })
            .collect();
        std::fs::write(dir.join("events.jsonl"), lines).unwrap();
    }

    #[tokio::test]
    async fn a_project_that_cant_be_read_keeps_its_dots_and_summaries() {
        use argonauts_core::Paths;
        use std::os::unix::fs::PermissionsExt as _;
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        run_on_disk(
            home.path(),
            "alpha-3f9a1c2b4d5e",
            "run-1",
            &["ls", "pwd", "git log"],
        );
        run_on_disk(
            home.path(),
            "beta-4e8b2d3c5f6a",
            "run-2",
            &["pytest", "make", "cargo test"],
        );
        let store = Store::new(&paths);
        let all = super::super::records::recorded(&paths);
        let first = update(
            &all,
            |_| true,
            &store,
            &Echo::new(QWEN),
            EmbedWith::Words,
            &CancellationToken::new(),
            &mut |_| {},
            &mut || {},
        )
        .await
        .unwrap();
        assert_eq!(first.placed, 6);
        let before = store.layout().unwrap();
        run_on_disk(home.path(), "alpha-3f9a1c2b4d5e", "run-3", &["cargo fmt"]);
        let beta = home.path().join("state").join("beta-4e8b2d3c5f6a");
        std::fs::set_permissions(&beta, std::fs::Permissions::from_mode(0o000)).unwrap();
        let read = super::super::records::recorded(&paths);
        std::fs::set_permissions(&beta, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(read.actions.len(), 4, "beta's three are left out");
        let done = update(
            &read,
            |_| true,
            &store,
            &Echo::new(QWEN),
            EmbedWith::Words,
            &CancellationToken::new(),
            &mut |_| {},
            &mut || {},
        )
        .await
        .unwrap();
        assert_eq!(done.pruned, 0, "nothing of beta is dropped");
        assert_eq!(done.unread.len(), 1, "{:?}", done.unread);
        assert_eq!((done.new_places, done.placed), (1, 7));
        let after = store.layout().unwrap();
        for old in &before.points {
            let now = after.points.iter().find(|p| p.action == old.action);
            assert_eq!(
                now.map(|p| (p.x, p.y)),
                Some((old.x, old.y)),
                "{:?}",
                old.action
            );
        }
        assert_eq!(store.summary_lines().len(), 7);
        // Once beta's run is really gone, its dots and summaries go with it.
        std::fs::remove_dir_all(beta.join("runs").join("run-2")).unwrap();
        let read = super::super::records::recorded(&paths);
        let done = update(
            &read,
            |_| true,
            &store,
            &Refuses(QWEN.parse().unwrap()),
            EmbedWith::Words,
            &CancellationToken::new(),
            &mut |_| {},
            &mut || {},
        )
        .await
        .unwrap();
        assert_eq!((done.pruned, done.placed), (3, 4));
    }

    #[tokio::test]
    async fn a_redo_with_fewer_dots_than_the_map_keeps_it_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let all = recorded();
        update_with(&all, &store, &Echo::new(QWEN), None).await;
        let before = store.layout().unwrap();
        let haiku = Echo::new("claude-code/haiku");
        let done = update(
            &Recorded::from(all.clone()),
            |action| action.key.run == "run-1",
            &store,
            &haiku,
            EmbedWith::Words,
            &CancellationToken::new(),
            &mut |_| {},
            &mut || {},
        )
        .await
        .unwrap();
        assert_eq!(done.fewer, Some(FewerDots { made: 8, shown: 16 }));
        assert_eq!((done.new_places, done.placed), (0, 16));
        assert_eq!(store.layout().unwrap(), before, "the map is as it was");
        assert_eq!(
            store
                .summaries("claude-code/haiku", PROMPT_VERSION)
                .by_action
                .len(),
            8,
            "the new summaries are kept for the next try"
        );
    }
}
