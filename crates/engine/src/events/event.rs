//! One thing that happened: a name, an owner, a moment and some data. For
//! code to read, not a log line for a person.
//!
//! **Order by `sequence`, not the timestamp.** Two events in one millisecond
//! are ordinary, so the bus stamps a counter that only goes up.
//!
//! The bus builds events, not the emitter: moment and sequence have to come
//! from one place, so the fields are read-only outside this crate.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::names;

/// What an event, an action or a provider's facts carry: JSON, opaque to the
/// runtime, which carries it and never reads it.
pub type Data = Map<String, Value>;

/// `process.created` on `sbx-3f9c21`, at a moment, with a pid in it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    pub(crate) name: String,
    pub(crate) sandbox_id: String,
    pub(crate) at: String,
    pub(crate) sequence: u64,
    pub(crate) data: Data,
}

impl Event {
    /// What happened: `process.created`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whose it was. Empty for an event about no sandbox in particular.
    pub fn sandbox_id(&self) -> &str {
        &self.sandbox_id
    }

    /// When, as the bus's clock said it.
    pub fn at(&self) -> &str {
        &self.at
    }

    /// Before and after: 1, 2, 3, for as long as this bus lives.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// What the emitter said about it.
    pub fn data(&self) -> &Data {
        &self.data
    }

    /// `process.created` -> `process`.
    pub fn namespace(&self) -> &str {
        names::namespace(&self.name)
    }

    /// Whether `pattern` covers this event. See [`names::matches`].
    pub fn matches(&self, pattern: &str) -> bool {
        names::matches(&self.name, pattern)
    }

    /// One value out of the data.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    /// The event as JSON, the shape the records keep.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("an event is JSON")
    }
}

impl std::fmt::Display for Event {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "[{}] {} {}",
            self.sequence, self.name, self.sandbox_id
        )
    }
}
