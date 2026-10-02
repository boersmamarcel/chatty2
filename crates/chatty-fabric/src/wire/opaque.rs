//! A payload the wire carries but does not type (ADR-0021 § 1).

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use serde_json::value::RawValue;

/// The largest [`Opaque`] value, in bytes of JSON: RC-0's 32 MiB conversation
/// cap (`fabric-resumable-conversations` §4.1), the biggest payload a worker
/// sends, until ADR-0021's Q4 settles per-direction frame caps.
pub const OPAQUE_CAP_BYTES: usize = 32 * 1024 * 1024;

/// One of the four payloads chatty-core owns by contract — the captured
/// conversation, the handoff answer, the handoff schema and a virtual
/// agent's evidence data — carried verbatim.
///
/// The broker never reads one: it is decoded as raw JSON, checked against
/// [`OPAQUE_CAP_BYTES`] and passed on. Only the side that owns the shape
/// parses it ([`Opaque::to_value`]). Deserialising one needs `serde_json`'s
/// own deserializer reading a string, which is what the codec does; a
/// `serde_json::Value` cannot be decoded into one.
#[derive(Clone)]
pub struct Opaque(Box<RawValue>);

/// Why a value cannot be an [`Opaque`].
#[derive(Debug, thiserror::Error)]
pub enum OpaqueError {
    #[error("an opaque payload of {0} bytes is over the {OPAQUE_CAP_BYTES}-byte cap")]
    TooLarge(usize),
    #[error("not JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl Opaque {
    /// `value`, serialised, if it is under the cap.
    pub fn from_value(value: &Value) -> Result<Self, OpaqueError> {
        Self::from_json(serde_json::to_string(value)?)
    }

    /// `json`, which must be one JSON value, if it is under the cap.
    pub fn from_json(json: String) -> Result<Self, OpaqueError> {
        if json.len() > OPAQUE_CAP_BYTES {
            return Err(OpaqueError::TooLarge(json.len()));
        }
        Ok(Self(RawValue::from_string(json)?))
    }

    /// The payload, parsed: for the side that owns its shape.
    pub fn to_value(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_str(self.0.get())
    }

    /// The payload's JSON text.
    pub fn get(&self) -> &str {
        self.0.get()
    }

    /// Its length in bytes of JSON.
    pub fn len(&self) -> usize {
        self.0.get().len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.get().is_empty()
    }
}

/// Only the length: a conversation can be 32 MiB.
impl std::fmt::Debug for Opaque {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Opaque({} bytes)", self.len())
    }
}

/// Equal when the JSON text is: what the wire carried, byte for byte.
impl PartialEq for Opaque {
    fn eq(&self, other: &Self) -> bool {
        self.0.get() == other.0.get()
    }
}

impl Eq for Opaque {}

impl Serialize for Opaque {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Opaque {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        if raw.get().len() > OPAQUE_CAP_BYTES {
            return Err(serde::de::Error::custom(OpaqueError::TooLarge(
                raw.get().len(),
            )));
        }
        Ok(Self(raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_opaque_value_round_trips_verbatim() {
        let messages = json!([{"role": "user", "content": "hi"}]);
        let opaque = Opaque::from_value(&messages).unwrap();
        #[derive(Serialize, Deserialize)]
        struct Holder {
            conversation: Opaque,
        }
        let line = serde_json::to_string(&Holder {
            conversation: opaque.clone(),
        })
        .unwrap();
        let held: Holder = serde_json::from_str(&line).unwrap();
        assert_eq!(held.conversation, opaque, "the text, byte for byte");
        assert_eq!(held.conversation.to_value().unwrap(), messages);
    }

    #[test]
    fn an_opaque_value_over_the_cap_is_refused() {
        let big = format!("\"{}\"", "x".repeat(OPAQUE_CAP_BYTES));
        assert!(matches!(
            Opaque::from_json(big.clone()),
            Err(OpaqueError::TooLarge(_))
        ));
        let line = format!(r#"{{"conversation":{big}}}"#);
        #[derive(Debug, Deserialize)]
        #[allow(dead_code)]
        struct Holder {
            conversation: Opaque,
        }
        let err = serde_json::from_str::<Holder>(&line).unwrap_err();
        assert!(err.to_string().contains("cap"), "{err}");
    }
}
