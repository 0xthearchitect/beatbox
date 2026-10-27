//! What the engine refuses with, and the difference between a no and a break.
//!
//! Three families, kept apart because a caller treats them differently:
//!
//! | | |
//! |---|---|
//! | [`EngineError::Refused`] | The runtime said no before anything was attempted. Nothing ran, nothing changed, and the sandbox is exactly where it was. Safe to try something else. |
//! | [`EngineError::Provider`] | Something was attempted and it broke. The world may have moved even though the transition did not complete, and [`ProviderError::fatal`] says whether the sandbox survived it. |
//! | [`EngineError::Contract`] | Somebody broke the contract -- an unknown sandbox id, a capability named `Process.Execute`. A bug, not a condition, and no amount of retrying will help. |
//!
//! The first two carry a [`Reason`], so the reader gets it in their
//! language. A [`ContractError`] is plain English on purpose: only a
//! developer reads it, and a bug report has no place on a user's screen.

use std::fmt;

use beatbox_reasons::Reason;

/// Everything the engine refuses or reports deliberately.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineError {
    /// The runtime said no and nothing was attempted (illegal transition,
    /// missing capability, unknown type, weak isolation, empty budget). The
    /// sandbox is unchanged, so it's safe to catch and carry on.
    Refused(Reason),
    /// A provider tried and could not.
    Provider(ProviderError),
    /// Somebody broke the contract. This is a bug and reads like one.
    Contract(ContractError),
}

impl EngineError {
    /// The named reason, when there is one: always for a refusal, usually for
    /// a provider's failure, never for a broken contract.
    pub fn reason(&self) -> Option<&Reason> {
        match self {
            Self::Refused(why) => Some(why),
            Self::Provider(error) => error.reason.as_ref(),
            Self::Contract(_) => None,
        }
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(why) => write!(formatter, "{why}"),
            Self::Provider(error) => write!(formatter, "{error}"),
            Self::Contract(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<ProviderError> for EngineError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}

impl From<ContractError> for EngineError {
    fn from(error: ContractError) -> Self {
        Self::Contract(error)
    }
}

/// A provider tried and could not.
///
/// `fatal` means the guest went with the call: the runtime then moves the
/// sandbox to `failed` instead of claiming something still runs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProviderError {
    /// Why, named.
    pub reason: Option<Reason>,
    /// Whether the sandbox was lost along with the call.
    pub fatal: bool,
}

impl ProviderError {
    /// One call failed and the sandbox survived it.
    pub fn new(reason: Reason) -> Self {
        Self {
            reason: Some(reason),
            fatal: false,
        }
    }

    /// The call failed and took the sandbox with it.
    pub fn fatal(reason: Reason) -> Self {
        Self {
            reason: Some(reason),
            fatal: true,
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.reason {
            Some(why) => write!(formatter, "{why}"),
            None => formatter.write_str("ProviderError"),
        }
    }
}

impl std::error::Error for ProviderError {}

/// Somebody broke the contract. An English sentence, for a developer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// Anything else the contract forbids.
    Broken(String),
    /// Asked about a sandbox this runtime never created, or never heard of.
    UnknownSandbox(String),
}

impl ContractError {
    pub(crate) fn broken(message: impl Into<String>) -> Self {
        Self::Broken(message.into())
    }

    /// The sentence.
    pub fn message(&self) -> &str {
        match self {
            Self::Broken(message) | Self::UnknownSandbox(message) => message,
        }
    }
}

impl fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for ContractError {}

/// A value as Python's `repr` would quote it in these sentences: `'x'`.
pub(crate) fn repr(text: &str) -> String {
    format!("'{text}'")
}
