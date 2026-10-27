//! Asking a sandbox to do something, and what comes back.
//!
//! An action is named by the capability it needs (`process.execute` with
//! `{"command": ...}`), so the gate is a set lookup and there's one
//! vocabulary, not two.
//!
//! **"It didn't work" and "it couldn't be done" are different answers.** A
//! process that exited 1 is `ok: true` with `exit_code` in the data; a
//! sandbox without a filesystem asked to write a file is `ok: false`.

use std::fmt;

use beatbox_reasons::Reason;
use serde_json::Value;

use crate::events::Data;
use crate::faults::ContractError;
use crate::names::validate;

/// One request: the capability it needs, and what it needs to know.
#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    capability: String,
    /// What it needs to know. Any argument name goes, `name` and
    /// `capability` included.
    pub arguments: Data,
}

impl Action {
    /// An action needing `capability`, or a broken contract for a name that
    /// is not `namespace.verb`.
    pub fn new(capability: &str, arguments: Data) -> Result<Self, ContractError> {
        validate(capability, "an action's capability")?;
        Ok(Self {
            capability: capability.to_owned(),
            arguments,
        })
    }

    /// The capability it needs.
    pub fn capability(&self) -> &str {
        &self.capability
    }

    /// One argument, if it was given.
    pub fn argument(&self, name: &str) -> Option<&Value> {
        self.arguments.get(name)
    }

    /// As JSON, the shape the records keep.
    pub fn to_value(&self) -> Value {
        serde_json::json!({"capability": self.capability, "arguments": self.arguments})
    }
}

impl fmt::Display for Action {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let named: Vec<String> = self
            .arguments
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        write!(formatter, "{}({})", self.capability, named.join(", "))
    }
}

/// What the sandbox did, or why it did not.
///
/// Returned, not raised, even for refusals, unlike the lifecycle methods:
/// a caller stepping through actions wants one place to look after each
/// step, while a caller starting a sandbox wants to stop.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionOutcome {
    /// What was asked.
    pub action: Action,
    /// Whether it could be done.
    pub ok: bool,
    /// What came of it.
    pub data: Data,
    /// Why not, when it could not.
    pub reason: Option<Reason>,
}

impl ActionOutcome {
    /// It was done, and this came of it.
    pub fn done(action: Action, data: Data) -> Self {
        Self {
            action,
            ok: true,
            data,
            reason: None,
        }
    }

    /// It could not be done, and this is why.
    pub fn failed(action: Action, reason: Option<Reason>) -> Self {
        Self {
            action,
            ok: false,
            data: Data::new(),
            reason,
        }
    }

    /// The capability it needed.
    pub fn capability(&self) -> &str {
        self.action.capability()
    }

    /// As JSON, the shape the records keep.
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "action": self.action.to_value(),
            "ok": self.ok,
            "data": self.data,
            "reason": self.reason,
        })
    }
}
