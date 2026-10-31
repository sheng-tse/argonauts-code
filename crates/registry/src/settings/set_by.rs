//! Which layer set each leaf key, and the file or flag it came from, so `/config` can show the
//! layer and an error can name the file.

use std::collections::BTreeMap;

use argonauts_core::settings::LAYERS;
use toml::{Table, Value};

use crate::settings::resolve::leaves;

#[derive(Debug, Clone, Default)]
pub(crate) struct SetBy {
    keys: BTreeMap<String, Origin>,
    /// Every table a layer wrote, an empty one too: it sets no key, but a value that's wrong
    /// there is still that file's.
    tables: BTreeMap<String, Origin>,
}

#[derive(Debug, Clone)]
struct Origin {
    layer: &'static str,
    /// The file path, `--set key=value`, or the workflow.
    source: String,
}

impl SetBy {
    /// Records every leaf of `table`. A leaf replaces what an earlier layer set at that key,
    /// below it, or above it, since a value and a table at one key replace each other.
    pub fn record(&mut self, table: &Table, layer: &'static str, source: &str) {
        let origin = Origin {
            layer,
            source: source.to_owned(),
        };
        for key in tables(table, "") {
            self.tables.insert(key, origin.clone());
        }
        for (key, _) in leaves(table) {
            self.forget_around(&key);
            let origin = Origin {
                layer,
                source: source.to_owned(),
            };
            self.keys.insert(key, origin);
        }
    }

    /// `to` took its value from `from`, so it has the same origin.
    pub fn copy(&mut self, from: &str, to: String) {
        if let Some(origin) = self.keys.get(from).cloned() {
            self.keys.insert(to, origin);
        }
    }

    pub fn remove(&mut self, key: &str) {
        self.keys.remove(key);
    }

    pub fn layer(&self, key: &str) -> Option<&'static str> {
        self.keys.get(key).map(|origin| origin.layer)
    }

    /// The layer that set `key`, else the latest that set a key under it or above it.
    pub fn layer_around(&self, key: &str) -> Option<&'static str> {
        if let Some(layer) = self.layer(key) {
            return Some(layer);
        }
        let under = format!("{key}.");
        let rank = |layer: &&'static str| LAYERS.iter().position(|each| each == layer);
        self.keys
            .iter()
            .filter(|(set, _)| set.starts_with(&under) || key.starts_with(&format!("{set}.")))
            .map(|(_, origin)| origin.layer)
            .max_by_key(rank)
    }

    /// Every key a layer set, with where it came from.
    pub fn keys(&self) -> impl Iterator<Item = (&str, &str)> {
        self.keys
            .iter()
            .map(|(key, origin)| (key.as_str(), origin.source.as_str()))
    }

    /// Where `key`'s value came from: the key itself, else the latest layer that set a key
    /// under it or above it, else `built-in`.
    pub fn source(&self, key: &str) -> String {
        if let Some(origin) = self.keys.get(key) {
            return origin.source.clone();
        }
        let under = format!("{key}.");
        let rank = |origin: &Origin| LAYERS.iter().position(|layer| *layer == origin.layer);
        self.keys
            .iter()
            .filter(|(set, _)| set.starts_with(&under) || key.starts_with(&format!("{set}.")))
            .map(|(_, origin)| origin)
            .max_by_key(|origin| rank(origin))
            .or_else(|| self.tables.get(key))
            .map_or_else(|| "built-in".to_owned(), |origin| origin.source.clone())
    }

    fn forget_around(&mut self, key: &str) {
        let under = format!("{key}.");
        self.keys
            .retain(|set, _| !set.starts_with(&under) && !key.starts_with(&format!("{set}.")));
    }
}

/// The dotted key of every table under `table`.
fn tables(table: &Table, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (key, value) in table {
        if let Value::Table(inner) = value {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            out.extend(tables(inner, &path));
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Table {
        text.parse().unwrap()
    }

    #[test]
    fn a_value_and_a_table_at_one_key_replace_each_other() {
        let mut set_by = SetBy::default();
        set_by.record(&table("native_fanout = { max = 5 }"), "global", "g.toml");
        assert_eq!(set_by.layer("native_fanout.max"), Some("global"));
        set_by.record(&table("native_fanout = \"on\""), "project", "p.toml");
        assert_eq!(set_by.layer("native_fanout.max"), None);
        assert_eq!(set_by.layer("native_fanout"), Some("project"));
        set_by.record(&table("native_fanout = { max = 2 }"), "flag", "--set");
        assert_eq!(set_by.layer("native_fanout"), None);
    }

    #[test]
    fn a_table_key_names_the_latest_layer_under_it() {
        let mut set_by = SetBy::default();
        set_by.record(&table("[limits.lines]\ntasks = 0.5\n"), "project", "p.toml");
        set_by.record(&table("[limits.lines]\nreview = 0.2\n"), "global", "g.toml");
        assert_eq!(set_by.source("limits.lines"), "p.toml");
        assert_eq!(set_by.source("limits.lines.tasks"), "p.toml");
        assert_eq!(set_by.source("critique.rounds"), "built-in");
        set_by.record(
            &table("checks = { run = [] }"),
            "flag",
            "--set checks.run=[]",
        );
        assert_eq!(set_by.source("checks.run.0.cmd"), "--set checks.run=[]");
        // A table written empty sets nothing, yet it's still the file's.
        set_by.record(&table("[steps.review]\n"), "project", "p.toml");
        assert_eq!(set_by.source("steps.review"), "p.toml");
    }
}
