//! Four made-up crews and their skies, counted from the very records their maps draw: R, a busy
//! crew over 30 days with models of six makers; S, a crew of five models that tests and works
//! nights; T, a crew that has been quiet for 9 days; and U, R's crew a little later. Each group
//! of a crew's actions is one model, and its numbers, its behaviors, its runs, its crashes, the
//! models it works with and the runs that mixed makers are counted from those actions and the
//! sessions they fell in, as the world counts them, so the sky and the map tell one story.

use std::collections::{BTreeMap, BTreeSet};

use crate::world::cosmos::{StarAction, Universe, rules};
use crate::world::map::fields::crews::detail::{Ended, Outcome};
use crate::world::map::fields::crews::{Crew as MapCrew, Key};
use crate::world::map::nature::crews as nature;
use argonauts_core::behaviors::behavior_world::{
    CliFailure, CrewFacts, MixedRun, ModelBehavior, ModelFacts, ModelPair, MonthActions,
    NewerModel, WorldFacts,
};

pub(crate) struct Crew {
    pub facts: WorldFacts,
    pub colours: Vec<[u8; 3]>,
    pub actions: Vec<StarAction>,
}

impl Crew {
    pub fn universe(&self) -> Universe {
        Universe::new(&self.facts, &self.colours, self.actions.clone())
    }

    /// The model with this id, to change a fact before the universe reads it.
    pub fn model(&mut self, id: &str) -> &mut ModelFacts {
        self.facts
            .models
            .iter_mut()
            .find(|m| m.model == id)
            .unwrap_or_else(|| panic!("no model {id}"))
    }
}

/// A behavior done this many times in a row in one session loops: a ring round its planet.
const LOOP_RUN: usize = 4;

/// An action whose words say it failed.
fn fails(behavior: &str) -> bool {
    ["which failed", "didn't apply", "didn't fit", "failed to"]
        .iter()
        .any(|words| behavior.contains(words))
}

/// The model a made-up group stands for, by its name.
fn model_of(group: &str) -> &'static str {
    match group {
        "Haiku 5.5" => "claude-haiku-5-5",
        "GPT 6 Luna" => "gpt-6-luna",
        "Opus 5.5" => "claude-opus-5-5",
        "Qwen3 Coder 30B" => "ollama/qwen3-coder:30b",
        "Codex" => "gpt-6-codex",
        "GPT 6 Astra" => "gpt-6-astra",
        "GPT 6.1 Sol" => "gpt-6.1-sol",
        "Gemini 3.8 Flash" => "gemini-3.8-flash",
        "Sonnet 5.5" => "claude-sonnet-5-5",
        "GLM 5" => "opencode/glm-5",
        "Kimi K2.6" => "opencode/kimi-k2.6",
        "Gemma 3 12B" => "ollama/gemma3:12b",
        _ => panic!("no made-up group {group}"),
    }
}

/// The CLI, the maker and the colour every tab draws each model in.
fn known(model: &str) -> (&'static str, &'static str, [u8; 3]) {
    match model {
        "claude-haiku-5-5" => ("claude-code", "anthropic", [224, 138, 102]),
        "claude-opus-5-5" => ("claude-code", "anthropic", [180, 71, 42]),
        "claude-sonnet-5-5" => ("claude-code", "anthropic", [243, 178, 122]),
        "claude-haiku-4-5" => ("claude-code", "anthropic", [180, 71, 42]),
        "gpt-6-luna" => ("codex", "openai", [106, 166, 255]),
        "gpt-6-codex" => ("codex", "openai", [46, 95, 192]),
        "gpt-6-astra" => ("codex", "openai", [168, 205, 255]),
        "gpt-6.1-sol" => ("codex", "openai", [63, 196, 240]),
        "gemini-3.8-flash" => ("antigravity-cli", "google", [54, 207, 230]),
        "ollama/gemma3:12b" => ("opencode", "google", [216, 184, 90]),
        "ollama/qwen3-coder:30b" => ("opencode", "alibaba", [255, 122, 198]),
        "opencode/glm-5" => ("opencode", "zhipu", [192, 154, 255]),
        "opencode/kimi-k2.6" => ("opencode", "moonshot", [154, 208, 138]),
        _ => panic!("no made-up model {model}"),
    }
}

pub(crate) fn r() -> Crew {
    counted(
        Key::R,
        &[
            ("2026-08", 934),
            ("2026-07", 724),
            ("2026-06", 601),
            ("2026-05", 962),
            ("2026-04", 959),
        ],
    )
}

pub(crate) fn s() -> Crew {
    counted(
        Key::S,
        &[
            ("2026-08", 941),
            ("2026-07", 960),
            ("2026-06", 782),
            ("2026-05", 1037),
        ],
    )
}

pub(crate) fn t() -> Crew {
    counted(
        Key::T,
        &[
            ("2026-08", 763),
            ("2026-07", 60),
            ("2026-06", 67),
            ("2026-05", 71),
            ("2026-04", 69),
            ("2026-03", 79),
            ("2026-02", 51),
            ("2026-01", 55),
            ("2025-12", 59),
            ("2025-11", 67),
            ("2025-10", 69),
            ("2025-09", 50),
        ],
    )
}

pub(crate) fn u() -> Crew {
    counted(
        Key::U,
        &[
            ("2026-08", 1017),
            ("2026-07", 705),
            ("2026-06", 586),
            ("2026-05", 937),
            ("2026-04", 935),
        ],
    )
}

/// A made-up crew's sky, counted from the actions its map draws and the sessions they fell in,
/// with the actions of the months before it.
fn counted(key: Key, months: &[(&str, u32)]) -> Crew {
    let crew = MapCrew::get(key);
    let detail = &crew.detail;
    let at = |action: usize| nature::action_at(crew, action);
    // The sky is seen a few minutes after the last day's last action, or at 19:30 where that
    // day was quiet.
    let last_day = nature::START + time::Duration::days(i64::from(detail.period) - 1);
    let end = (0..crew.len())
        .filter(|action| detail.day[*action] == 0)
        .map(at)
        .max()
        .map_or(last_day + time::Duration::minutes(19 * 60 + 30), |last| {
            last + time::Duration::minutes(3)
        });
    let age = |action: usize| (end - at(action)).as_seconds_f64();
    // The groups with actions, the busiest first, each one model.
    let mut groups: Vec<usize> = (0..crew.groups.len())
        .filter(|group| crew.g.contains(group))
        .collect();
    let count = |group: usize| crew.g.iter().filter(|g| **g == group).count();
    groups.sort_by_key(|group| std::cmp::Reverse(count(*group)));
    let model_at = |group: usize| groups.iter().position(|g| *g == group).unwrap_or(0);
    // A session's model where it holds several: the one with the most actions in it, the
    // first to act among equals.
    let busiest = |actions: &[usize]| -> usize {
        let mut tally: Vec<(usize, usize)> = Vec::new();
        for action in actions {
            let group = crew.g[*action];
            match tally.iter_mut().find(|(g, _)| *g == group) {
                Some((_, n)) => *n += 1,
                None => tally.push((group, 1)),
            }
        }
        let most = tally.iter().map(|(_, n)| *n).max().unwrap_or(0);
        tally
            .into_iter()
            .find(|(_, n)| *n == most)
            .map_or(0, |(group, _)| group)
    };
    // A behavior done 4 or more times in a row in one session loops, for the model that led
    // that stretch.
    let mut loops: BTreeMap<(usize, usize), u32> = BTreeMap::new();
    let mut crashes: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for session in &detail.sessions {
        let actions = &session.actions;
        let mut from = 0;
        while from < actions.len() {
            let label = crew.l[actions[from]];
            let mut to = from;
            while to + 1 < actions.len() && crew.l[actions[to + 1]] == label {
                to += 1;
            }
            if to + 1 - from >= LOOP_RUN {
                *loops
                    .entry((busiest(&actions[from..=to]), label))
                    .or_default() += 1;
            }
            from = to + 1;
        }
        if session.ended == Ended::Failed
            && let Some(last) = actions.last()
        {
            crashes.entry(busiest(actions)).or_default().push(*last);
        }
    }
    let runs_of = |group: usize| -> BTreeSet<usize> {
        (0..crew.len())
            .filter(|action| crew.g[*action] == group)
            .map(|action| detail.session[action])
            .collect()
    };
    let mut facts = WorldFacts {
        end,
        crew: CrewFacts {
            earlier_months: months
                .iter()
                .map(|(month, actions)| MonthActions {
                    month: (*month).to_owned(),
                    actions: *actions,
                })
                .collect(),
            ..CrewFacts::default()
        },
        ..WorldFacts::default()
    };
    let mut colours = Vec::new();
    for &group in &groups {
        let model = model_of(&crew.groups[group].name);
        let (cli, maker, rgb) = known(model);
        let mine: Vec<usize> = (0..crew.len())
            .filter(|action| crew.g[*action] == group)
            .collect();
        let within = |seconds: f64| mine.iter().filter(|a| age(**a) < seconds).count() as u32;
        let days: BTreeSet<u32> = mine.iter().map(|a| detail.day[*a]).collect();
        let outcomes = |wanted: Outcome| {
            mine.iter()
                .filter(|a| detail.outcome[**a] == wanted)
                .count() as u32
        };
        let mut behaviors: Vec<ModelBehavior> = Vec::new();
        for label in 0..crew.labels.len() {
            let done: Vec<usize> = mine
                .iter()
                .copied()
                .filter(|a| crew.l[*a] == label)
                .collect();
            if done.is_empty() {
                continue;
            }
            let recent: BTreeSet<u32> = done
                .iter()
                .map(|a| detail.day[*a])
                .filter(|day| *day < 7)
                .collect();
            behaviors.push(ModelBehavior {
                behavior: crew.labels[label].clone(),
                count: done.len() as u32,
                failed: done.iter().filter(|a| !detail.ok[**a]).count() as u32,
                last_used: done.iter().map(|a| detail.day[*a]).min().unwrap_or(0),
                days_in_last_7: recent.len() as u8,
                runs_in_a_row: loops.get(&(group, label)).copied().unwrap_or(0),
            });
        }
        behaviors.sort_by_key(|b| std::cmp::Reverse(b.count));
        facts.models.push(ModelFacts {
            model: model.to_owned(),
            agent: cli.to_owned(),
            maker: maker.to_owned(),
            actions: mine.len() as u32,
            failed: mine.iter().filter(|a| !detail.ok[**a]).count() as u32,
            last_7_days: mine.iter().filter(|a| detail.day[**a] < 7).count() as u32,
            last_24_hours: within(24.0 * 3600.0),
            last_hour: within(3600.0),
            last_10_minutes: within(rules::METEOR_MINUTES * 60.0),
            first_used: days.iter().copied().max().unwrap_or(0),
            last_used: days.iter().copied().min().unwrap_or(0),
            days_used: days.len() as u32,
            sessions: runs_of(group).len() as u32,
            merged: outcomes(Outcome::Merged),
            sent_back: outcomes(Outcome::SentBack),
            dropped: outcomes(Outcome::Dropped),
            last_action: mine.iter().map(|a| at(*a)).max(),
            cli_failures: crashes
                .get(&group)
                .into_iter()
                .flatten()
                .map(|last| CliFailure { at: at(*last) })
                .collect(),
            newer: None,
            behaviors,
        });
        colours.push(rgb);
    }
    // Two models work together where they share enough runs, more often than chance gives.
    let runs = detail.sessions.len() as f32;
    for (x, &a) in groups.iter().enumerate() {
        for &b in &groups[x + 1..] {
            let (runs_a, runs_b) = (runs_of(a), runs_of(b));
            let both = runs_a.intersection(&runs_b).count() as u32;
            let lift = both as f32 * runs / (runs_a.len() * runs_b.len()).max(1) as f32;
            if both >= rules::TOGETHER_RUNS && lift >= rules::TOGETHER_LIFT {
                facts.pairs.push(ModelPair {
                    models: [
                        facts.models[model_at(a)].model.clone(),
                        facts.models[model_at(b)].model.clone(),
                    ],
                    runs: both,
                    lift,
                });
            }
        }
    }
    facts.pairs.sort_by(|p, q| q.lift.total_cmp(&p.lift));
    // The runs of the last day whose models came from more than one maker, the newest first.
    for (index, session) in detail.sessions.iter().enumerate() {
        let Some(&first) = session.actions.first() else {
            continue;
        };
        if age(first) > 24.0 * 3600.0 || age(first) < 0.0 {
            continue;
        }
        let mut makers: Vec<String> = Vec::new();
        for action in &session.actions {
            let maker = &facts.models[model_at(crew.g[*action])].maker;
            if !makers.contains(maker) {
                makers.push(maker.clone());
            }
        }
        if makers.len() < 2 {
            continue;
        }
        let models = makers
            .iter()
            .map(|maker| {
                let of_maker: Vec<usize> = session
                    .actions
                    .iter()
                    .copied()
                    .filter(|a| facts.models[model_at(crew.g[*a])].maker == *maker)
                    .collect();
                facts.models[model_at(busiest(&of_maker))].model.clone()
            })
            .collect();
        facts.mixed_runs.push(MixedRun {
            run: format!("run-{index:03}"),
            at: at(first),
            actions: session.actions.len() as u32,
            makers,
            models,
        });
    }
    facts
        .mixed_runs
        .sort_by_key(|run| std::cmp::Reverse(run.at));
    let actions = (0..crew.len())
        .map(|action| StarAction {
            index: action as u32,
            model: model_at(crew.g[action]),
            behavior: crew.labels[crew.l[action]].clone(),
            day: detail.day[action],
            age: age(action).max(0.0),
            failed: !detail.ok[action] || fails(&crew.labels[crew.l[action]]),
        })
        .collect();
    Crew {
        facts,
        colours,
        actions,
    }
}

/// A white dwarf's crew: R with an older Haiku whose newer one did more this week.
pub(crate) fn r_with_older_haiku() -> Crew {
    let mut crew = r();
    let mut older = crew.facts.models[0].clone();
    older.model = "claude-haiku-4-5".to_owned();
    older.actions = 40;
    older.last_7_days = 2;
    older.last_24_hours = 0;
    older.last_hour = 0;
    older.last_10_minutes = 0;
    older.cli_failures.clear();
    older.behaviors.truncate(4);
    for behavior in &mut older.behaviors {
        behavior.count = 10;
        behavior.failed = behavior.failed.min(10);
    }
    older.newer = Some(NewerModel {
        model: "claude-haiku-5-5".to_owned(),
        came: 20,
    });
    crew.facts.models.push(older);
    crew.colours.push(known("claude-haiku-4-5").2);
    crew
}
