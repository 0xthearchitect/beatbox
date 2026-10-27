//! One naming rule, for capabilities, actions and events alike.
//!
//! What a sandbox can do (`process.execute`), is asked to do, and reports
//! (`process.created`) share one shape and one matcher, so they can't drift.
//!
//! The shape is `namespace.verb`, lowercase, two segments or more
//! (`network.firewall.configure` is fine).
//!
//! A name that doesn't fit is a [`ContractError`] when it's written, because
//! a typo in a capability is a sandbox that silently can't do something.

use crate::faults::{ContractError, repr};

/// The one namespace a provider may not emit into: only the runtime says
/// `sandbox.started` or `sandbox.failed`, so a provider can't forge them.
pub const RESERVED: &str = "sandbox";

/// One segment: a lowercase letter, then lowercase letters, digits and
/// underscores.
pub(crate) fn segment(text: &str) -> bool {
    let mut characters = text.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && characters.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `namespace.verb`, or deeper: at least two segments, dots between them,
/// never leading, trailing or doubled.
pub fn is_dotted(name: &str) -> bool {
    let segments: Vec<&str> = name.split('.').collect();
    segments.len() >= 2 && segments.iter().all(|s| segment(s))
}

/// `*` on its own, or a prefix ending in `.*`. Not a glob on purpose: `proc*`
/// matching half a segment is how a pattern quietly widens.
fn is_pattern(pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match pattern.strip_suffix(".*") {
        Some(prefix) => prefix.split('.').all(segment),
        None => false,
    }
}

/// `name`, or which contract it broke and where.
pub fn validate<'a>(name: &'a str, what: &str) -> Result<&'a str, ContractError> {
    if is_dotted(name) {
        Ok(name)
    } else {
        Err(ContractError::broken(format!(
            "{what} must read like 'namespace.verb': {}",
            repr(name)
        )))
    }
}

/// `process.execute` -> `process`.
pub fn namespace(name: &str) -> &str {
    name.split_once('.').map_or(name, |(head, _)| head)
}

/// Whether `name` is covered by `pattern`.
///
/// `*` covers everything, `process.*` the namespace and anything nested in
/// it, and anything else is an exact name.
pub fn matches(name: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*').filter(|p| p.ends_with('.')) {
        // The dot is kept: process.* must not match processes.x.
        return name.starts_with(prefix);
    }
    name == pattern
}

/// `pattern`, or a refusal of a shape [`matches`] would read wrong.
pub fn validate_pattern<'a>(pattern: &'a str, what: &str) -> Result<&'a str, ContractError> {
    if is_dotted(pattern) || is_pattern(pattern) {
        Ok(pattern)
    } else {
        Err(ContractError::broken(format!(
            "{what} must be a name, 'namespace.*' or '*': {}",
            repr(pattern)
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shape_is_namespace_dot_verb() {
        for good in ["process.execute", "network.firewall.configure", "a1.b_2"] {
            assert!(is_dotted(good), "{good}");
        }
        for bad in [
            "",
            "process",
            "Process.Execute",
            "process.",
            ".execute",
            "process..execute",
            "1process.execute",
            "process.exec-ute",
            "process.*",
        ] {
            assert!(!is_dotted(bad), "{bad}");
        }
    }

    #[test]
    fn patterns_are_a_star_or_a_namespace_and_a_star() {
        for good in ["*", "process.*", "network.firewall.*", "process.created"] {
            assert!(validate_pattern(good, "p").is_ok(), "{good}");
        }
        for bad in [
            "proc*",
            "process*",
            "*.created",
            "process.*.x",
            ".*",
            "Process.*",
        ] {
            assert!(validate_pattern(bad, "p").is_err(), "{bad}");
        }
    }
}
