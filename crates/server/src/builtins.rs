//! The definitions BeatBox brings with it: data like any other, compiled in
//! so they can't be missing. A copy in the user's library is the same
//! service.

use crate::definition::Definition;

const SOURCES: [&str; 5] = [
    include_str!("../definitions/http-static.json"),
    include_str!("../definitions/nginx.json"),
    include_str!("../definitions/dns.json"),
    include_str!("../definitions/postgresql.json"),
    include_str!("../definitions/smtp.json"),
];

/// Every built-in definition, checked.
pub fn builtins() -> Vec<Definition> {
    SOURCES
        .iter()
        .map(|text| Definition::parse(text).expect("a built-in definition is valid"))
        .collect()
}
