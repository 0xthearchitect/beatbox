//! How strong a boundary a sandbox asked for, and whether it got one.
//!
//! The runtime can't check a boundary, but it can refuse to hand out a
//! weaker one than was asked for.
//!
//! | | |
//! |---|---|
//! | `none` | Whatever runs, runs as this process does. |
//! | `container` | Its own namespaces, the host's kernel. Not a boundary against hostile code. |
//! | `machine` | Its own kernel. The boundary malware requires. |
//!
//! There's no `model` level: a sandbox where nothing runs says so by not
//! declaring `process.execute`.
//!
//! **The level is a floor.** Asking for `container` and getting `machine` is
//! fine. The variants are declared weakest first, so the order is the rank.

use std::fmt;

use serde::Serialize;
use serde_json::Value;

use crate::events::Data;
use crate::faults::{ContractError, repr};

/// One of the three boundaries, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    /// No boundary at all.
    #[default]
    None,
    /// Its own namespaces, the host's kernel.
    Container,
    /// Its own kernel.
    Machine,
}

/// Every level, weakest first.
pub const ISOLATION_LEVELS: [Isolation; 3] =
    [Isolation::None, Isolation::Container, Isolation::Machine];

impl Isolation {
    /// The word a configuration and a registry use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Container => "container",
            Self::Machine => "machine",
        }
    }

    /// A level from its word, or a refusal to rank one nobody can: `strict`.
    pub fn parse(level: &str) -> Result<Self, ContractError> {
        ISOLATION_LEVELS
            .into_iter()
            .find(|known| known.as_str() == level)
            .ok_or_else(|| {
                ContractError::broken(format!(
                    "not an isolation level: {} (have none, container, machine)",
                    repr(level)
                ))
            })
    }
}

impl fmt::Display for Isolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Whether `provided` is as strong as `required`, or stronger.
pub fn at_least(provided: Isolation, required: Isolation) -> bool {
    provided >= required
}

/// The level a sandbox asked for, and anything its provider needs besides.
///
/// `options` is the provider's: "no network" is `-nic none` to one and
/// something else to another.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct IsolationPolicy {
    /// The floor.
    pub level: Isolation,
    /// The provider's half.
    pub options: Data,
}

impl IsolationPolicy {
    /// A policy asking for `level` and nothing more.
    pub fn at(level: Isolation) -> Self {
        Self {
            level,
            options: Data::new(),
        }
    }

    /// As JSON, the shape the records keep.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("a policy is JSON")
    }

    /// Read back what [`IsolationPolicy::to_value`] wrote. A missing level is
    /// `none`; a level nobody can rank is a broken contract.
    pub fn from_value(raw: &Value) -> Result<Self, ContractError> {
        let level = match raw.get("level").and_then(Value::as_str) {
            Some(text) if !text.is_empty() => Isolation::parse(text)?,
            _ => Isolation::None,
        };
        let options = raw
            .get("options")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        Ok(Self { level, options })
    }
}
