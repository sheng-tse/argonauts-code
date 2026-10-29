//! The model catalog's local models, ranked the same way on every computer: the largest window
//! from 64k, 32k or 16k whose memory fits two thirds of what models may use there, leaving the
//! rest for a reviewer, then the best coding score, then the newer, then the smaller. Ranked
//! against that ceiling rather than what is free now, so the order holds still while you look.

use std::cmp::{Ordering, Reverse};

use argonauts_core::model_picker::LocalModel;

/// The windows a model is ranked at, largest first.
pub const WINDOWS: [u64; 3] = [65_536, 32_768, 16_384];

const KIB: f64 = 1024.0;
const GIB: f64 = (1u64 << 30) as f64;

/// A local model as it would run on one computer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed<'a> {
    pub model: &'a LocalModel,
    pub window: u64,
    /// In bytes, at `window`.
    pub ram: u64,
}

/// What a pick may take where models may use `limit` bytes: two thirds, so a reviewer fits
/// beside it.
pub fn room(limit: u64) -> u64 {
    limit / 3 * 2
}

/// Bytes `model` takes at `window`: its weights and its context.
pub fn ram_at(model: &LocalModel, window: u64) -> u64 {
    let weights = model.weights_gb.0 * GIB;
    let context = model.context_kib_per_token.0 * KIB * window as f64;
    (weights + context).round() as u64
}

/// Bytes `model`'s download takes.
pub fn download(model: &LocalModel) -> u64 {
    (model.download_gb.0 * GIB).round() as u64
}

/// The window a model run at `window` ranks at: the largest of [`WINDOWS`] it reaches.
pub fn ranked_window(window: u64) -> Option<u64> {
    WINDOWS.into_iter().find(|&ranked| window >= ranked)
}

/// `model` on a computer where models may use `limit`: at the largest ranked window it takes
/// whose memory fits the room a pick gets, or `None` where none does.
pub fn placed(model: &LocalModel, limit: u64) -> Option<Placed<'_>> {
    WINDOWS
        .into_iter()
        .filter(|&window| window <= model.window)
        .map(|window| Placed {
            model,
            window,
            ram: ram_at(model, window),
        })
        .find(|placed| placed.ram <= room(limit))
}

/// The better of two models at the windows they'd run at comes first: the larger ranked
/// window, then the better coding score, then the newer, then the smaller. A model with no
/// score or month comes after one with one.
pub fn better(a: &Placed<'_>, b: &Placed<'_>) -> Ordering {
    let key = |placed: &Placed<'_>| {
        let model = placed.model;
        (
            Reverse(ranked_window(placed.window).unwrap_or(0)),
            Reverse(model.coding.as_ref().map(|coding| Number(coding.score.0))),
            Reverse(model.released.clone()),
            Number(model.weights_gb.0),
        )
    };
    key(a).cmp(&key(b))
}

/// The catalog's models that would run on a computer where models may use `limit`, best
/// first, each at its window. One another model in the catalog replaces is left out.
pub fn ranked(models: &[LocalModel], limit: u64) -> Vec<Placed<'_>> {
    let mut placed: Vec<Placed<'_>> = models
        .iter()
        .filter(|model| {
            model
                .replaced_by
                .as_ref()
                .is_none_or(|next| matching(models, next).is_none())
        })
        .filter_map(|model| placed(model, limit))
        .collect();
    placed.sort_by(better);
    placed
}

/// The catalog's model `tag` is made from: a tag counts as the model it's made from, so
/// `sample-coder:30b-64k` is `sample-coder:30b`. `None` for one not in the catalog.
pub fn matching<'a>(models: &'a [LocalModel], tag: &str) -> Option<&'a LocalModel> {
    let tag = tag.strip_suffix(":latest").unwrap_or(tag);
    models.iter().find(|model| {
        let base = model.tag.strip_suffix(":latest").unwrap_or(&model.tag);
        tag == base
            || tag
                .strip_prefix(base)
                .is_some_and(|rest| rest.starts_with('-'))
    })
}

/// A catalog figure, ordered as numbers are.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Number(f64);

impl Eq for Number {}

impl PartialOrd for Number {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Number {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::model_picker::{CodingScore, Figure};

    use super::*;

    const GIB: u64 = 1 << 30;

    /// A made-up model: `weights` GB, `context` KiB a token, up to `window`.
    fn model(tag: &str, weights: f64, context: f64, window: u64) -> LocalModel {
        LocalModel {
            tag: tag.to_owned(),
            name: tag.to_owned(),
            window,
            weights_gb: Figure(weights),
            download_gb: Figure(weights),
            context_kib_per_token: Figure(context),
            tools: true,
            thinking: false,
            released: None,
            coding: None,
            replaced_by: None,
        }
    }

    fn scored(mut model: LocalModel, score: f64, released: &str) -> LocalModel {
        model.coding = Some(CodingScore {
            score: Figure(score),
            source: "a sample leaderboard".to_owned(),
            checked: "2026-10-20".to_owned(),
        });
        model.released = Some(released.to_owned());
        model
    }

    fn tags(placed: &[Placed<'_>]) -> Vec<(String, u64)> {
        placed
            .iter()
            .map(|placed| (placed.model.tag.clone(), placed.window))
            .collect()
    }

    #[test]
    fn a_tag_counts_as_the_model_it_is_made_from() {
        let models = [
            model("sample-coder:30b", 19.0, 96.0, 65_536),
            model("sample:latest", 2.0, 64.0, 32_768),
        ];
        let tag = |tag| matching(&models, tag).map(|model| model.tag.as_str());
        assert_eq!(tag("sample-coder:30b-64k"), Some("sample-coder:30b"));
        assert_eq!(tag("sample-coder:30b"), Some("sample-coder:30b"));
        assert_eq!(tag("sample"), Some("sample:latest"));
        assert_eq!(tag("other:12b"), None);
    }

    #[test]
    fn a_model_runs_at_the_largest_window_that_fits_two_thirds() {
        let coder = model("sample-coder:30b", 19.0, 96.0, 65_536);
        // Models may use 52 GB, a pick 34: 64k at 25 GB fits.
        let roomy = placed(&coder, 52 * GIB).unwrap();
        assert_eq!((roomy.window, roomy.ram), (65_536, 25 * GIB));
        // Models may use 33 GB, a pick 22: 32k at 22 GB.
        assert_eq!(placed(&coder, 33 * GIB).unwrap().window, 32_768);
        // Models may use 24 GB, a pick 16: even 16k takes 20.5 GB.
        assert_eq!(placed(&coder, 24 * GIB), None);
        // A model that takes at most 40k ranks at 32k however roomy the computer.
        let small = model("sample:8b", 6.0, 32.0, 40_960);
        assert_eq!(placed(&small, 52 * GIB).unwrap().window, 32_768);
    }

    #[test]
    fn each_computer_ranks_by_window_then_score_then_newer_then_smaller() {
        let models = [
            scored(model("big:30b", 19.0, 96.0, 65_536), 51.0, "2025-07"),
            scored(model("mid:24b", 14.0, 64.0, 65_536), 46.0, "2025-05"),
            scored(model("newer-mid:24b", 14.0, 64.0, 65_536), 46.0, "2025-09"),
            scored(
                model("bigger-twin:24b", 15.0, 64.0, 65_536),
                46.0,
                "2025-09",
            ),
            model("unscored:20b", 13.0, 32.0, 65_536),
            scored(model("small:8b", 6.0, 32.0, 32_768), 60.0, "2026-01"),
        ];
        let at = |tag: &str, window: u64| (tag.to_owned(), window);
        assert_eq!(
            tags(&ranked(&models, 52 * GIB)),
            [
                at("big:30b", 65_536),
                at("newer-mid:24b", 65_536),
                at("bigger-twin:24b", 65_536),
                at("mid:24b", 65_536),
                at("unscored:20b", 65_536),
                at("small:8b", 32_768),
            ]
        );
        // Where models may use 24 GB a pick gets 16: the 30B fits at no window, and the
        // largest window comes before the best score.
        assert_eq!(
            tags(&ranked(&models, 24 * GIB)),
            [
                at("unscored:20b", 65_536),
                at("small:8b", 32_768),
                at("newer-mid:24b", 32_768),
                at("mid:24b", 32_768),
                at("bigger-twin:24b", 16_384),
            ]
        );
    }

    #[test]
    fn a_model_another_one_replaces_is_left_out() {
        let mut old = model("old:7b", 5.0, 32.0, 32_768);
        old.replaced_by = Some("new:8b".to_owned());
        let models = [old, model("new:8b", 6.0, 32.0, 32_768)];
        assert_eq!(
            tags(&ranked(&models, 52 * GIB)),
            [("new:8b".to_owned(), 32_768)]
        );
    }
}
