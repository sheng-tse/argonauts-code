//! Canonical JSON and SHA-256 helpers used for config hashes and repo ids.

use std::fmt::Write as _;

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in &digest {
        // Writing to a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Compact JSON with object keys sorted at every level, so equal values always hash the same.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (i, (key, item)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(item, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn canonical_json_sorts_keys_recursively() {
        let a = json!({"b": 1, "a": {"z": [3, {"y": 1, "x": 2}], "c": null}});
        assert_eq!(
            canonical_json(&a),
            r#"{"a":{"c":null,"z":[3,{"x":2,"y":1}]},"b":1}"#
        );
    }

    #[test]
    fn canonical_json_escapes_keys_and_strings() {
        let v = json!({"q\"k": "line\nbreak"});
        assert_eq!(canonical_json(&v), r#"{"q\"k":"line\nbreak"}"#);
    }

    #[test]
    fn canonical_json_is_order_independent() {
        let a: Value = serde_json::from_str(r#"{"x":1,"y":2.5}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"y":2.5,"x":1}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }
}
