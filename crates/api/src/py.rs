//! Reading a payload the way the Python facade read it.
//!
//! The door checks which fields arrive, never what's in them, so these
//! readings (`str(x or "")`, `float(cpu)`, `bool(network)`...) are the
//! facade's whole type discipline. `golden/api/walk.json` was recorded
//! through them.
//!
//! Where Python raised, these return code `internal` with `Type: text`.
//! Nothing branches on the text.

use serde_json::{Map, Value};

use crate::fault::ApiFault;

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

/// The name Python gives the type a JSON value is read as.
pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(number) if number.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Python's `str()`.
pub fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => repr(other),
    }
}

/// `str(value or "")`.
pub fn text_or_empty(value: &Value) -> String {
    if truthy(value) {
        text(value)
    } else {
        String::new()
    }
}

/// Python's `repr()`.
pub fn repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(number) => match number.as_f64() {
            Some(float) if number.is_f64() => float_repr(float),
            _ => number.to_string(),
        },
        Value::String(text) => repr_str(text),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(key, item)| format!("{}: {}", repr_str(key), repr(item)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// Python's `repr()` of a `float`: `4.0`, `1e+20`, `inf`.
fn float_repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.into();
    }
    let magnitude = value.abs();
    if magnitude != 0.0 && !(1e-4..1e16).contains(&magnitude) {
        // Python: shortest digits, exponent with a sign and at least two
        // figures. Rust's `{:e}` gives the same digits.
        let written = format!("{value:e}");
        let (digits, exponent) = written.split_once('e').expect("{:e} has an e");
        let exponent: i32 = exponent.parse().expect("an exponent is a number");
        let sign = if exponent < 0 { '-' } else { '+' };
        return format!("{digits}e{sign}{:02}", exponent.abs());
    }
    let written = format!("{value}");
    if written.contains('.') {
        written
    } else {
        format!("{written}.0")
    }
}

/// Python's `repr()` of a `str`: single quotes unless the text has one and
/// no double quote, and escapes for what cannot be printed.
pub fn repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if printable(c) => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// Near enough to `str.isprintable()`: no control characters, no separators
/// but the space, and none of the invisible formatting marks.
fn printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    !(c.is_control()
        || c.is_whitespace()
        || matches!(c, '\u{ad}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{feff}'))
}

fn internal(kind: &str, message: impl AsRef<str>) -> ApiFault {
    ApiFault::new("internal", format!("{kind}: {}", message.as_ref()))
}

/// Python's `float()`.
pub fn float(value: &Value) -> Result<f64, ApiFault> {
    match value {
        Value::Number(number) => Ok(number.as_f64().unwrap_or_default()),
        Value::Bool(flag) => Ok(f64::from(u8::from(*flag))),
        Value::String(text) => {
            let trimmed = text.trim().replace('_', "");
            trimmed.parse::<f64>().map_err(|_| {
                internal(
                    "ValueError",
                    format!("could not convert string to float: {}", repr_str(text)),
                )
            })
        }
        other => Err(internal(
            "TypeError",
            format!(
                "float() argument must be a string or a real number, not '{}'",
                type_name(other)
            ),
        )),
    }
}

/// Python's `int()`.
pub fn int(value: &Value) -> Result<i64, ApiFault> {
    match value {
        Value::Number(number) => {
            if let Some(whole) = number.as_i64() {
                return Ok(whole);
            }
            let float = number.as_f64().unwrap_or_default();
            if float.is_nan() {
                return Err(internal(
                    "ValueError",
                    "cannot convert float NaN to integer",
                ));
            }
            if float.is_infinite() {
                return Err(internal(
                    "OverflowError",
                    "cannot convert float infinity to integer",
                ));
            }
            Ok(float.trunc() as i64)
        }
        Value::Bool(flag) => Ok(i64::from(*flag)),
        Value::String(text) => text.trim().replace('_', "").parse().map_err(|_| {
            internal(
                "ValueError",
                format!("invalid literal for int() with base 10: {}", repr_str(text)),
            )
        }),
        other => Err(internal(
            "TypeError",
            format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                type_name(other)
            ),
        )),
    }
}

/// `int(value or 0)`.
pub fn int_or_zero(value: &Value) -> Result<i64, ApiFault> {
    if truthy(value) { int(value) } else { Ok(0) }
}

/// `dict(value or {})`, for the maps a payload carries.
///
/// Anything but an object is a `TypeError`; nobody sends a list of pairs.
pub fn mapping(value: &Value) -> Result<Map<String, Value>, ApiFault> {
    match value {
        Value::Object(map) => Ok(map.clone()),
        other if !truthy(other) => Ok(Map::new()),
        other => Err(internal(
            "TypeError",
            format!("'{}' object is not a mapping", type_name(other)),
        )),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn repr_quotes_the_way_python_does() {
        assert_eq!(repr_str("sbx-nobody"), "'sbx-nobody'");
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(repr_str("both ' and \""), "'both \\' and \"'");
        assert_eq!(repr_str("a\nb\\c"), "'a\\nb\\\\c'");
        assert_eq!(repr_str("\u{7}"), "'\\x07'");
        assert_eq!(repr_str("olá"), "'olá'");
        assert_eq!(repr(&json!(null)), "None");
        assert_eq!(repr(&json!(4.0)), "4.0");
        assert_eq!(repr(&json!(1e20)), "1e+20");
        assert_eq!(repr(&json!(0.5)), "0.5");
        assert_eq!(repr(&json!([1, "a"])), "[1, 'a']");
        assert_eq!(repr(&json!({"k": true})), "{'k': True}");
    }

    #[test]
    fn numbers_are_read_the_way_python_reads_them() {
        assert_eq!(float(&json!("4")).unwrap(), 4.0);
        assert_eq!(float(&json!(true)).unwrap(), 1.0);
        assert_eq!(
            float(&json!("x")).unwrap_err().message,
            "ValueError: could not convert string to float: 'x'"
        );
        assert_eq!(
            float(&json!({})).unwrap_err().message,
            "TypeError: float() argument must be a string or a real number, not 'dict'"
        );
        assert_eq!(int(&json!(4.7)).unwrap(), 4);
        assert_eq!(int(&json!(" 12 ")).unwrap(), 12);
        assert_eq!(
            int(&json!("1.5")).unwrap_err().message,
            "ValueError: invalid literal for int() with base 10: '1.5'"
        );
        assert_eq!(int_or_zero(&json!(null)).unwrap(), 0);
        assert_eq!(text_or_empty(&json!(null)), "");
        assert_eq!(text_or_empty(&json!(7)), "7");
    }
}
