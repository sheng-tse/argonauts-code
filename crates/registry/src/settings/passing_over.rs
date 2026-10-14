//! A key a settings file sets that can't be used never stops the app: the key is left out,
//! its default is used, and the warnings name it, which is how Doctor and the start notices
//! tell you. A flag you typed or a request still stops with the error, since you're there to
//! fix it.

use argonauts_core::settings::Settings;
use toml::{Table, Value};

use crate::SettingsError;
use crate::resolve::{
    GLOBAL, PROJECT, PROJECT_LOCAL, WORKFLOW, is_plain_key, remove_leaf, value_at,
};

/// Why one pass over the layers stopped, and the key it stopped at when that's known.
#[derive(Debug)]
pub(crate) struct Stop {
    key: Option<String>,
    /// A settings or workflow file set the key, rather than a flag, a request or the built-in
    /// defaults.
    in_a_file: bool,
    /// Boxed, so a stop stays small enough to return.
    pub error: Box<SettingsError>,
    /// What else is wrong under the key, which leaving it out takes along.
    also: Vec<SettingsError>,
    /// What the notice says when the key is left out, where the error alone would say less.
    notice: Option<String>,
}

impl Stop {
    pub fn at(key: &str, in_a_file: bool, error: SettingsError) -> Self {
        Self {
            key: Some(key.to_owned()),
            in_a_file,
            error: Box::new(error),
            also: Vec::new(),
            notice: None,
        }
    }

    pub fn noticed(mut self, notice: String) -> Self {
        self.notice = Some(notice);
        self
    }

    /// Names every other fault under the key too, so none of them goes without a notice.
    pub fn with(mut self, also: impl IntoIterator<Item = SettingsError>) -> Self {
        for error in also {
            if error != *self.error && !self.also.contains(&error) {
                self.also.push(error);
            }
        }
        self
    }
}

impl From<SettingsError> for Stop {
    fn from(error: SettingsError) -> Self {
        Self {
            key: None,
            in_a_file: false,
            error: Box::new(error),
            also: Vec::new(),
            notice: None,
        }
    }
}

/// The keys left out of the files so far, each with the notice that says what was wrong.
#[derive(Debug, Default)]
pub(crate) struct PassedOver {
    keys: Vec<(String, String)>,
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
        let first = stop.notice.clone().unwrap_or_else(|| warning(&stop.error));
        let notices = std::iter::once(first).chain(stop.also.iter().map(warning));
        self.keys
            .extend(notices.map(|notice| (key.clone(), notice)));
        true
    }

    pub fn leave_out(&self, table: &mut Table) {
        for (key, _) in &self.keys {
            remove_leaf(table, key);
        }
    }

    pub fn warnings(&self) -> Vec<String> {
        self.keys.iter().map(|(_, notice)| notice.clone()).collect()
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
    matches!(layer, GLOBAL | PROJECT | PROJECT_LOCAL | WORKFLOW)
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
    Value::Table(alone(key, name, value))
        .try_into::<Settings>()
        .is_ok()
}

/// Settings that hold only `name` under `key`.
fn alone(key: &str, name: &str, value: &Value) -> Table {
    let mut table = Table::new();
    table.insert(name.to_owned(), value.clone());
    for segment in key.rsplit('.') {
        let mut outer = Table::new();
        outer.insert(segment.to_owned(), Value::Table(table));
        table = outer;
    }
    table
}

/// The entries of the table at `key` that can't be read even alone: each with the settings it
/// was read from and what serde said. Leaving the table out takes them all, and serde named
/// only the first.
pub(crate) fn refused_alone(key: &str, merged: &Table) -> Vec<(Table, toml::de::Error)> {
    let Some(Value::Table(entries)) = value_at(merged, key) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|(name, value)| {
            let alone = alone(key, name, value);
            let refused = Value::Table(alone.clone()).try_into::<Settings>().err()?;
            Some((alone, refused))
        })
        .collect()
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
