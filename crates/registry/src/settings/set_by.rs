//! Which layer set each leaf key, and the file or flag it came from, so `/config` can show the
//! layer and an error can name the file.

use std::collections::BTreeMap;

use argonauts_core::settings::LAYERS;
use toml::Table;

use crate::resolve::leaves;

#[derive(Debug, Clone, Default)]
pub(crate) struct SetBy {
    keys: BTreeMap<String, Origin>,
}

#[derive(Debug, Clone)]
struct Origin {
    layer: &'static str,
    /// The file path, `--set key=value`, or the preset.
    source: String,
}

impl SetBy {
    /// Records every leaf of `table`. A leaf replaces what an earlier layer set at that key,
    /// below it, or above it, since a value and a table at one key replace each other.
    pub fn record(&mut self, table: &Table, layer: &'static str, source: &str) {
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
            .map_or_else(|| "built-in".to_owned(), |origin| origin.source.clone())
    }

    fn forget_around(&mut self, key: &str) {
        let under = format!("{key}.");
        self.keys
            .retain(|set, _| !set.starts_with(&under) && !key.starts_with(&format!("{set}.")));
    }
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
        set_by.record(&table("[budget.lines]\ncards = 0.5\n"), "project", "p.toml");
        set_by.record(&table("[budget.lines]\nreview = 0.2\n"), "global", "g.toml");
        assert_eq!(set_by.source("budget.lines"), "p.toml");
        assert_eq!(set_by.source("budget.lines.cards"), "p.toml");
        assert_eq!(set_by.source("critique.rounds"), "built-in");
        set_by.record(&table("gates = { run = [] }"), "flag", "--set gates.run=[]");
        assert_eq!(set_by.source("gates.run.0.cmd"), "--set gates.run=[]");
    }
}
