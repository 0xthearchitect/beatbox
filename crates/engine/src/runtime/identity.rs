//! Who a sandbox is: four fields that never change while it exists.
//!
//! Apart from config and state because it outlives the sandbox: an event
//! carries the id long after the sandbox is destroyed.
//!
//! `version` is the provider's, so a recording read a month later says which
//! implementation produced it.

use std::fmt;

use serde::Serialize;
use serde_json::Value;

/// `sbx-3f9c21`, of type `host`, called `windows-client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SandboxIdentity {
    /// `sbx-` and twelve hex digits, unless somebody restored another.
    pub id: String,
    /// The registered type that built it.
    #[serde(rename = "type")]
    pub type_name: String,
    /// What to call it.
    pub name: String,
    /// The provider's version.
    pub version: String,
}

impl SandboxIdentity {
    /// An identity, as a caller that is restoring one writes it.
    pub fn new(id: &str, type_name: &str, name: &str, version: &str) -> Self {
        Self {
            id: id.to_owned(),
            type_name: type_name.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
        }
    }

    /// What to call this in a sentence: its name, or its type if unnamed.
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.type_name
        } else {
            &self.name
        }
    }

    /// As JSON, the shape the records keep.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("an identity is JSON")
    }

    /// Read back what [`SandboxIdentity::to_value`] wrote, forgivingly: a
    /// missing field is empty, and a missing version is `"0"`.
    pub fn from_value(raw: &Value) -> Self {
        let text = |key: &str, default: &str| {
            raw.get(key)
                .and_then(text_of)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| default.to_owned())
        };
        Self {
            id: text("id", ""),
            type_name: text("type", ""),
            name: text("name", ""),
            version: text("version", "0"),
        }
    }
}

/// A record's scalar as text, or nothing for null.
pub(crate) fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

impl fmt::Display for SandboxIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.label(), self.id)
    }
}
