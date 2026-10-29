//! Maps made by hand for the tests: groups, actions at chosen places, and each group's list,
//! as the app would send them.

use argonauts_core::behavior_map::{
    BehaviorMap, BehaviorRow, Comparison, GRID, MapAction, MapGroup, MapRequest,
};
use time::macros::datetime;

pub(crate) struct Sample {
    pub map: BehaviorMap,
}

/// Haiku reads and edits; Luna reads git history; Gemini has too few actions to compare.
pub(crate) fn three_models() -> Sample {
    Sample::new(MapRequest::default())
        .model("Haiku 5.5", "claude-code", "claude-haiku-5-5")
        .model("GPT 6 Luna", "codex", "gpt-6-luna")
        .model("Gemini 3.8 Flash", "antigravity-cli", "gemini-3.8-flash")
        .actions(0, "read a source file", 12, (0.2, 0.7))
        .actions(0, "edited a source file", 6, (0.4, 0.2))
        .actions(1, "read git history", 5, (0.6, 0.8))
        .actions(1, "read a source file", 2, (0.2, 0.7))
        .actions(2, "read a source file", 3, (0.2, 0.7))
        .rows(
            0,
            &[
                ("edited a source file", 6, 0),
                ("read a source file", 12, 5),
            ],
        )
        .rows(1, &[("read git history", 5, 0)])
        .small(2, "3 actions are too few to draw where it acts: it takes 5")
}

pub(super) fn action(group: usize, behavior: &str, (x, y): (f32, f32)) -> MapAction {
    MapAction {
        x,
        y,
        group,
        behavior: behavior.to_owned(),
        label: behavior.to_owned(),
        summary: None,
        agent: "build:per-api-key".to_owned(),
        model: String::new(),
        cli: String::new(),
        role: "builder".to_owned(),
        workflow: "spec-first".to_owned(),
        project: "council".to_owned(),
        run: Some("run-20261020-123036-4e50".to_owned()),
        at: datetime!(2026-10-20 12:32 UTC),
        nth: 3,
        of: 6,
        tool: "command_execution".to_owned(),
        input: "git status".to_owned(),
        place: Some("in the run's worktree".to_owned()),
        run_shows: None,
        failed: false,
        outcome: None,
    }
}

impl Sample {
    pub fn new(request: MapRequest) -> Self {
        Self {
            map: BehaviorMap {
                request,
                runs: 19,
                projects: ["council", "due", "web", "cli", "notes", "docs"]
                    .map(str::to_owned)
                    .to_vec(),
                ..BehaviorMap::default()
            },
        }
    }

    /// A model of `agent`'s, by its name and id.
    pub fn model(mut self, name: &str, agent: &str, model: &str) -> Self {
        self.map.groups.push(MapGroup {
            name: name.to_owned(),
            agent: Some(agent.to_owned()),
            model: Some(model.to_owned()),
            actions: 0,
        });
        self.map.comparisons.push(Comparison::default());
        self
    }

    /// `count` actions of `group` under `behavior`, spread a little around `place`.
    pub fn actions(
        mut self,
        group: usize,
        behavior: &str,
        count: usize,
        place: (f32, f32),
    ) -> Self {
        let name = self.map.groups[group].name.clone();
        let agent = self.map.groups[group].agent.clone().unwrap_or_default();
        for n in 0..count {
            let turn = n as f32 * 2.399;
            let spread = 0.004 * (n as f32).sqrt();
            let mut action = action(
                group,
                behavior,
                (place.0 + spread * turn.cos(), place.1 + spread * turn.sin()),
            );
            action.model = name.clone();
            action.cli = match agent.as_str() {
                "claude-code" => "Claude Code",
                "codex" => "Codex",
                _ => "agy",
            }
            .to_owned();
            self.map.actions.push(action);
        }
        self.map.groups[group].actions += count;
        self
    }

    /// One action of `group` under `behavior`, exactly at `place`.
    pub fn at(self, group: usize, behavior: &str, place: (f32, f32)) -> Self {
        self.actions(group, behavior, 1, place)
    }

    /// `count` more actions of `group`, each the same as its first.
    pub fn again(mut self, group: usize, count: usize) -> Self {
        let Some(first) = self.map.actions.iter().find(|a| a.group == group).cloned() else {
            return self;
        };
        self.map.actions.extend(std::iter::repeat_n(first, count));
        self.map.groups[group].actions += count;
        self
    }

    /// Each group's list, from its counts against the rest, with the rows given.
    pub fn rows(mut self, group: usize, rows: &[(&str, usize, usize)]) -> Self {
        let mine = self.map.groups[group].actions;
        let total: usize = self.map.groups.iter().map(|g| g.actions).sum();
        let lit = |behavior: &str| -> Vec<usize> {
            self.map
                .actions
                .iter()
                .enumerate()
                .filter(|(_, a)| a.behavior == behavior)
                .map(|(at, _)| at)
                .collect()
        };
        let rows = rows
            .iter()
            .map(|(behavior, mine, others)| BehaviorRow {
                behavior: (*behavior).to_owned(),
                label: None,
                mine: *mine,
                others: *others,
                lit: lit(behavior),
                new: false,
            })
            .collect();
        self.map.comparisons[group] = Comparison {
            overlay: Some(vec![0.0; GRID * GRID]),
            why_not: None,
            rows,
            mine,
            others: total - mine,
        };
        self
    }

    pub fn small(mut self, group: usize, why: &str) -> Self {
        let comparison = &mut self.map.comparisons[group];
        comparison.overlay = None;
        comparison.why_not = Some(why.to_owned());
        self
    }
}

/// One model's facts: its maker, its actions, and its behaviors with their counts.
pub(crate) fn model_facts(
    model: &str,
    agent: &str,
    maker: &str,
    behaviors: &[(&str, u32)],
) -> argonauts_core::behavior_world::ModelFacts {
    use argonauts_core::behavior_world::{ModelBehavior, ModelFacts};
    ModelFacts {
        model: model.to_owned(),
        agent: agent.to_owned(),
        maker: maker.to_owned(),
        actions: behaviors.iter().map(|(_, count)| count).sum(),
        behaviors: behaviors
            .iter()
            .map(|(behavior, count)| ModelBehavior {
                behavior: (*behavior).to_owned(),
                count: *count,
                ..ModelBehavior::default()
            })
            .collect(),
        ..ModelFacts::default()
    }
}

/// Three makers' models: Anthropic's two, the busiest, OpenAI's one and Google's one; Haiku 5.5
/// has more behaviors than a star has planets, so it has a belt.
pub(crate) fn three_makers() -> argonauts_core::behavior_world::WorldFacts {
    argonauts_core::behavior_world::WorldFacts {
        models: vec![
            model_facts(
                "gpt-6-luna",
                "codex",
                "openai",
                &[("read git history", 5), ("read a source file", 2)],
            ),
            model_facts(
                "claude-haiku-5-5",
                "claude-code",
                "anthropic",
                &[
                    ("read a source file", 12),
                    ("edited a source file", 6),
                    ("ran the test suite", 4),
                    ("searched code by pattern", 3),
                    ("wrote a doc", 2),
                    ("changed git state", 2),
                    ("listed a folder", 1),
                ],
            ),
            model_facts(
                "gemini-3.8-flash",
                "antigravity-cli",
                "google",
                &[("read a source file", 3)],
            ),
            model_facts(
                "claude-haiku-4-5",
                "claude-code",
                "anthropic",
                &[("read a source file", 2)],
            ),
        ],
        ..Default::default()
    }
}

impl Sample {
    /// The map with the world's facts beside it.
    pub fn world(mut self, world: argonauts_core::behavior_world::WorldFacts) -> Self {
        self.map.world = Some(Box::new(world));
        self
    }
}
