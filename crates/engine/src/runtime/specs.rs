//! What an action takes and what it gives back, said by the type itself.
//!
//! The shape behind a capability's name, so a form can be drawn and a
//! missing argument refused before the provider is touched.
//!
//! **The runtime checks inputs, the tests check outputs.** Missing is
//! `engine.missing_argument`, wrong kind `engine.bad_argument`. Outputs go
//! through [`ActionSpec::assert_outputs`] in each provider's tests, since
//! a broken answer is a bug, not something to re-check on every call.
//!
//! Not JSON Schema: six kinds, enough to pick a control. Undeclared
//! arguments pass, and a capability with no spec isn't checked. Whether now
//! is a good moment is the provider's call, not a spec's.

use beatbox_reasons::{Reason, reason};
use serde_json::Value;

use crate::events::Data;

/// The shape of one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Text, or a number or bool the provider reads as text: a snapshot
    /// called 2024 arrives as a number and is still a name.
    Text,
    /// Any JSON number.
    Number,
    /// `true` or `false`.
    Bool,
    /// An array.
    List,
    /// An object.
    Object,
    /// Anything at all.
    Any,
}

impl Kind {
    /// The word a spec and a refusal use for it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Bool => "bool",
            Self::List => "list",
            Self::Object => "object",
            Self::Any => "any",
        }
    }

    /// Whether `value` is one. `null` never is: a null is a value left out.
    pub fn fits(self, value: &Value) -> bool {
        match self {
            Self::Text => value.is_string() || value.is_number() || value.is_boolean(),
            Self::Number => value.is_number(),
            Self::Bool => value.is_boolean(),
            Self::List => value.is_array(),
            Self::Object => value.is_object(),
            Self::Any => !value.is_null(),
        }
    }
}

/// One named value an action takes or gives back.
///
/// `required` means present and not null. A value that is there, required
/// or not, has to be of its kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Its key in the arguments or in the answer.
    pub name: String,
    /// What it holds.
    pub kind: Kind,
    /// Whether it may be left out.
    pub required: bool,
}

impl Field {
    /// A value that has to be there.
    pub fn required(name: &str, kind: Kind) -> Self {
        Self {
            name: name.to_owned(),
            kind,
            required: true,
        }
    }

    /// A value that may be left out.
    pub fn optional(name: &str, kind: Kind) -> Self {
        Self {
            required: false,
            ..Self::required(name, kind)
        }
    }

    /// What is wrong with this field in `data`, if anything.
    fn problem(&self, data: &Data) -> Option<&'static str> {
        match data.get(&self.name) {
            None | Some(Value::Null) if self.required => Some("missing"),
            None | Some(Value::Null) => None,
            Some(value) if !self.kind.fits(value) => Some("wrong_kind"),
            Some(_) => None,
        }
    }
}

/// What one capability takes and gives back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionSpec {
    /// The capability it describes, declared by the same type.
    pub capability: String,
    /// What the arguments may hold.
    pub inputs: Vec<Field>,
    /// What a successful answer holds.
    pub outputs: Vec<Field>,
}

impl ActionSpec {
    /// `capability`, taking `inputs` and giving back `outputs`.
    pub fn new(capability: &str, inputs: Vec<Field>, outputs: Vec<Field>) -> Self {
        Self {
            capability: capability.to_owned(),
            inputs,
            outputs,
        }
    }

    /// The first argument that breaks this spec, as the refusal to give.
    pub fn check_inputs(&self, arguments: &Data) -> Result<(), Reason> {
        for field in &self.inputs {
            match field.problem(arguments) {
                Some("missing") => {
                    return Err(reason!(
                        "engine.missing_argument",
                        capability = self.capability,
                        argument = field.name
                    ));
                }
                Some(_) => {
                    return Err(reason!(
                        "engine.bad_argument",
                        capability = self.capability,
                        argument = field.name,
                        kind = field.kind.as_str()
                    ));
                }
                None => {}
            }
        }
        Ok(())
    }

    /// For a provider's tests: panic unless a successful answer holds what
    /// this spec promises.
    pub fn assert_outputs(&self, data: &Data) {
        for field in &self.outputs {
            if let Some(problem) = field.problem(data) {
                panic!(
                    "{} answered with {} {problem} (a {} {}): {data:?}",
                    self.capability,
                    field.name,
                    if field.required {
                        "required"
                    } else {
                        "optional"
                    },
                    field.kind.as_str()
                );
            }
        }
    }
}
