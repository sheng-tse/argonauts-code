use serde_json::Value;

pub(crate) fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

pub(crate) fn string_at(value: &Value, key: &str) -> Option<String> {
    str_at(value, key).map(str::to_owned)
}

pub(crate) fn bool_at(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

pub(crate) fn u64_at(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

pub(crate) fn array_at<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

pub(crate) fn strings_at(value: &Value, key: &str) -> Vec<String> {
    array_at(value, key)
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accessors_tolerate_missing_and_mistyped_fields() {
        let v = json!({"s": "x", "n": 3, "b": true, "a": ["p", 1, "q"]});
        assert_eq!(str_at(&v, "s"), Some("x"));
        assert_eq!(str_at(&v, "n"), None);
        assert_eq!(string_at(&v, "missing"), None);
        assert!(bool_at(&v, "b"));
        assert!(!bool_at(&v, "s"));
        assert_eq!(u64_at(&v, "n"), 3);
        assert_eq!(u64_at(&v, "s"), 0);
        assert_eq!(array_at(&v, "a").len(), 3);
        assert!(array_at(&v, "s").is_empty());
        assert_eq!(strings_at(&v, "a"), ["p", "q"]);
    }
}
