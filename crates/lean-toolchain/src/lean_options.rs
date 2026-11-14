//! Lean command-line option values, shared without linking the Lean runtime.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The three value types supported by Lake's `LeanOptions` JSON format.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum LeanOptionValue {
    /// A boolean option.
    Bool(bool),
    /// A natural-number option fitting in the worker protocol's unsigned range.
    Nat(u64),
    /// A string option.
    String(String),
}

/// Deterministic JSON for Lean's own `LeanOptions` decoder.
#[must_use]
pub fn lean_options_json(options: &BTreeMap<String, LeanOptionValue>) -> String {
    let values = options
        .iter()
        .map(|(name, value)| {
            let value = match value {
                LeanOptionValue::Bool(value) => serde_json::Value::Bool(*value),
                LeanOptionValue::Nat(value) => serde_json::Value::Number((*value).into()),
                LeanOptionValue::String(value) => serde_json::Value::String(value.clone()),
            };
            (name.clone(), value)
        })
        .collect();
    serde_json::Value::Object(values).to_string()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn options_roundtrip_lean_json_types() {
        let options = [
            ("autoImplicit".to_owned(), LeanOptionValue::Bool(false)),
            ("maxSynthPendingDepth".to_owned(), LeanOptionValue::Nat(3)),
            ("traceFile".to_owned(), LeanOptionValue::String("a\\b\"c".to_owned())),
        ]
        .into();
        let encoded = lean_options_json(&options);
        assert_eq!(
            serde_json::from_str::<BTreeMap<String, LeanOptionValue>>(&encoded)
                .expect("valid option values round-trip"),
            options
        );
        for invalid in ["-1", "1.5", "null", "[]", "18446744073709551616"] {
            assert!(serde_json::from_str::<LeanOptionValue>(invalid).is_err());
        }
    }
}
