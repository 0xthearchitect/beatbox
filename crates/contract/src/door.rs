//! The checks every request passes before a handler. Shared so daemon and
//! shell refuse identically. Checked against `golden/contract/envelopes.json`.
//!
//! Order matters (the goldens pin it): unknown action; top-level keys
//! camelCase -> snake_case; unexpected field; missing field. Field names in
//! refusals go back to camelCase, sorted, since page authors read them.

use serde_json::{Map, Value};

use crate::{ACTIONS, ActionSpec, ApiError, Envelope};

/// `objectiveId` -> `objective_id`: every uppercase letter becomes `_` + lowercase.
pub fn snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for c in name.chars() {
        if c.is_uppercase() {
            out.push('_');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// `duration_minutes` -> `durationMinutes`: first part as is, the rest through [`title`].
pub fn camel(name: &str) -> String {
    let (head, tail) = name.split_once('_').unwrap_or((name, ""));
    let mut out = head.to_owned();
    for part in tail.split('_').filter(|part| !part.is_empty()) {
        out.push_str(&title(part));
    }
    out
}

/// Python's `str.title()`, which the goldens were recorded with: a non-letter
/// starts a new word, so `b1c` -> `B1C`.
fn title(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut after_a_letter = false;
    for c in text.chars() {
        let cased = c.is_uppercase() || c.is_lowercase();
        if cased {
            if after_a_letter {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
        } else {
            out.push(c);
        }
        after_a_letter = cased;
    }
    out
}

/// A refusal with no reason code; door refusals are for developers.
pub fn refusal(code: &str, message: impl Into<String>) -> ApiError {
    ApiError {
        code: code.to_owned(),
        message: message.into(),
        reason: None,
    }
}

/// An answer.
pub fn answered(data: Value) -> Envelope {
    Envelope {
        ok: true,
        data: Some(data),
        error: None,
    }
}

/// A refusal, as an envelope.
pub fn refused(error: ApiError) -> Envelope {
    Envelope {
        ok: false,
        data: None,
        error: Some(error),
    }
}

/// Let a request in (its spec and snake_case payload) or refuse it.
///
/// Only top-level keys are converted: what's inside `settings` and
/// `arguments` belongs to the provider and stays as sent (walk.json checks
/// this). `null` = empty payload; a non-object is refused.
pub fn admit(
    action: &str,
    payload: &Value,
) -> Result<(&'static ActionSpec, Map<String, Value>), ApiError> {
    let Some(spec) = ACTIONS.iter().find(|spec| spec.name == action) else {
        return Err(refusal(
            "unknown-action",
            format!("No such action: {action}"),
        ));
    };
    let given = match payload {
        Value::Null => Map::new(),
        Value::Object(map) => map.clone(),
        _ => {
            return Err(refusal("bad-payload", format!("{action} takes an object")));
        }
    };
    let payload: Map<String, Value> = given
        .into_iter()
        .map(|(key, value)| (snake(&key), value))
        .collect();

    let declared = |name: &str| spec.payload.iter().any(|(field, _)| *field == name);
    let mut unexpected: Vec<String> = payload
        .keys()
        .filter(|name| !declared(name))
        .map(|name| camel(name))
        .collect();
    if !unexpected.is_empty() {
        unexpected.sort();
        return Err(refusal(
            "bad-payload",
            format!("{action} does not take: {}", unexpected.join(", ")),
        ));
    }
    let mut missing: Vec<String> = spec
        .payload
        .iter()
        .filter(|(field, _)| !payload.contains_key(*field))
        .map(|(field, _)| camel(field))
        .collect();
    if !missing.is_empty() {
        missing.sort();
        return Err(refusal(
            "bad-payload",
            format!("{action} needs: {}", missing.join(", ")),
        ));
    }
    Ok((spec, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_naming_policy_is_the_pythons_both_ways() {
        assert_eq!(snake("objectiveId"), "objective_id");
        assert_eq!(snake("sizeGb"), "size_gb");
        assert_eq!(snake("sandboxId"), "sandbox_id");
        assert_eq!(camel("size_gb"), "sizeGb");
        assert_eq!(camel("memory_free_mb"), "memoryFreeMb");
        assert_eq!(camel("id"), "id");
        // A digit starts a new word, like str.title().
        assert_eq!(camel("a_b1c"), "aB1C");
        assert_eq!(camel("x_ABC"), "xAbc");
        assert_eq!(camel("trailing_"), "trailing");
    }
}
