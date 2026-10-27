//! The person's few choices about the window, and remembering them.
//!
//! `system.preferences` and `system.set_preferences`. Kept in `shell.json`
//! beside the language (`language.rs`), one key each:
//!
//! | | |
//! |---|---|
//! | `on_close` | what closing does while machines run: `ask` puts the question (`closing.rs`); `stop` and `leave` answer it before it is asked, the same answer the page would have sent |
//! | `builder_tools` | whether the rail shows Engine and Foundations |
//! | `new_memory_mb`, `new_cpus`, `new_reach`, `new_agent` | what the new machine form starts at |
//!
//! Unsaved, each is what the window did before there was a choice: it
//! asked, it showed the builder's pages in a debug build or with
//! `BEATBOX_BUILDER` set, and a new machine started at 2 GB, 2 CPUs, no
//! network and the agent on.

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use beatbox_contract::Preferences as View;
use serde_json::{Map, Value, json};

use crate::Fault;
use crate::language::{read_down, write_down};

/// The answers `on_close` can hold, `ask` first because it is the default.
pub const ON_CLOSE: [&str; 3] = ["ask", "stop", "leave"];

/// What the rail did before it was a choice.
fn builder_by_default() -> bool {
    cfg!(debug_assertions) || std::env::var_os("BEATBOX_BUILDER").is_some()
}

/// The ways out a new machine can start with, `none` first because it is
/// the default: the same three the new machine form offers.
pub const NEW_REACH: [&str; 3] = ["none", "isolated", "nat"];

/// The most a default may ask for. Past these it is a typing slip, not a
/// machine anyone means to start every time.
const MOST_MEMORY_MB: i64 = 256 * 1024;
const MOST_CPUS: i64 = 256;

fn known(choices: &[&'static str], choice: &str) -> Option<&'static str> {
    choices.iter().find(|known| **known == choice).copied()
}

#[derive(Clone, Copy)]
struct Saved {
    on_close: &'static str,
    builder_tools: bool,
    new_memory_mb: i64,
    new_cpus: i64,
    new_reach: &'static str,
    new_agent: bool,
}

fn bad(message: impl Into<String>) -> Fault {
    Fault::new("bad-choice", message)
}

fn between(value: &Value, least: i64, most: i64) -> Option<i64> {
    value.as_i64().filter(|n| (least..=most).contains(n))
}

/// The choices, and where they are written down.
pub struct Preferences {
    saved: Mutex<Saved>,
    settings: PathBuf,
}

impl Preferences {
    /// Whatever `settings` last said, or the defaults.
    pub fn at(settings: PathBuf) -> Self {
        let saved = read_down(&settings);
        Self {
            saved: Mutex::new(Saved {
                on_close: saved["on_close"]
                    .as_str()
                    .and_then(|choice| known(&ON_CLOSE, choice))
                    .unwrap_or("ask"),
                builder_tools: saved["builder_tools"]
                    .as_bool()
                    .unwrap_or_else(builder_by_default),
                new_memory_mb: between(&saved["new_memory_mb"], 256, MOST_MEMORY_MB)
                    .unwrap_or(2048),
                new_cpus: between(&saved["new_cpus"], 1, MOST_CPUS).unwrap_or(2),
                new_reach: saved["new_reach"]
                    .as_str()
                    .and_then(|choice| known(&NEW_REACH, choice))
                    .unwrap_or("none"),
                new_agent: saved["new_agent"].as_bool().unwrap_or(true),
            }),
            settings,
        }
    }

    fn saved(&self) -> Saved {
        *self.saved.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What closing does while machines run.
    pub fn on_close(&self) -> &'static str {
        self.saved().on_close
    }

    /// Whether the rail shows the builder's pages.
    pub fn builder_tools(&self) -> bool {
        self.saved().builder_tools
    }

    /// As the page reads them.
    pub fn view(&self) -> View {
        let saved = self.saved();
        View {
            on_close: saved.on_close.to_owned(),
            builder_tools: saved.builder_tools,
            new_memory_mb: saved.new_memory_mb,
            new_cpus: saved.new_cpus,
            new_reach: saved.new_reach.to_owned(),
            new_agent: saved.new_agent,
        }
    }

    /// Change them all, as `system.set_preferences` admitted them, and
    /// write them down. Nothing changes when one is wrong.
    pub fn set(&self, given: &Map<String, Value>) -> Result<View, Fault> {
        let on_close = given["on_close"].as_str().unwrap_or_default();
        let Some(on_close) = known(&ON_CLOSE, on_close) else {
            return Err(bad(format!("onClose is one of: {}.", ON_CLOSE.join(", "))));
        };
        let Some(builder_tools) = given["builder_tools"].as_bool() else {
            return Err(bad("builderTools is true or false."));
        };
        let Some(new_memory_mb) = between(&given["new_memory_mb"], 256, MOST_MEMORY_MB) else {
            return Err(bad(format!("newMemoryMb is from 256 to {MOST_MEMORY_MB}.")));
        };
        let Some(new_cpus) = between(&given["new_cpus"], 1, MOST_CPUS) else {
            return Err(bad(format!("newCpus is from 1 to {MOST_CPUS}.")));
        };
        let new_reach = given["new_reach"].as_str().unwrap_or_default();
        let Some(new_reach) = known(&NEW_REACH, new_reach) else {
            return Err(bad(format!(
                "newReach is one of: {}.",
                NEW_REACH.join(", ")
            )));
        };
        let Some(new_agent) = given["new_agent"].as_bool() else {
            return Err(bad("newAgent is true or false."));
        };
        *self.saved.lock().unwrap_or_else(PoisonError::into_inner) = Saved {
            on_close,
            builder_tools,
            new_memory_mb,
            new_cpus,
            new_reach,
            new_agent,
        };
        for (key, value) in [
            ("on_close", json!(on_close)),
            ("builder_tools", json!(builder_tools)),
            ("new_memory_mb", json!(new_memory_mb)),
            ("new_cpus", json!(new_cpus)),
            ("new_reach", json!(new_reach)),
            ("new_agent", json!(new_agent)),
        ] {
            if let Err(error) = write_down(&self.settings, key, value) {
                // As with the language: a choice that will not save is not a
                // reason to refuse it for this window.
                log::error!("BeatBox could not remember {key}: {error}");
            }
        }
        Ok(self.view())
    }
}
