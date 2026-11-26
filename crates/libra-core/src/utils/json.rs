//! Canonical JSON ordering helpers.
//!
//! `serde_json::Map` is a sorted `BTreeMap` unless a dependency enables the
//! additive `preserve_order` feature. The desktop UI stack enables it, so
//! canonical bytes and public JSON output must sort object keys explicitly to
//! stay independent of whichever dependency set is linked.

use std::collections::BTreeMap;

use serde_json::Value;

/// Sort every object key recursively while preserving array order.
pub fn normalize_json_value(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(normalize_json_value).collect()),
        Value::Object(map) => {
            let normalized = map
                .into_iter()
                .map(|(key, value)| (key, normalize_json_value(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(normalized.into_iter().collect())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::normalize_json_value;

    #[test]
    fn normalizes_nested_object_keys() {
        let value = json!({
            "z": 1,
            "a": {
                "k2": 2,
                "k1": [{"b": 1, "a": 2}]
            }
        });
        let canonical = serde_json::to_string(&normalize_json_value(value)).unwrap();
        assert_eq!(canonical, r#"{"a":{"k1":[{"a":2,"b":1}],"k2":2},"z":1}"#);
    }
}
