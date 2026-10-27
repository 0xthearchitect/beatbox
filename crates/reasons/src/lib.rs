//! Why BeatBox said no, as a name plus values, so the page can say it in
//! either language. The engine never writes user-facing text itself.
//!
//! The code is a locale key minus `beatbox.`: `reason!("machine.no_image")`
//! is `beatbox.machine.no_image`. web/tests/strings.test.mjs greps for
//! `reason!("…"` literals and checks both locale files, which is why the
//! code must be a literal.
//!
//! `Display` prints `<machine.no_image>`, not English, so an untranslated
//! reason on screen looks like the bug it is.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// One refusal or one finding, named rather than written out.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Reason {
    /// A locale key without its `beatbox.` prefix.
    pub code: String,
    /// What the sentence's placeholders are filled with. Always text.
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

impl Reason {
    /// A reason with no values. Prefer [`reason!`]: the strings check can see it.
    pub fn new(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            values: BTreeMap::new(),
        }
    }

    /// Add one value, as text. Any name works, `code` included ("QEMU exited with {code}").
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl fmt::Display) -> Self {
        self.values.insert(key.into(), value.to_string());
        self
    }

    /// The same reason with every value in `values` added, as text.
    #[must_use]
    pub fn with_values<K, V>(mut self, values: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: fmt::Display,
    {
        for (key, value) in values {
            self.values.insert(key.into(), value.to_string());
        }
        self
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "<{}>", self.code)
    }
}

/// `reason!("machine.no_such_image", path = image)`. Values become text.
///
/// The code must be a literal so the strings check can find it; a built
/// code is one nobody checked has a translation.
#[macro_export]
macro_rules! reason {
    ($code:literal $(,)?) => {
        $crate::Reason::new($code)
    };
    ($code:literal, $($key:ident = $value:expr),+ $(,)?) => {
        $crate::Reason::new($code)$(.with(stringify!($key), &$value))+
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_always_text() {
        let why = reason!("machine.too_many", count = 3, ok = true, path = "/tmp/a");
        assert_eq!(why.code, "machine.too_many");
        assert_eq!(why.values["count"], "3");
        assert_eq!(why.values["ok"], "true");
        assert_eq!(why.values["path"], "/tmp/a");
    }

    #[test]
    fn a_value_can_be_called_code_or_type() {
        let why = reason!("host.guest_went", code = "137", type = "host");
        assert_eq!(why.code, "host.guest_went");
        assert_eq!(why.values["code"], "137");
        assert_eq!(why.values["type"], "host");
    }

    #[test]
    fn untranslated_it_looks_like_the_bug_it_is() {
        assert_eq!(
            reason!("machine.no_image").to_string(),
            "<machine.no_image>"
        );
    }

    #[test]
    fn on_the_wire_it_is_a_code_and_its_values() {
        let why = reason!("engine.over_budget", resource = "cores");
        let wire = serde_json::to_value(&why).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({"code": "engine.over_budget", "values": {"resource": "cores"}})
        );
        assert_eq!(serde_json::from_value::<Reason>(wire).unwrap(), why);
    }
}
