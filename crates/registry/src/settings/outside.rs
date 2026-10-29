//! A project file may keep agents closer to the project than your own settings do, never
//! farther: in `[outside]` its mode lists only lose modes, and its never-list only gains
//! entries, trusted or not, so none of its own takes a name back out with `!`.

use toml::{Table, Value};

/// The `[outside]` lists whose modes a project file may only take away.
const MODE_LISTS: [&str; 3] = ["read", "write", "ask"];

/// Narrows `layer`'s `[outside]` to what `below` allows, and says what it left out, one line
/// each, naming `source`.
pub(crate) fn narrow(layer: &mut Table, below: &Table, source: &str) -> Vec<String> {
    let Some(Value::Table(outside)) = layer.get_mut("outside") else {
        return Vec::new();
    };
    let lower = |key: &str| -> Vec<Value> {
        match below.get("outside").and_then(|outside| outside.get(key)) {
            Some(Value::Array(entries)) => entries.clone(),
            _ => Vec::new(),
        }
    };
    let mut said = Vec::new();
    for key in MODE_LISTS {
        let Some(Value::Array(modes)) = outside.get_mut(key) else {
            continue;
        };
        let allowed = lower(key);
        let (kept, wider): (Vec<Value>, Vec<Value>) =
            modes.drain(..).partition(|mode| allowed.contains(mode));
        *modes = kept;
        if !wider.is_empty() {
            said.push(format!(
                "outside.{key} in {source} adds {}, which your own settings leave out. A project \
                 file can only take modes away there, so it is ignored",
                listed(&wider)
            ));
        }
    }
    if let Some(Value::Array(never)) = outside.get_mut("never") {
        let mut kept = lower("never");
        let dropped: Vec<Value> = kept
            .iter()
            .filter(|entry| !never.contains(entry))
            .cloned()
            .collect();
        let (added, opened): (Vec<Value>, Vec<Value>) = never
            .drain(..)
            .filter(|entry| !kept.contains(entry))
            .partition(|entry| !takes_back(entry));
        kept.extend(added);
        *never = kept;
        if !opened.is_empty() {
            said.push(format!(
                "outside.never in {source} takes {} back out of the never-list. A project file \
                 can only add to it, so it is ignored",
                listed(&opened)
            ));
        }
        if !dropped.is_empty() {
            said.push(format!(
                "outside.never in {source} leaves out {} of your own never-list. A project file \
                 can only add to it, so they still hold",
                listed(&dropped)
            ));
        }
    }
    said
}

/// An entry such as `!.env.example`, which opens a name the list closes.
fn takes_back(entry: &Value) -> bool {
    entry
        .as_str()
        .is_some_and(|text| text.trim_start().starts_with('!'))
}

/// `a`, `a and b`, or `a and N more`.
fn listed(values: &[Value]) -> String {
    let words: Vec<String> = values
        .iter()
        .map(|value| match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .collect();
    match words.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [one, two] => format!("{one} and {two}"),
        [first, rest @ ..] => format!("{first} and {} more", rest.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Table {
        text.parse().unwrap()
    }

    fn yours() -> Table {
        table(
            "[outside]\nread = [\"auto\", \"autopilot\"]\nwrite = [\"autopilot\"]\n\
             ask = [\"supervised\"]\nnever = [\"~/.ssh\", \".env\"]",
        )
    }

    #[test]
    fn a_project_file_takes_modes_away_and_adds_to_the_never_list() {
        let mut project = table(
            "[outside]\nread = [\"autopilot\"]\nwrite = []\nask = []\n\
             never = [\"~/.ssh\", \".env\", \"secrets\"]",
        );
        let said = narrow(&mut project, &yours(), "app/.argonauts/config.toml");
        assert!(said.is_empty(), "{said:?}");
        let outside = project["outside"].as_table().unwrap();
        assert_eq!(outside["read"], Value::Array(vec!["autopilot".into()]));
        assert_eq!(outside["write"], Value::Array(Vec::new()));
        assert_eq!(outside["never"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn a_project_file_never_widens_the_modes_or_empties_the_never_list() {
        let mut project = table(
            "[outside]\nwrite = [\"autopilot\", \"supervised\", \"auto_preview\"]\n\
             ask = [\"supervised\", \"auto\"]\nnever = [\"keys\"]",
        );
        let said = narrow(&mut project, &yours(), "app/.argonauts/config.toml");
        let outside = project["outside"].as_table().unwrap();
        assert_eq!(outside["write"], Value::Array(vec!["autopilot".into()]));
        assert_eq!(outside["ask"], Value::Array(vec!["supervised".into()]));
        assert_eq!(
            outside["never"],
            Value::Array(vec!["~/.ssh".into(), ".env".into(), "keys".into()])
        );
        assert_eq!(
            said,
            [
                "outside.write in app/.argonauts/config.toml adds supervised and auto_preview, \
                 which your own settings leave out. A project file can only take modes away \
                 there, so it is ignored",
                "outside.ask in app/.argonauts/config.toml adds auto, which your own settings \
                 leave out. A project file can only take modes away there, so it is ignored",
                "outside.never in app/.argonauts/config.toml leaves out ~/.ssh and .env of your \
                 own never-list. A project file can only add to it, so they still hold",
            ]
        );
    }

    /// `!` opens a name the never-list closes, which only your own settings may do.
    #[test]
    fn a_project_file_never_takes_a_name_back_out_of_the_never_list() {
        let mut project =
            table("[outside]\nnever = [\"~/.ssh\", \".env\", \"!.env.local\", \"keys\"]");
        let said = narrow(&mut project, &yours(), "app/.argonauts/config.toml");
        let outside = project["outside"].as_table().unwrap();
        assert_eq!(
            outside["never"],
            Value::Array(vec!["~/.ssh".into(), ".env".into(), "keys".into()])
        );
        assert_eq!(
            said,
            [
                "outside.never in app/.argonauts/config.toml takes !.env.local back out of the \
              never-list. A project file can only add to it, so it is ignored"
            ]
        );
        let mut yours_with_templates = yours();
        yours_with_templates["outside"]["never"]
            .as_array_mut()
            .unwrap()
            .push("!.env.example".into());
        let mut same = table("[outside]\nnever = [\"~/.ssh\", \".env\", \"!.env.example\"]");
        assert!(
            narrow(
                &mut same,
                &yours_with_templates,
                "app/.argonauts/config.toml"
            )
            .is_empty()
        );
    }

    #[test]
    fn a_file_without_outside_is_left_as_it_is() {
        let mut project = table("[captain]\nasks = \"every_change\"");
        let before = project.clone();
        assert!(narrow(&mut project, &yours(), "x").is_empty());
        assert_eq!(project, before);
    }
}
