//! Which of the two languages BeatBox speaks, and remembering it.
//!
//! `system.locale` and `system.set_language`. The strings are
//! `locales/{en,pt_PT}.json`, built into the binary; the page asks at boot,
//! so switching is a reload, not a rebuild.
//!
//! The choice is kept in `shell.json` beside the machines and read back at
//! start, so a window opens in the language it was left in.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use beatbox_contract::Locale;
use serde_json::{Map, Value, json};

use crate::Fault;

/// What BeatBox ships, in the order the settings screen offers them.
pub const SUPPORTED: [&str; 2] = ["en", "pt_PT"];

/// What a string falls back to, key by key.
pub const DEFAULT: &str = "en";

/// Only these travel to the page.
pub const PREFIX: &str = "beatbox.";

const EN: &str = include_str!("../../locales/en.json");
const PT_PT: &str = include_str!("../../locales/pt_PT.json");

fn catalogue(language: &str) -> &'static BTreeMap<String, String> {
    static EN_STRINGS: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    static PT_STRINGS: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    let (cell, text) = if language == "pt_PT" {
        (&PT_STRINGS, PT_PT)
    } else {
        (&EN_STRINGS, EN)
    };
    cell.get_or_init(|| {
        let parsed: Value = serde_json::from_str(text).unwrap_or_default();
        parsed
            .as_object()
            .map(|strings| {
                strings
                    .iter()
                    .map(|(key, value)| {
                        let text = value
                            .as_str()
                            .map_or_else(|| value.to_string(), str::to_owned);
                        (key.clone(), text)
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// The language manager's `resolve_language`: a code BeatBox ships, or the
/// nearest one (`en_GB` is `en`, `pt_BR` is `pt_PT`), or the default.
pub fn resolve(code: &str) -> &'static str {
    let code = code.trim();
    if let Some(known) = SUPPORTED.iter().find(|known| **known == code) {
        return known;
    }
    if code.starts_with("pt") {
        return "pt_PT";
    }
    DEFAULT
}

/// The language this window speaks, and where it is written down.
pub struct Language {
    active: Mutex<&'static str>,
    settings: PathBuf,
}

impl Language {
    /// Whatever `settings` last said, or the default.
    pub fn at(settings: PathBuf) -> Self {
        let remembered = read_down(&settings)["language"]
            .as_str()
            .map_or(DEFAULT, resolve);
        Self {
            active: Mutex::new(remembered),
            settings,
        }
    }

    /// The code in use.
    pub fn active(&self) -> &'static str {
        *self.active.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One string in the active language, falling back to the default's,
    /// then to the key itself -- as `tr()` did.
    pub fn text(&self, key: &str) -> String {
        catalogue(self.active())
            .get(key)
            .or_else(|| catalogue(DEFAULT).get(key))
            .cloned()
            .unwrap_or_else(|| key.to_owned())
    }

    /// Every `beatbox.*` string in the active language, a missing one taken
    /// from the default (the locale parity test keeps that from happening).
    pub fn locale(&self) -> Locale {
        let active = self.active();
        let mut strings = Map::new();
        for language in [DEFAULT, active] {
            for (key, value) in catalogue(language) {
                if key.starts_with(PREFIX) {
                    strings.insert(key.clone(), Value::String(value.clone()));
                }
            }
        }
        Locale {
            language: active.to_owned(),
            available: SUPPORTED.iter().map(|code| (*code).to_owned()).collect(),
            strings: Value::Object(strings),
        }
    }

    /// Switch, for the whole of BeatBox, and write it down.
    pub fn set(&self, language: &str) -> Result<Locale, Fault> {
        let Some(known) = SUPPORTED.iter().find(|known| **known == language) else {
            return Err(Fault::new(
                "no-such-language",
                format!("{language} is not one of: {}.", SUPPORTED.join(", ")),
            ));
        };
        *self.active.lock().unwrap_or_else(PoisonError::into_inner) = known;
        if let Err(error) = write_down(&self.settings, "language", json!(known)) {
            // A preference that will not save is not a reason to refuse it.
            log::error!("BeatBox could not remember the language choice: {error}");
        }
        Ok(self.locale())
    }
}

/// Everything `shell.json` holds, or nothing when it is not there yet.
pub(crate) fn read_down(settings: &Path) -> Value {
    fs::read_to_string(settings)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or_default()
}

/// One key of `shell.json` changed and the others kept, written whole or
/// not at all.
pub(crate) fn write_down(settings: &Path, key: &str, value: Value) -> std::io::Result<()> {
    let mut saved = read_down(settings).as_object().cloned().unwrap_or_default();
    saved.insert(key.into(), value);
    if let Some(folder) = settings.parent() {
        fs::create_dir_all(folder)?;
    }
    let partial = settings.with_extension("json.tmp");
    fs::write(
        &partial,
        serde_json::to_string_pretty(&Value::Object(saved)).expect("JSON"),
    )?;
    fs::rename(&partial, settings)
}
