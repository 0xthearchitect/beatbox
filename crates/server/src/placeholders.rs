//! `{port}` in a definition's text, and what it becomes.
//!
//! `{` a key `}`, the key being what [`is_key`] accepts or one of
//! [`OWN`](crate::definition::OWN). Anything else in braces is text, so
//! `{"a": 1}` stays as written.
//!
//! Substituting inside a word (`--port={port}`) is safe: the result is an
//! argv element or file content, never a line a shell reads.
//!
//! ponytail: a literal `{port}` cannot be written; add `{{` escaping when a
//! definition needs one.

use std::collections::BTreeMap;

use crate::definition::is_key;

/// Each `{key}` in `text`, in order, repeats included.
pub fn placeholders(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('}') else { break };
        let key = &rest[..close];
        let own = key.strip_prefix('_').is_some_and(is_key);
        if is_key(key) || own {
            found.push(key);
            rest = &rest[close + 1..];
        }
    }
    found
}

/// `text` with every placeholder that `values` has replaced by its value.
/// One it has not is left as it was: [`Definition::parse`](crate::Definition::parse)
/// has already refused a definition where that could happen.
pub fn fill(text: &str, values: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}').map(|close| (&after[..close], close)) {
            Some((key, close)) if values.contains_key(key) => {
                out.push_str(&values[key]);
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_keys_in_braces_are_placeholders() {
        assert_eq!(
            placeholders("--port={port} {_id} {\"a\": 1} {Bad} {}"),
            ["port", "_id"]
        );
    }

    #[test]
    fn filling_leaves_what_it_does_not_know() {
        let values = BTreeMap::from([("port".to_owned(), "8000".to_owned())]);
        assert_eq!(
            fill("--port={port} {other} {\"a\": 1}", &values),
            "--port=8000 {other} {\"a\": 1}"
        );
    }
}
