//! Generates the bridge types from `contract.json`, the single source of
//! truth. Rename a field there and every old use here stops compiling.
//!
//! Type language: `str`, `int`, `float`, `bool`, `any`, a PascalCase type,
//! `[]` for list, `?` for optional. Anything else fails the build.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::{env, fs};

use serde_json::{Map, Value};

const CONTRACT: &str = "contract.json";

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets this"));
    let path = manifest.join(CONTRACT);
    println!("cargo::rerun-if-changed={}", path.display());

    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let contract: Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} is not JSON: {error}", path.display()));

    let types = object(&contract, "types");
    let known: BTreeSet<&str> = types.keys().map(String::as_str).collect();

    let mut out = String::new();
    let version = contract["contract_version"]
        .as_u64()
        .expect("contract_version is a number");
    writeln!(out, "/// `contract_version` in `contract.json`.").unwrap();
    writeln!(out, "pub const CONTRACT_VERSION: u32 = {version};\n").unwrap();

    let envelope = object(&contract, "envelope");
    write_struct(
        &mut out,
        "Envelope",
        "The one shape every answer has: `ok`, and `data` or `error`.",
        envelope,
        &known,
    );
    for (name, fields) in types {
        let fields = fields
            .as_object()
            .unwrap_or_else(|| panic!("type {name} is not an object"));
        write_struct(
            &mut out,
            name,
            &format!("`{name}`, as `contract.json` declares it."),
            fields,
            &known,
        );
    }

    writeln!(out, "/// Every type `contract.json` declares, by name.").unwrap();
    writeln!(out, "pub const TYPES: &[&str] = &[").unwrap();
    for name in types.keys() {
        writeln!(out, "    {name:?},").unwrap();
    }
    writeln!(out, "];\n").unwrap();

    writeln!(out, "/// Every action the bridge answers, by name.").unwrap();
    writeln!(out, "pub const ACTIONS: &[ActionSpec] = &[").unwrap();
    for (name, action) in object(&contract, "actions") {
        let summary = action["summary"].as_str().unwrap_or_default();
        let mutates = action["mutates"].as_bool().unwrap_or(false);
        let response = action["response"]
            .as_str()
            .expect("an action has a response");
        check(response, &known, name);
        let side = answered_by(action, name);
        let mut payload = String::new();
        for (field, kind) in action["payload"]
            .as_object()
            .expect("a payload is an object")
        {
            let kind = kind.as_str().expect("a payload field has a type");
            check(kind, &known, name);
            write!(payload, "({field:?}, {kind:?}), ").unwrap();
        }
        writeln!(
            out,
            "    ActionSpec {{ name: {name:?}, summary: {summary:?}, mutates: {mutates}, \
             payload: &[{payload}], response: {response:?}, answered_by: {side} }},"
        )
        .unwrap();
    }
    writeln!(out, "];\n").unwrap();

    writeln!(out, "/// Every event the bridge pushes, by name.").unwrap();
    writeln!(out, "pub const EVENTS: &[EventSpec] = &[").unwrap();
    for (name, event) in object(&contract, "events") {
        let summary = event["summary"].as_str().unwrap_or_default();
        let payload = event["payload"]
            .as_str()
            .expect("an event has a payload type");
        check(payload, &known, name);
        let side = answered_by(event, name);
        writeln!(
            out,
            "    EventSpec {{ name: {name:?}, summary: {summary:?}, payload: {payload:?}, \
             answered_by: {side} }},"
        )
        .unwrap();
    }
    writeln!(out, "];").unwrap();

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo sets this"));
    fs::write(out_dir.join("contract.rs"), out).expect("OUT_DIR is writable");
    fs::write(out_dir.join("reserialize.rs"), reserialize(types)).expect("OUT_DIR is writable");
}

/// Daemon or shell. Every action and event must say, so nothing is routed by guess.
fn answered_by(entry: &Value, name: &str) -> &'static str {
    match entry["answered_by"].as_str() {
        Some("daemon") => "AnsweredBy::Daemon",
        Some("shell") => "AnsweredBy::Shell",
        other => panic!("{name}: answered_by is {other:?}, not \"daemon\" or \"shell\""),
    }
}

fn object<'a>(contract: &'a Value, key: &str) -> &'a Map<String, Value> {
    contract[key]
        .as_object()
        .unwrap_or_else(|| panic!("contract.json has no {key:?} object"))
}

fn write_struct(
    out: &mut String,
    name: &str,
    doc: &str,
    fields: &Map<String, Value>,
    known: &BTreeSet<&str>,
) {
    writeln!(out, "/// {doc}").unwrap();
    writeln!(
        out,
        "#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]"
    )
    .unwrap();
    // camelCase on the wire, snake_case here. deny_unknown_fields so drift is loud.
    writeln!(
        out,
        "#[serde(rename_all = \"camelCase\", deny_unknown_fields)]"
    )
    .unwrap();
    writeln!(out, "pub struct {name} {{").unwrap();
    for (field, kind) in fields {
        let kind = kind
            .as_str()
            .unwrap_or_else(|| panic!("{name}.{field} has no type"));
        writeln!(out, "    /// `{kind}`").unwrap();
        writeln!(
            out,
            "    pub {}: {},",
            identifier(field),
            rust_type(kind, known, name)
        )
        .unwrap();
    }
    writeln!(out, "}}\n").unwrap();
}

/// Contract type -> Rust type. Unknown types panic.
fn rust_type(kind: &str, known: &BTreeSet<&str>, owner: &str) -> String {
    check(kind, known, owner);
    let (base, optional) = match kind.strip_suffix('?') {
        Some(base) => (base, true),
        None => (kind, false),
    };
    let (base, list) = match base.strip_suffix("[]") {
        Some(base) => (base, true),
        None => (base, false),
    };
    let mut rust = match base {
        "str" => "String".to_owned(),
        "int" => "i64".to_owned(),
        "float" => "f64".to_owned(),
        "bool" => "bool".to_owned(),
        "any" => "serde_json::Value".to_owned(),
        name => name.to_owned(),
    };
    if list {
        rust = format!("Vec<{rust}>");
    }
    if optional {
        rust = format!("Option<{rust}>");
    }
    rust
}

fn check(kind: &str, known: &BTreeSet<&str>, owner: &str) {
    let base = kind.trim_end_matches('?').trim_end_matches("[]");
    let scalar = matches!(base, "str" | "int" | "float" | "bool" | "any");
    if !scalar && !known.contains(base) {
        panic!("{owner}: {kind:?} is not in contract.json's type language");
    }
}

/// Field name as a Rust identifier (`type` needs escaping).
fn identifier(field: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "static", "struct", "trait", "true", "type", "unsafe",
        "use", "where", "while", "yield",
    ];
    if KEYWORDS.contains(&field) {
        format!("r#{field}")
    } else {
        field.to_owned()
    }
}

/// Test helper: parse a value as a named type and serialize it back.
fn reserialize(types: &Map<String, Value>) -> String {
    let mut out = String::new();
    writeln!(
        out,
        "/// Read `value` as the contract type called `name`, and write it back."
    )
    .unwrap();
    writeln!(out, "/// `None` for a name the contract does not declare.").unwrap();
    writeln!(
        out,
        "pub fn reserialize(name: &str, value: serde_json::Value) \
         -> Option<Result<serde_json::Value, serde_json::Error>> {{"
    )
    .unwrap();
    writeln!(out, "    Some(match name {{").unwrap();
    for name in std::iter::once("Envelope").chain(types.keys().map(String::as_str)) {
        writeln!(
            out,
            "        {name:?} => serde_json::from_value::<{name}>(value).and_then(serde_json::to_value),"
        )
        .unwrap();
    }
    writeln!(out, "        _ => return None,").unwrap();
    writeln!(out, "    }})").unwrap();
    writeln!(out, "}}").unwrap();
    out
}
