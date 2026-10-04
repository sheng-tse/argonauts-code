//! What counts as tightening a safety key, fixed in code and never judged by a model. A
//! request may move a safety key only this way: toward asking you, a lower run budget, no
//! network, or solo in a checked copy.

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
        ["budget", "run_usd"] => match (number(new), current.and_then(number)) {
            (Some(new), Some(current)) => new > 0.0 && new < current,
            _ => false,
        },
        ["sandbox", "network"] => new.as_bool() == Some(false),
        ["solo", "style"] | ["triage", "solo_style"] => new.as_str() == Some("checked"),
        _ => false,
    }
}

/// How much an approval mode decides without you, least first.
fn autonomy(mode: &str) -> Option<u8> {
    match mode {
        "supervised" => Some(0),
        "shadow" => Some(1),
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
    fn only_the_five_tightenings_pass() {
        let supervised = text("supervised");
        let auto = text("auto");
        assert!(tightens("approvals.merge.mode", &supervised, Some(&auto)));
        assert!(tightens(
            "approvals.plan.mode",
            &text("shadow"),
            Some(&auto)
        ));
        assert!(!tightens("approvals.merge.mode", &auto, Some(&supervised)));
        assert!(tightens(
            "budget.run_usd",
            &Value::Float(1.0),
            Some(&Value::Float(20.0))
        ));
        assert!(tightens(
            "budget.run_usd",
            &Value::Integer(2),
            Some(&Value::Float(20.0))
        ));
        assert!(!tightens(
            "budget.run_usd",
            &Value::Float(30.0),
            Some(&Value::Float(20.0))
        ));
        assert!(!tightens(
            "budget.run_usd",
            &Value::Float(0.0),
            Some(&Value::Float(20.0))
        ));
        // No dollar limit now: setting one could switch the budget's unit, so it's refused.
        assert!(!tightens("budget.run_usd", &Value::Float(1.0), None));
        assert!(tightens("sandbox.network", &Value::Boolean(false), None));
        assert!(!tightens("sandbox.network", &Value::Boolean(true), None));
        assert!(tightens(
            "solo.style",
            &text("checked"),
            Some(&text("direct"))
        ));
        assert!(!tightens(
            "solo.style",
            &text("direct"),
            Some(&text("checked"))
        ));
        assert!(tightens("triage.solo_style", &text("checked"), None));
        assert!(!tightens("triage.solo_style", &text("as_solo"), None));
        for key in [
            "gates.run",
            "sandbox.mode",
            "merge.mode",
            "budget.enabled",
            "limits.x",
        ] {
            assert!(!tightens(key, &Value::Boolean(false), None), "{key}");
        }
    }
}
