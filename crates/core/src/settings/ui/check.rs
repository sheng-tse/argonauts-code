//! Checks the interface's settings as they load. A bad value is named and its default used, so
//! a typo never blanks a mod or stops the app from starting.

use std::collections::BTreeSet;

use toml::{Table, Value};

use super::UiSettings;
use super::look::Threshold;
use super::mods::{ModName, Place, SharedKey};

/// The keys under `[ui]` where a value that doesn't read falls back instead of failing.
const FORGIVING: &[&str] = &[
    "theme",
    "colors",
    "format",
    "levels",
    "states",
    "above_input",
    "views",
    "mods",
    "panel.context",
];

/// A value under `[ui]` that didn't read, now back at its default.
#[derive(Debug, Clone, PartialEq)]
pub struct BadValue {
    /// Dotted from the top, such as `ui.format.tokens`.
    pub key: String,
    pub written: Value,
    /// Why it didn't read, in serde's words.
    pub problem: String,
}

/// A value that read but can't work where it is, now left out or back at its default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiProblem {
    pub key: String,
    pub message: String,
}

impl UiProblem {
    fn new(key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            message: message.into(),
        }
    }
}

/// In the merged settings, puts each forgiving value under `[ui]` that won't read back to its
/// value in `defaults`, or drops it when there it has none. Returns what it put back.
pub fn drop_bad_values(settings: &mut Table, defaults: &Table) -> Vec<BadValue> {
    let Some(Value::Table(ui)) = settings.get_mut("ui") else {
        return Vec::new();
    };
    let default_ui = defaults.get("ui").and_then(Value::as_table);
    let mut bad = Vec::new();
    for (path, written) in leaves(ui) {
        let dotted = path.join(".");
        let forgiving = FORGIVING
            .iter()
            .any(|key| dotted == *key || dotted.starts_with(&format!("{key}.")));
        let default = default_ui.and_then(|table| value_at(table, &path));
        // Every default reads, so only what a layer changed needs trying.
        if !forgiving || default == Some(&written) {
            continue;
        }
        let Err(err) = Value::Table(alone(&path, written.clone())).try_into::<UiSettings>() else {
            continue;
        };
        put(ui, &path, default.cloned());
        bad.push(BadValue {
            key: format!("ui.{dotted}"),
            written,
            problem: err.message().to_owned(),
        });
    }
    bad
}

/// Each leaf with its path; a list is one leaf.
fn leaves(table: &Table) -> Vec<(Vec<String>, Value)> {
    let mut found = Vec::new();
    for (key, value) in table {
        match value {
            Value::Table(inner) => {
                for (mut path, leaf) in leaves(inner) {
                    path.insert(0, key.clone());
                    found.push((path, leaf));
                }
            }
            leaf => found.push((vec![key.clone()], leaf.clone())),
        }
    }
    found
}

fn alone(path: &[String], value: Value) -> Table {
    let mut table = Table::new();
    match path {
        [] => {}
        [key] => {
            table.insert(key.clone(), value);
        }
        [key, rest @ ..] => {
            table.insert(key.clone(), Value::Table(alone(rest, value)));
        }
    }
    table
}

fn value_at<'a>(table: &'a Table, path: &[String]) -> Option<&'a Value> {
    let (last, parents) = path.split_last()?;
    let mut current = table;
    for key in parents {
        current = current.get(key)?.as_table()?;
    }
    current.get(last)
}

fn put(table: &mut Table, path: &[String], value: Option<Value>) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut current = table;
    for key in parents {
        match current.get_mut(key) {
            Some(Value::Table(inner)) => current = inner,
            _ => return,
        }
    }
    match value {
        Some(value) => current.insert(last.clone(), value),
        None => current.remove(last),
    };
}

impl UiSettings {
    /// Leaves out what read but can't work: a mod in a place it can't draw, a form or
    /// units a mod doesn't have, a duration that isn't one. Returns what it changed.
    pub fn tidy(&mut self) -> Vec<UiProblem> {
        let mut problems = self.tidy_places();
        problems.extend(self.tidy_mods());
        problems.extend(self.tidy_spans());
        if self.levels.warn.value() > self.levels.danger.value() {
            problems.push(UiProblem::new(
                "ui.levels.warn",
                format!(
                    "ui.levels.warn ({}) is above ui.levels.danger ({}), so nothing turns amber \
                     before it turns red",
                    self.levels.warn, self.levels.danger
                ),
            ));
        }
        problems
    }

    fn tidy_places(&mut self) -> Vec<UiProblem> {
        let mut problems = Vec::new();
        for place in Place::ALL {
            let mut seen = BTreeSet::new();
            self.place_mut(place).retain(|&the_mod| {
                if !the_mod.fits(place) {
                    problems.push(UiProblem::new(
                        place.key(),
                        format!(
                            "{}: {the_mod} can't show {}, so it's left out",
                            place.key(),
                            place.phrase()
                        ),
                    ));
                    return false;
                }
                if !seen.insert(the_mod) {
                    problems.push(UiProblem::new(
                        place.key(),
                        format!(
                            "{}: {the_mod} is listed twice; the first stays",
                            place.key()
                        ),
                    ));
                    return false;
                }
                true
            });
        }
        problems
    }

    fn tidy_mods(&mut self) -> Vec<UiProblem> {
        let mut problems = Vec::new();
        for name in ModName::ALL {
            let mut clear = Vec::new();
            let shared = self.mods.shared(name);
            let key = |field: &str| format!("ui.mods.{name}.{field}");
            if let Some(form) = shared.form.filter(|form| !name.forms().contains(form)) {
                let forms: Vec<&str> = name.forms().iter().map(|form| form.word()).collect();
                let draws = if forms.is_empty() {
                    "only a chip on the bottom line".to_owned()
                } else {
                    forms.join(", ")
                };
                problems.push(UiProblem::new(
                    key("form"),
                    format!(
                        "{}: {name} doesn't draw {}; it draws {draws}",
                        key("form"),
                        form.word()
                    ),
                ));
                clear.push(SharedKey::Form);
            }
            if let Some(units) = shared.units.filter(|units| !name.units().contains(units)) {
                let takes: Vec<&str> = name.units().iter().map(|units| units.word()).collect();
                let takes = if takes.is_empty() {
                    "it has no choice of units".to_owned()
                } else {
                    format!("it takes {}", takes.join(" or "))
                };
                problems.push(UiProblem::new(
                    key("units"),
                    format!(
                        "{}: {name} can't show {} first; {takes}",
                        key("units"),
                        units.word()
                    ),
                ));
                clear.push(SharedKey::Units);
            }
            for (field, threshold, shared_key) in [
                ("warn", shared.warn, SharedKey::Warn),
                ("danger", shared.danger, SharedKey::Danger),
            ] {
                if threshold == Some(Threshold::SoftStop) && !name.counts_limit() {
                    problems.push(UiProblem::new(
                        key(field),
                        format!(
                            "{}: soft_stop is limits.soft_stop, which only the run-limits and tasks \
                             mods measure against",
                            key(field)
                        ),
                    ));
                    clear.push(shared_key);
                }
            }
            if let Some(parts) = shared.parts.filter(|_| !name.parts().is_empty()) {
                let (known, unknown): (Vec<String>, Vec<String>) = parts
                    .iter()
                    .cloned()
                    .partition(|part| name.parts().contains(&part.as_str()));
                if !unknown.is_empty() {
                    problems.push(UiProblem::new(
                        key("parts"),
                        format!(
                            "{}: {name} has no part named {}; it has {}",
                            key("parts"),
                            unknown.join(", "),
                            name.parts().join(", ")
                        ),
                    ));
                    clear.push(SharedKey::Parts(known));
                }
            }
            for shared_key in clear {
                self.mods.clear(name, shared_key);
            }
        }
        problems
    }

    fn tidy_spans(&mut self) -> Vec<UiProblem> {
        let defaults = UiSettings::default();
        let mut problems = Vec::new();
        let spans = [
            (
                "ui.above_input.refresh",
                &mut self.above_input.refresh,
                defaults.above_input.refresh,
            ),
            (
                "ui.above_input.settle",
                &mut self.above_input.settle,
                defaults.above_input.settle,
            ),
            (
                "ui.mods.plan_usage.stale_after",
                &mut self.mods.plan_usage.own.stale_after,
                defaults.mods.plan_usage.own.stale_after,
            ),
        ];
        for (key, span, default) in spans {
            if let Err(problem) = span.parse() {
                problems.push(UiProblem::new(
                    key,
                    format!("{key}: {problem}; using {}", default.0),
                ));
                *span = default;
            }
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::super::mods::ModShape;
    use super::*;
    use crate::settings::Span;

    fn defaults() -> Table {
        let mut table = Table::new();
        table.insert(
            "ui".into(),
            Value::try_from(UiSettings::default()).expect("defaults serialize"),
        );
        table
    }

    fn merged(text: &str) -> Table {
        let mut merged = defaults();
        let layer: Table = text.parse().expect("test TOML");
        let ui = merged.get_mut("ui").and_then(Value::as_table_mut).unwrap();
        for (key, value) in leaves(layer["ui"].as_table().unwrap()) {
            let mut current = &mut *ui;
            for part in &key[..key.len() - 1] {
                current = current
                    .entry(part.clone())
                    .or_insert_with(|| Value::Table(Table::new()))
                    .as_table_mut()
                    .unwrap();
            }
            current.insert(key[key.len() - 1].clone(), value);
        }
        merged
    }

    fn read(table: &Table) -> UiSettings {
        table["ui"]
            .clone()
            .try_into()
            .expect("reads after the check")
    }

    #[test]
    fn a_bad_value_is_named_and_its_default_used() {
        let mut table = merged(
            r##"
            [ui]
            theme = "solarized"
            [ui.format]
            tokens = "tiny"
            clock = "24h"
            [ui.levels]
            warn = "0.8"
            [ui.above_input]
            mods = ["context", "plan_usagea"]
            max_rows = -3
            [ui.mods.context]
            form = "fulll"
            warn = "85%"
            [ui.mods.context.colors]
            messages = "#d97757"
            skills = "pinkish!"
            [ui.states.done]
            glyph = "+"
            color = 12
            [ui.panel]
            max_rows = 3
            context = "loud"
            "##,
        );
        let bad = drop_bad_values(&mut table, &defaults());
        let keys: Vec<&str> = bad.iter().map(|bad| bad.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "ui.above_input.max_rows",
                "ui.above_input.mods",
                "ui.format.tokens",
                "ui.levels.warn",
                "ui.mods.context.colors.skills",
                "ui.mods.context.form",
                "ui.panel.context",
                "ui.states.done.color",
                "ui.theme",
            ]
        );
        assert!(bad[2].problem.contains("tiny"), "{:?}", bad[2]);

        let ui = read(&table);
        let default = UiSettings::default();
        assert_eq!(ui.theme, default.theme);
        assert_eq!(ui.format.tokens, default.format.tokens);
        assert_eq!(
            ui.format.clock,
            super::super::look::ClockStyle::TwentyFourHour
        );
        assert_eq!(ui.levels.warn, default.levels.warn);
        assert_eq!(ui.above_input.mods, default.above_input.mods);
        assert_eq!(ui.above_input.max_rows, 14);
        assert_eq!(ui.mods.context.form, None);
        assert_eq!(ui.mods.context.warn, Some(Threshold::percent(85.0)));
        assert_eq!(ui.mods.context.colors.len(), 1);
        assert_eq!(ui.states.done.glyph.as_deref(), Some("+"));
        assert_eq!(ui.states.done.color, default.states.done.color);
        assert_eq!(ui.panel.max_rows, 3);
        assert_eq!(ui.panel.context, default.panel.context);
    }

    #[test]
    fn values_outside_the_forgiving_tables_still_fail_as_before() {
        let mut table = merged(
            r#"
            [ui]
            verbosity = "loud"
            "#,
        );
        assert!(drop_bad_values(&mut table, &defaults()).is_empty());
        assert!(table["ui"].clone().try_into::<UiSettings>().is_err());
    }

    #[test]
    fn mods_leave_places_they_cant_draw_in() {
        let mut ui = UiSettings::default();
        ui.above_input.mods = vec![
            ModName::Context,
            ModName::Agent,
            ModName::Context,
            ModName::PlanUsage,
        ];
        ui.views.status.mods.push(ModName::Limits);
        let problems = ui.tidy();
        assert_eq!(ui.above_input.mods, [ModName::Context, ModName::PlanUsage]);
        assert!(!ui.views.status.mods.contains(&ModName::Limits));
        let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "ui.above_input.mods: agent can't show above the input, so it's left out",
                "ui.above_input.mods: context is listed twice; the first stays",
                "ui.views.status.mods: limits can't show on the Status tab, so it's left out",
            ]
        );
    }

    #[test]
    fn a_mod_keeps_only_the_forms_units_parts_and_levels_it_has() {
        let mut ui = UiSettings::default();
        ui.mods.sessions.form = Some(ModShape::Columns);
        ui.mods.plan_usage.units = Some(super::super::mods::Units::Usd);
        ui.mods.context.parts = Some(vec!["bar".into(), "sparkle".into()]);
        ui.mods.run.warn = Some(Threshold::SoftStop);
        ui.mods.tasks.warn = Some(Threshold::SoftStop);
        ui.above_input.settle = Span::new("soon");
        let problems = ui.tidy();
        let keys: Vec<&str> = problems.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "ui.mods.context.parts",
                "ui.mods.sessions.form",
                "ui.mods.plan-usage.units",
                "ui.mods.run.warn",
                "ui.above_input.settle",
            ]
        );
        assert_eq!(ui.mods.sessions.form, None);
        assert_eq!(ui.mods.plan_usage.units, None);
        assert_eq!(ui.mods.context.parts, Some(vec!["bar".to_owned()]));
        assert_eq!(ui.mods.run.warn, None);
        assert_eq!(ui.mods.tasks.warn, Some(Threshold::SoftStop));
        assert_eq!(ui.above_input.settle, Span::new("150ms"));
        assert!(UiSettings::default().tidy().is_empty());
    }
}
