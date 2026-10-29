//! Which model each of the map's actions belongs to. The map names an action's CLI and model as
//! the screen shows them, the world's facts by their ids, so the two meet through the names the
//! app keeps.

use std::collections::HashMap;

use argonauts_core::behavior_map::BehaviorMap;
use argonauts_core::behavior_world::{ModelFacts, WorldFacts};
use argonauts_core::display_names::DisplayNames;

use super::model::StarAction;
use crate::format::WallClock;

/// The map's actions that belong to one of the world's models, placed in days and seconds back
/// from the end of the period. An action of a model the facts don't know is left out.
pub fn star_actions(
    map: &BehaviorMap,
    facts: &WorldFacts,
    names: &DisplayNames,
    clock: &WallClock,
) -> Vec<StarAction> {
    let mut by_name: HashMap<(String, String), usize> = HashMap::new();
    for (at, model) in facts.models.iter().enumerate() {
        by_name.entry(shown_as(model, names)).or_insert(at);
    }
    let end_day = clock.to_local(facts.end).date();
    map.actions
        .iter()
        .enumerate()
        .filter_map(|(index, action)| {
            let model = *by_name.get(&(action.cli.clone(), action.model.clone()))?;
            let day = (end_day - clock.to_local(action.at).date()).whole_days();
            Some(StarAction {
                index: u32::try_from(index).ok()?,
                model,
                behavior: action.behavior.clone(),
                day: u32::try_from(day).unwrap_or(0),
                age: (facts.end - action.at).as_seconds_f64(),
                failed: action.failed,
            })
        })
        .collect()
}

/// The CLI and the model as the map writes them on an action of this model.
pub(crate) fn shown_as(model: &ModelFacts, names: &DisplayNames) -> (String, String) {
    let cli = names.cli(&model.agent);
    // A model the facts know only by its CLI stands for the CLI's own default.
    let id = (model.model != model.agent).then_some(model.model.as_str());
    let name = names.model(&model.agent, id).unwrap_or_else(|| cli.clone());
    (cli, name)
}
