//! What counts as tightening a safety key, fixed in code and never judged by a model. A
//! request may move a safety key only this way: toward asking you, a lower run limit (or a
//! first one), no network, Captain in a checked worktree, or checks where none are set.

use toml::Value;

/// Whether setting `key` to `new` where it is now `current` only tightens it. Any other
/// safety key never moves from a request, whatever the value.
pub fn tightens(key: &str, new: &Value, current: Option<&Value>) -> bool {
    let segments: Vec<&str> = key.split('.').collect();
    match segments.as_slice() {
        ["approvals", _, "mode"] => match (new.as_str(), current.and_then(Value::as_str)) {
            (Some(new), Some(current)) => match (autonomy(new), autonomy(current)) {
                (Some(new), Some(current)) => new <= current,
                _ => false,
            },
            (Some(new), None) => autonomy(new) == Some(0),
            _ => false,
        },
        // With no limit set, the first one only tightens. The caller refuses it when the run
        // is limited in another unit, which setting dollars would switch off.
        ["limits", "spend_per_run"] => match (number(new), current.map(number)) {
            (Some(new), Some(Some(current))) => new > 0.0 && new < current,
            (Some(new), None) => new > 0.0,
            _ => false,
        },
        ["sandbox", "network"] => new.as_bool() == Some(false),
        ["captain", "kind"] | ["captain_or_crew", "captain_kind"] => {
            new.as_str() == Some("checked")
        }
        // Checks where none are set only add something the work must pass. They run in each
        // task's sandbox, so they can do nothing the run's agents couldn't, and checks you set
        // are never replaced.
        ["checks", "run"] => {
            let none_set =
                current.is_none_or(|current| current.as_array().is_some_and(Vec::is_empty));
            none_set && new.as_array().is_some_and(|checks| !checks.is_empty())
        }
        _ => false,
    }
}

/// How much an approval mode decides without you, least first.
fn autonomy(mode: &str) -> Option<u8> {
    match mode {
        "supervised" => Some(0),
        "auto_preview" => Some(1),
        "auto" => Some(2),
        _ => None,
    }
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Float(number) => Some(*number),
        Value::Integer(number) => Some(*number as f64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Value {
        Value::String(value.to_owned())
    }

    #[test]
    fn only_the_six_tightenings_pass() {
        let supervised = text("supervised");
        let auto = text("auto");
        assert!(tightens("approvals.merge.mode", &supervised, Some(&auto)));
        assert!(tightens(
            "approvals.plan.mode",
            &text("auto_preview"),
            Some(&auto)
        ));
        assert!(!tightens("approvals.merge.mode", &auto, Some(&supervised)));
        assert!(tightens(
            "limits.spend_per_run",
            &Value::Float(1.0),
            Some(&Value::Float(20.0))
        ));
        assert!(tightens(
            "limits.spend_per_run",
            &Value::Integer(2),
            Some(&Value::Float(20.0))
        ));
        assert!(!tightens(
            "limits.spend_per_run",
            &Value::Float(30.0),
            Some(&Value::Float(20.0))
        ));
        assert!(!tightens(
            "limits.spend_per_run",
            &Value::Float(0.0),
            Some(&Value::Float(20.0))
        ));
        // No limit now: a first one only tightens.
        assert!(tightens("limits.spend_per_run", &Value::Float(1.0), None));
        assert!(!tightens("limits.spend_per_run", &Value::Float(0.0), None));
        assert!(!tightens(
            "limits.spend_per_run",
            &Value::Float(1.0),
            Some(&text("a lot"))
        ));
        assert!(tightens("sandbox.network", &Value::Boolean(false), None));
        assert!(!tightens("sandbox.network", &Value::Boolean(true), None));
        assert!(tightens(
            "captain.kind",
            &text("checked"),
            Some(&text("direct"))
        ));
        assert!(!tightens(
            "captain.kind",
            &text("direct"),
            Some(&text("checked"))
        ));
        assert!(tightens(
            "captain_or_crew.captain_kind",
            &text("checked"),
            None
        ));
        assert!(!tightens(
            "captain_or_crew.captain_kind",
            &text("as_captain"),
            None
        ));
        let tests = "run = [{ name = \"tests\", command = [\"python3\", \"-m\", \"unittest\"] }]"
            .parse::<toml::Table>()
            .unwrap()["run"]
            .clone();
        assert!(tightens("checks.run", &tests, None));
        assert!(tightens(
            "checks.run",
            &tests,
            Some(&Value::Array(Vec::new()))
        ));
        // Checks you set are never replaced, and an empty list sets nothing.
        assert!(!tightens("checks.run", &tests, Some(&tests)));
        assert!(!tightens("checks.run", &Value::Array(Vec::new()), None));
        for key in [
            "checks.run",
            "sandbox.mode",
            "merge.mode",
            "limits.plan_usage",
            "limits.x",
        ] {
            assert!(!tightens(key, &Value::Boolean(false), None), "{key}");
        }
    }
}
