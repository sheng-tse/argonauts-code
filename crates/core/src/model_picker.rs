//! `/model`'s models level, with the effort line under it, and `/effort`'s slider. In both, ←/→
//! step through the effort levels the model supports.

use serde::{Deserialize, Serialize};

use crate::app::ModelChoice;
use crate::names::{ModelRef, RoleName};

/// One CLI's or endpoint's models, as Claude Code's picker lists them: Default first, then the
/// models in the controller's order (newest first), with the aliases on one line under Default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelsLevel {
    /// Adapter or endpoint id.
    pub cli: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<ModelChoice>,
    pub models: Vec<ModelChoice>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<ModelChoice>,
}

impl ModelsLevel {
    /// The choices of `cli` from `ViewData::Models`, keeping their order.
    pub fn new(cli: &str, choices: &[ModelChoice]) -> Self {
        let mut level = Self {
            cli: cli.to_owned(),
            default: None,
            models: Vec::new(),
            aliases: Vec::new(),
        };
        for choice in choices.iter().filter(|choice| choice.worker.agent == cli) {
            if choice.worker.model.is_none() {
                level.default.get_or_insert_with(|| choice.clone());
            } else if choice.alias {
                level.aliases.push(choice.clone());
            } else {
                level.models.push(choice.clone());
            }
        }
        level
    }

    /// Default, then the models.
    pub fn rows(&self) -> impl Iterator<Item = &ModelChoice> {
        self.default.iter().chain(&self.models)
    }

    /// Where the pointer starts: on the current choice, or else the first row.
    pub fn current_row(&self) -> usize {
        self.rows().position(|choice| choice.current).unwrap_or(0)
    }
}

/// The line under the models list for the highlighted model, such as "xhigh effort". It shows
/// only the levels that model supports.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EffortLine {
    /// Lowest first. Empty when the model takes no effort setting, and then the line is hidden.
    pub levels: Vec<String>,
    /// Index into `levels`. `None` leaves the effort to the CLI.
    pub chosen: Option<usize>,
    pub default: Option<usize>,
}

impl EffortLine {
    /// Starts on `wanted` when the model supports it, else on the model's default. A model
    /// that takes one level is always at it.
    pub fn new(levels: &[String], default: Option<&str>, wanted: Option<&str>) -> Self {
        let at =
            |level: Option<&str>| level.and_then(|level| levels.iter().position(|l| l == level));
        let default = at(default);
        let only = (levels.len() == 1).then_some(0);
        Self {
            levels: levels.to_vec(),
            chosen: at(wanted).or(default).or(only),
            default,
        }
    }

    /// The model takes one level only, so there's nothing to adjust, as with agy's
    /// `gemini-3.8-flash-high`, whose level is part of the model.
    pub fn is_fixed(&self) -> bool {
        self.levels.len() == 1
    }

    /// For a highlighted choice, carrying over `wanted`, the level picked on the row before.
    pub fn for_choice(choice: &ModelChoice, wanted: Option<&str>) -> Self {
        Self::new(
            &choice.efforts,
            choice.default_effort.as_deref(),
            wanted.or(choice.effort.as_deref()),
        )
    }

    pub fn level(&self) -> Option<&str> {
        self.chosen
            .and_then(|at| self.levels.get(at))
            .map(String::as_str)
    }

    pub fn is_default(&self) -> bool {
        self.chosen.is_some() && self.chosen == self.default
    }

    /// ←. Returns whether the level changed.
    pub fn lower(&mut self) -> bool {
        self.step(false)
    }

    /// →. Returns whether the level changed.
    pub fn raise(&mut self) -> bool {
        self.step(true)
    }

    fn step(&mut self, up: bool) -> bool {
        let Some(last) = self.levels.len().checked_sub(1) else {
            return false;
        };
        // Unset starts from the default, or the middle when the model has none.
        let from = self.chosen.or(self.default).unwrap_or(last / 2);
        let to = if up {
            (from + 1).min(last)
        } else {
            from.saturating_sub(1)
        };
        let moved = self.chosen != Some(to);
        self.chosen = Some(to);
        moved
    }
}

/// `/effort`'s slider for the main agent or a role: the model's levels from "Faster" to
/// "Smarter", a marker over the chosen one, and a line on what it means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortSlider {
    /// The main agent when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<RoleName>,
    /// The model whose levels these are.
    pub worker: ModelRef,
    /// Its display name.
    pub model: String,
    /// Lowest first. Empty only on the main agent's view of a model that takes no effort
    /// setting, which opens for Ultracode alone.
    pub stops: Vec<EffortStop>,
    pub chosen: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<usize>,
    /// No level is set and the model names no default, so the CLI uses its own and the slider
    /// only starts in the middle. Moving it clears this.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cli_default: bool,
    /// The Ultracode toggle beside the slider, which Tab flips. Only the main agent's
    /// slider offers it, since it decides how every task runs, not how one role thinks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ultracode: Option<UltracodeToggle>,
}

/// `ultracode` as `/effort` shows it: on or off now, and as the slider opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UltracodeToggle {
    pub on: bool,
    pub was: bool,
}

impl UltracodeToggle {
    pub const NAME: &'static str = "Ultracode";
    /// What it does, shown while it's on.
    pub const MEANS: &'static str = "a workflow for every real request";

    pub fn new(on: bool) -> Self {
        Self { on, was: on }
    }

    pub fn changed(&self) -> bool {
        self.on != self.was
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortStop {
    pub level: String,
    /// One line.
    pub description: String,
}

impl EffortSlider {
    pub const FASTER: &'static str = "Faster";
    pub const SMARTER: &'static str = "Smarter";

    /// Starts on `current` when the model supports it, else on its default, else the middle.
    /// `None` when the model has no levels.
    pub fn new(
        role: Option<RoleName>,
        worker: ModelRef,
        model: impl Into<String>,
        stops: Vec<EffortStop>,
        default: Option<&str>,
        current: Option<&str>,
    ) -> Option<Self> {
        let at = |level: Option<&str>| {
            level.and_then(|level| stops.iter().position(|s| s.level == level))
        };
        let default = at(default);
        let set = at(current);
        let chosen = set.or(default).unwrap_or(stops.len().checked_sub(1)? / 2);
        // A model with one level runs at it, so the CLI has nothing to choose.
        let cli_default = set.is_none() && default.is_none() && stops.len() > 1;
        Some(Self {
            role,
            worker,
            model: model.into(),
            stops,
            chosen,
            default,
            cli_default,
            ultracode: None,
        })
    }

    /// The main agent's view when its model takes no effort setting, so Ultracode still has
    /// somewhere to be.
    pub fn without_levels(
        role: Option<RoleName>,
        worker: ModelRef,
        model: impl Into<String>,
    ) -> Self {
        Self {
            role,
            worker,
            model: model.into(),
            stops: Vec::new(),
            chosen: 0,
            default: None,
            cli_default: false,
            ultracode: None,
        }
    }

    /// Offers the Ultracode toggle, as it is now.
    pub fn with_ultracode(self, on: bool) -> Self {
        Self {
            ultracode: Some(UltracodeToggle::new(on)),
            ..self
        }
    }

    /// Tab, where the slider offers the toggle.
    pub fn toggle_ultracode(&mut self) {
        if let Some(toggle) = self.ultracode.as_mut() {
            toggle.on = !toggle.on;
        }
    }

    /// The model takes one level only, so the slider has nothing to move. See
    /// [`EffortLine::is_fixed`].
    pub fn is_fixed(&self) -> bool {
        self.stops.len() == 1
    }

    /// Levels to pick from, so ←/→ move the marker.
    pub fn moves(&self) -> bool {
        self.stops.len() > 1
    }

    /// `None` when the model takes no effort setting.
    pub fn level(&self) -> Option<&str> {
        self.stops.get(self.chosen).map(|stop| stop.level.as_str())
    }

    pub fn description(&self) -> Option<&str> {
        self.stops
            .get(self.chosen)
            .map(|stop| stop.description.as_str())
    }

    pub fn is_default(&self) -> bool {
        self.default == Some(self.chosen)
    }

    /// ←, toward Faster. Returns whether the level changed.
    pub fn lower(&mut self) -> bool {
        let before = self.chosen;
        self.chosen = self.chosen.saturating_sub(1);
        self.moved(before)
    }

    /// →, toward Smarter. Returns whether the level changed.
    pub fn raise(&mut self) -> bool {
        let before = self.chosen;
        self.chosen = (self.chosen + 1).min(self.stops.len().saturating_sub(1));
        self.moved(before)
    }

    fn moved(&mut self, before: usize) -> bool {
        let moved = self.chosen != before;
        self.cli_default &= !moved;
        moved
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::limits::CostEstimate;
    use crate::registry::ModelAccess;

    fn choice(worker: &str) -> ModelChoice {
        serde_json::from_value(json!({
            "worker": worker, "provider": "anthropic", "access": "subscription",
            "cost": {"unit": "unknown"}, "plan_usage": null, "efforts": [], "strengths": []
        }))
        .unwrap()
    }

    fn levels(names: &[&str]) -> Vec<String> {
        names.iter().map(|&name| name.to_owned()).collect()
    }

    fn opus() -> ModelChoice {
        ModelChoice {
            name: "Opus 5.5".into(),
            description: Some("For complex work and everyday tasks".into()),
            efforts: levels(&["low", "medium", "high", "xhigh", "max"]),
            default_effort: Some("xhigh".into()),
            current: true,
            ..choice("claude-code/claude-opus-5-5")
        }
    }

    #[test]
    fn a_level_puts_default_first_and_aliases_aside() {
        let default = ModelChoice {
            recommended: true,
            runs: Some("Opus 5.5".into()),
            description: Some("Best for everyday, complex tasks".into()),
            ..choice("claude-code")
        };
        let alias = ModelChoice {
            alias: true,
            runs: Some("Opus 5.5".into()),
            ..choice("claude-code/opus")
        };
        let haiku = choice("claude-code/claude-haiku-4-5");
        let other = choice("codex/gpt-6.1-sol");
        let choices = [opus(), alias.clone(), other, default.clone(), haiku.clone()];
        let level = ModelsLevel::new("claude-code", &choices);
        assert_eq!(level.default, Some(default));
        assert_eq!(level.models, [opus(), haiku]);
        assert_eq!(level.aliases, [alias]);
        assert_eq!(level.rows().count(), 3);
        assert_eq!(level.current_row(), 1);

        let value = serde_json::to_value(&level).unwrap();
        assert_eq!(serde_json::from_value::<ModelsLevel>(value).unwrap(), level);
        assert_eq!(opus().cost, CostEstimate::Unknown);
        assert_eq!(opus().access, ModelAccess::Subscription);
    }

    #[test]
    fn the_effort_line_shows_the_models_levels_and_its_default() {
        let mut line = EffortLine::for_choice(&opus(), None);
        assert_eq!(line.level(), Some("xhigh"));
        assert!(line.is_default());
        assert!(line.raise());
        assert_eq!(line.level(), Some("max"));
        assert!(!line.raise(), "max is the end");
        assert!(!line.is_default());

        let wanted = EffortLine::for_choice(&opus(), Some("low"));
        assert_eq!(wanted.level(), Some("low"));
        let unsupported = EffortLine::for_choice(&opus(), Some("ultra"));
        assert_eq!(
            unsupported.level(),
            Some("xhigh"),
            "falls back to the default"
        );

        let set = ModelChoice {
            effort: Some("high".into()),
            ..opus()
        };
        assert_eq!(EffortLine::for_choice(&set, None).level(), Some("high"));

        let mut bare = EffortLine::new(&levels(&["low", "medium", "high"]), None, None);
        assert_eq!(bare.level(), None, "the CLI decides until you choose");
        assert!(bare.lower());
        assert_eq!(bare.level(), Some("low"));

        let mut none = EffortLine::for_choice(&choice("claude-code/opus"), None);
        assert!(!none.raise());
        assert_eq!(none.level(), None);

        let value = serde_json::to_value(&line).unwrap();
        assert_eq!(
            value,
            json!({"levels": ["low", "medium", "high", "xhigh", "max"],
            "chosen": 4, "default": 3})
        );
        assert_eq!(serde_json::from_value::<EffortLine>(value).unwrap(), line);
    }

    #[test]
    fn the_slider_steps_between_faster_and_smarter() {
        let stops: Vec<EffortStop> = ["low", "high", "max"]
            .iter()
            .map(|&level| EffortStop {
                level: level.into(),
                description: format!("{level} effort"),
            })
            .collect();
        let worker: ModelRef = "claude-code/claude-opus-5-5".parse().unwrap();
        let mut slider = EffortSlider::new(
            Some(RoleName::Reviewer),
            worker.clone(),
            "Opus 5.5",
            stops.clone(),
            Some("high"),
            None,
        )
        .unwrap();
        assert_eq!(slider.level(), Some("high"));
        assert!(slider.is_default() && slider.moves());
        assert!(slider.raise());
        assert_eq!(slider.description(), Some("max effort"));
        assert!(!slider.raise());
        assert!(slider.lower() && slider.lower());
        assert!(!slider.lower());
        assert_eq!(slider.level(), Some("low"));

        let current = EffortSlider::new(
            None,
            worker.clone(),
            "Opus 5.5",
            stops.clone(),
            None,
            Some("max"),
        );
        assert_eq!(current.as_ref().map(|s| s.chosen), Some(2));
        assert!(!current.unwrap().cli_default);
        let mut middle =
            EffortSlider::new(None, worker.clone(), "Opus 5.5", stops, None, None).unwrap();
        assert_eq!(middle.chosen, 1);
        // Nothing set and no default: the CLI picks, until you move the slider.
        assert!(middle.cli_default);
        assert!(middle.lower());
        assert!(!middle.cli_default);
        assert!(EffortSlider::new(None, worker, "Opus 5.5", Vec::new(), None, None).is_none());

        let value = serde_json::to_value(&slider).unwrap();
        assert_eq!(value["role"], "reviewer");
        assert_eq!(value["chosen"], 0);
        assert_eq!(
            serde_json::from_value::<EffortSlider>(value).unwrap(),
            slider
        );
    }

    /// agy's `gemini-3.8-flash-high` takes high only: the line and the slider sit on it, with
    /// nothing set and no default, and the arrows don't move them.
    #[test]
    fn a_model_with_one_level_is_fixed_at_it() {
        let only = vec!["high".to_owned()];
        for wanted in [None, Some("low")] {
            let mut line = EffortLine::new(&only, None, wanted);
            assert!(line.is_fixed());
            assert_eq!(line.level(), Some("high"));
            assert!(!line.raise() && !line.lower());
        }
        assert!(!EffortLine::new(&["low".into(), "high".into()], None, None).is_fixed());

        let stops = vec![EffortStop {
            level: "high".into(),
            description: String::new(),
        }];
        let worker: ModelRef = "antigravity-cli/gemini-3.8-flash-high".parse().unwrap();
        let mut slider =
            EffortSlider::new(None, worker, "Gemini 3.8 Flash (High)", stops, None, None).unwrap();
        assert!(slider.is_fixed() && !slider.cli_default && !slider.moves());
        assert_eq!(slider.level(), Some("high"));
        assert!(!slider.raise() && !slider.lower());
    }

    /// Haiku takes no effort setting, so the main agent's view has no level, and the arrows
    /// have nothing to move.
    #[test]
    fn a_model_with_no_levels_has_a_view_without_them() {
        let worker: ModelRef = "claude-code/haiku".parse().unwrap();
        let mut view =
            EffortSlider::without_levels(None, worker, "Haiku 4.5").with_ultracode(false);
        assert_eq!((view.level(), view.description()), (None, None));
        assert!(!view.is_fixed() && !view.moves() && !view.is_default());
        assert!(!view.raise() && !view.lower());
        view.toggle_ultracode();
        assert!(
            view.ultracode
                .is_some_and(|toggle| toggle.on && toggle.changed())
        );
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(serde_json::from_value::<EffortSlider>(value).unwrap(), view);
    }
}
