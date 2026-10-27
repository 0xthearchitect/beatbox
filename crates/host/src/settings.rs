//! Reading a host sandbox's settings with Python's truthiness and `str()`,
//! because `golden/host/spec.json` was recorded with exactly that reading.

use beatbox_engine::SandboxConfig;
use serde_json::Value;

/// Python's `bool()`.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// Python's `str()`, for the scalars a setting holds.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        other => other.to_string(),
    }
}

/// Python's `int()`, for a port number: `0` for what is not one.
pub fn py_int(value: &Value) -> i64 {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        Value::Bool(flag) => i64::from(*flag),
        Value::String(text) => text.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// `str(config.setting(key, "") or "")`.
pub fn text(config: &SandboxConfig, key: &str) -> String {
    match config.setting(key) {
        Some(value) if truthy(value) => py_str(value),
        _ => String::new(),
    }
}

/// `bool(config.setting(key, default))`.
pub fn flag(config: &SandboxConfig, key: &str, default: bool) -> bool {
    config.setting(key).map_or(default, truthy)
}
