//! One kind of failure, with a code the interface can branch on.
//!
//! `code` is for branching, never for display. When there's a `reason`, the
//! page writes the sentence in the reader's language and `message` (English)
//! is left for logs and tests.

use std::fmt;

use beatbox_reasons::Reason;

/// Something the page is meant to show, rather than something to crash on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiFault {
    /// For branching: `no-sandbox`, `bad-payload`, `refused`...
    pub code: String,
    /// For a person, in English, and for logs.
    pub message: String,
    /// Why, named, when the engine said.
    pub reason: Option<Reason>,
}

impl ApiFault {
    /// A fault with no named reason.
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            reason: None,
        }
    }

    /// A fault the engine explained.
    pub fn because(code: &str, message: impl Into<String>, reason: Option<Reason>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            reason,
        }
    }
}

impl fmt::Display for ApiFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiFault {}
