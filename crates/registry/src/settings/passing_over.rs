//! A key a settings file sets that can't be used never stops the app: the key is left out,
//! its default is used, and the warnings name it, which is how Doctor and the start notices
//! tell you. A flag you typed or a request still stops with the error, since you're there to
//! fix it.

use argonauts_core::settings::Settings;
use toml::{Table, Value};

use crate::SettingsError;
use crate::resolve::{GLOBAL, PRESET, PROJECT, PROJECT_LOCAL, is_plain_key, remove_leaf, value_at};

/// Why one pass over the layers stopped, and the key it stopped at when that's known.
#[derive(Debug)]
pub(crate) struct Stop {
    key: Option<String>,
    /// A settings or workflow file set the key, rather than a flag, a request or the built-in
    /// defaults.
    in_a_file: bool,
    pub error: SettingsError,
}

impl Stop {
    pub fn at(key: &str, in_a_file: bool, error: SettingsError) -> Self {
        Self {
            key: Some(key.to_owned()),
            in_a_file,
            error,
        }
    }
}

impl From<SettingsError> for Stop {
    fn from(error: SettingsError) -> Self {
        Self {
            key: None,
            in_a_file: false,
            error,
        }
    }
}

/// The keys left out of the files so far, each with what was wrong with it.
#[derive(Debug, Default)]
pub(crate) struct PassedOver {
    keys: Vec<(String, SettingsError)>,
}

impl PassedOver {
    /// Leaves `stop`'s key out from now on, when a file set it and it isn't out already.
    /// False when the stop is final.
    pub fn add(&mut self, stop: &Stop) -> bool {
        let Some(key) = &stop.key else {
            return false;
        };
        if !stop.in_a_file || self.keys.iter().any(|(out, _)| out == key) {
            return false;
        }
        self.keys.push((key.clone(), stop.error.clone()));
        true
    }

    pub fn leave_out(&self, table: &mut Table) {
        for (key, _) in &self.keys {
            remove_leaf(table, key);
        }
    }

    pub fn warnings(&self) -> Vec<String> {
        self.keys.iter().map(|(_, error)| warning(error)).collect()
    }
}

fn warning(error: &SettingsError) -> String {
    match error {
        SettingsError::Invalid { layer, message } => {
            format!("{message} (in {layer}), so the default is used")
        }
        other => format!("{other}, so the default is used"),
    }
}

pub(crate) fn from_a_file(layer: &str) -> bool {
    matches!(layer, GLOBAL | PROJECT | PROJECT_LOCAL | PRESET)
}

/// The key under `key` that serde refused, when `key` holds a table such as `[roles]`: each
/// entry is read alone, and the first that fails alone is the one. When they all fail, the
/// fault is the table's own.
pub(crate) fn narrowed(mut key: String, merged: &Table) -> String {
    loop {
        let Some(Value::Table(entries)) = value_at(merged, &key) else {
            return key;
        };
        let failing: Vec<&String> = entries
            .iter()
            .filter(|(name, value)| !reads_alone(&key, name, value))
            .map(|(name, _)| name)
            .collect();
        match failing.first() {
            Some(name) if failing.len() < entries.len() => key = format!("{key}.{name}"),
            _ => return key,
        }
    }
}

fn reads_alone(key: &str, name: &str, value: &Value) -> bool {
    let mut table = Table::new();
    table.insert(name.to_owned(), value.clone());
    for segment in key.rsplit('.') {
        let mut outer = Table::new();
        outer.insert(segment.to_owned(), Value::Table(table));
        table = outer;
    }
    Value::Table(table).try_into::<Settings>().is_ok()
}

/// Leaves out each key whose name isn't plain, with a warning, so one odd name in a file
/// doesn't stop the rest of it.
pub(crate) fn drop_odd_keys(table: &mut Table, source: &str) -> Vec<String> {
    fn walk(prefix: &str, table: &mut Table, source: &str, warnings: &mut Vec<String>) {
        let odd: Vec<String> = table
            .keys()
            .filter(|name| !is_plain_key(name))
            .cloned()
            .collect();
        for name in odd {
            table.remove(&name);
            let under = if prefix.is_empty() {
                String::new()
            } else {
                format!(" under {prefix}")
            };
            warnings.push(format!(
                "{name:?}{under} in {source} isn't a plain key name, so it is ignored; use \
                 letters, digits, _ and -"
            ));
        }
        for (name, value) in table.iter_mut() {
            if let Value::Table(inner) = value {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                walk(&path, inner, source, warnings);
            }
        }
    }
    let mut warnings = Vec::new();
    walk("", table, source, &mut warnings);
    warnings
}
