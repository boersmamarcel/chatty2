//! Custom serde deserializers for the `env` field in MCP tool schemas.
//!
//! OpenAI's strict-mode function calling rejects `"additionalProperties": { "type": "string" }`
//! on object-typed properties. To stay compatible with all providers, the JSON schema declares
//! `env` as `"type": "string"` (a JSON-encoded object). These deserializers accept **both**
//! a JSON string *and* a raw JSON object so that Anthropic/Gemini (which send objects) and
//! OpenAI (which sends strings) all work.

use serde::Deserialize;
use serde::de;
use std::collections::HashMap;

/// Deserialize `env` from either a JSON object or a JSON-encoded string.
///
/// * JSON object `{"K": "V"}` -> `HashMap { K: V }`   (Anthropic, Gemini, Ollama, etc.)
/// * JSON string `"{\"K\":\"V\"}"` -> parsed into `HashMap { K: V }`   (OpenAI strict mode)
/// * Empty string `""` or `"{}"` -> empty `HashMap`
pub fn deserialize_env_vars<'de, D>(deserializer: D) -> Result<HashMap<String, String>, D::Error>
where
    D: de::Deserializer<'de>,
{
    struct EnvVisitor;

    impl<'de> de::Visitor<'de> for EnvVisitor {
        type Value = HashMap<String, String>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a JSON object or a JSON-encoded string of key-value pairs")
        }

        // OpenAI sends a JSON-encoded string
        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            if v.is_empty() || v == "{}" {
                return Ok(HashMap::new());
            }
            serde_json::from_str(v).map_err(de::Error::custom)
        }

        // Anthropic / others send a native JSON object
        fn visit_map<M: de::MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
            HashMap::deserialize(de::value::MapAccessDeserializer::new(map))
        }
    }

    deserializer.deserialize_any(EnvVisitor)
}

/// Deserialize an **optional** `env` field from either a JSON object, JSON-encoded string, or null.
///
/// * Missing field (via `#[serde(default)]`) -> `None`
/// * `null` -> `None`
/// * JSON object `{"K": "V"}` -> `Some(HashMap { K: V })`
/// * JSON string `"{\"K\":\"V\"}"` -> `Some(parsed HashMap)`
/// * Empty string `""` or `"{}"` -> `Some(empty HashMap)`
pub fn deserialize_optional_env_vars<'de, D>(
    deserializer: D,
) -> Result<Option<HashMap<String, String>>, D::Error>
where
    D: de::Deserializer<'de>,
{
    struct OptionalEnvVisitor;

    impl<'de> de::Visitor<'de> for OptionalEnvVisitor {
        type Value = Option<HashMap<String, String>>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a JSON object, a JSON-encoded string, or null")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            if v.is_empty() || v == "{}" {
                return Ok(Some(HashMap::new()));
            }
            serde_json::from_str(v).map(Some).map_err(de::Error::custom)
        }

        fn visit_map<M: de::MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
            HashMap::deserialize(de::value::MapAccessDeserializer::new(map)).map(Some)
        }
    }

    deserializer.deserialize_any(OptionalEnvVisitor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct TestRequired {
        #[serde(default, deserialize_with = "deserialize_env_vars")]
        env: HashMap<String, String>,
    }

    #[derive(Deserialize)]
    struct TestOptional {
        #[serde(default, deserialize_with = "deserialize_optional_env_vars")]
        env: Option<HashMap<String, String>>,
    }

    // --- Required env (add_mcp_tool style) ---

    #[test]
    fn test_required_from_object() {
        let json = r#"{"env": {"KEY": "val"}}"#;
        let t: TestRequired = serde_json::from_str(json).unwrap();
        assert_eq!(t.env.get("KEY").unwrap(), "val");
    }

    #[test]
    fn test_required_from_string() {
        let json = r#"{"env": "{\"KEY\": \"val\"}"}"#;
        let t: TestRequired = serde_json::from_str(json).unwrap();
        assert_eq!(t.env.get("KEY").unwrap(), "val");
    }

    #[test]
    fn test_required_from_empty_string() {
        let json = r#"{"env": ""}"#;
        let t: TestRequired = serde_json::from_str(json).unwrap();
        assert!(t.env.is_empty());
    }

    #[test]
    fn test_required_from_empty_object_string() {
        let json = r#"{"env": "{}"}"#;
        let t: TestRequired = serde_json::from_str(json).unwrap();
        assert!(t.env.is_empty());
    }

    #[test]
    fn test_required_from_empty_object() {
        let json = r#"{"env": {}}"#;
        let t: TestRequired = serde_json::from_str(json).unwrap();
        assert!(t.env.is_empty());
    }

    #[test]
    fn test_required_missing_field() {
        let json = r#"{}"#;
        let t: TestRequired = serde_json::from_str(json).unwrap();
        assert!(t.env.is_empty());
    }

    #[test]
    fn test_required_multiple_keys_from_string() {
        let json = r#"{"env": "{\"A\": \"1\", \"B\": \"2\"}"}"#;
        let t: TestRequired = serde_json::from_str(json).unwrap();
        assert_eq!(t.env.len(), 2);
        assert_eq!(t.env["A"], "1");
        assert_eq!(t.env["B"], "2");
    }

    #[test]
    fn test_required_invalid_json_string_errors() {
        let json = r#"{"env": "not valid json"}"#;
        let result: Result<TestRequired, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // --- Optional env (edit_mcp_tool style) ---

    #[test]
    fn test_optional_from_object() {
        let json = r#"{"env": {"KEY": "val"}}"#;
        let t: TestOptional = serde_json::from_str(json).unwrap();
        assert_eq!(t.env.unwrap().get("KEY").unwrap(), "val");
    }

    #[test]
    fn test_optional_from_string() {
        let json = r#"{"env": "{\"KEY\": \"val\"}"}"#;
        let t: TestOptional = serde_json::from_str(json).unwrap();
        assert_eq!(t.env.unwrap().get("KEY").unwrap(), "val");
    }

    #[test]
    fn test_optional_null() {
        let json = r#"{"env": null}"#;
        let t: TestOptional = serde_json::from_str(json).unwrap();
        assert!(t.env.is_none());
    }

    #[test]
    fn test_optional_missing() {
        let json = r#"{}"#;
        let t: TestOptional = serde_json::from_str(json).unwrap();
        assert!(t.env.is_none());
    }

    #[test]
    fn test_optional_empty_string() {
        let json = r#"{"env": ""}"#;
        let t: TestOptional = serde_json::from_str(json).unwrap();
        assert_eq!(t.env, Some(HashMap::new()));
    }

    #[test]
    fn test_optional_from_empty_object() {
        let json = r#"{"env": {}}"#;
        let t: TestOptional = serde_json::from_str(json).unwrap();
        assert_eq!(t.env, Some(HashMap::new()));
    }
}
