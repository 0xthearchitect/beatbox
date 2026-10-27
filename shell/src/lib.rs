//! BeatBox's window, less the window: what the shell answers, and where it
//! sends the rest.
//!
//! The page sees one door. Every request is admitted as the daemon admits
//! one (`beatbox_contract::door`), then routed by what `contract.json` says
//! answers it:
//!
//! - the person's half is answered here, by the window: their files
//!   (`files.*`), their language (`system.locale`, `system.set_language`),
//!   their few choices (`system.preferences`, `system.set_preferences`),
//!   which BeatBox this is (`system.about`, `host.reveal`) and the closing
//!   question (`host.closing`);
//! - the machine's half goes to `beatboxd`, over the wire, started if it is
//!   not running (`link.rs`), and so do its events, back.
//!
//! Tauri is only in `main.rs`: everything here is plain Rust, and the tests
//! drive it with a pretend window.

pub mod about;
pub mod closing;
pub mod files;
pub mod language;
pub mod link;
pub mod one;
pub mod preferences;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use beatbox_contract::{AnsweredBy, ApiError, ClosingView, Envelope, door};
use serde_json::{Map, Value, json};

use crate::closing::{Close, Closing, Then};
use crate::language::Language;
use crate::link::Link;
use crate::preferences::Preferences;

/// A refusal of the window's own, a plain sentence with no named reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    /// For branching.
    pub code: String,
    /// For a person, in English, and for logs.
    pub message: String,
}

impl Fault {
    /// A fault.
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

impl From<Fault> for ApiError {
    fn from(fault: Fault) -> Self {
        door::refusal(&fault.code, fault.message)
    }
}

/// The window, as far as the shell is allowed to know about it.
pub trait Window: Send + Sync {
    /// Tell the page something it did not ask about.
    fn push(&self, name: &str, payload: &Value);
    /// Close, for good: the question has been answered, or never needed
    /// asking.
    fn go(&self);
    /// Open or focus one machine's BeatBox-owned display window.
    fn display(&self, id: &str, name: &str, url: &str, password: &str) -> Result<(), String>;
    /// Close that window, if it is open: the machine it showed has gone.
    fn undisplay(&self, _id: &str) {}
}

/// The machine a `sandbox` event says has stopped, shut down, failed or
/// gone -- whose display window has nothing left to show.
pub fn ended(name: &str, payload: &Value) -> Option<String> {
    if name != "sandbox" {
        return None;
    }
    let said = payload["name"].as_str().unwrap_or_default();
    if !matches!(
        said,
        "sandbox.stopped" | "sandbox.failed" | "sandbox.destroyed"
    ) {
        return None;
    }
    // The wire is camelCase; accept the contract's own spelling too.
    payload["sandboxId"]
        .as_str()
        .or_else(|| payload["sandbox_id"].as_str())
        .map(str::to_owned)
}

/// `~/.beatbox`, or `BEATBOX_HOME`: where the machines are, and so where
/// the window keeps its few settings and its lock.
pub fn beatbox_home() -> PathBuf {
    std::env::var_os("BEATBOX_HOME")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || {
                std::env::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".beatbox")
            },
            PathBuf::from,
        )
}

type Held = Arc<Mutex<Option<Arc<dyn Window>>>>;

/// The shell's half of the contract, and the way to the daemon's.
pub struct Shell {
    link: Link,
    language: Language,
    preferences: Preferences,
    closing: Closing,
    window: Held,
}

/// A contract shape, as the page's JSON.
fn wire<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("a contract shape is JSON")
}

/// `str(value or "")`, for a payload's text.
fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

impl Shell {
    /// A shell reaching the daemon through `link`, speaking `language`.
    pub fn new(link: Link, language: Language, preferences: Preferences) -> Arc<Self> {
        let window: Held = Arc::default();
        let told = Arc::clone(&window);
        link.listen(move |name, payload| {
            let held = told.lock().unwrap_or_else(PoisonError::into_inner).clone();
            if let Some(window) = held {
                if let Some(id) = ended(&name, &payload) {
                    window.undisplay(&id);
                }
                window.push(&name, &payload);
            }
        });
        Arc::new(Self {
            link,
            language,
            preferences,
            closing: Closing::default(),
            window,
        })
    }

    /// Lend the shell its window.
    pub fn attach(&self, window: Arc<dyn Window>) {
        *self.window.lock().unwrap_or_else(PoisonError::into_inner) = Some(window);
    }

    /// Take it back: a window that has closed must not be spoken to.
    pub fn detach(&self) {
        *self.window.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn window(&self) -> Option<Arc<dyn Window>> {
        self.window
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The language, for the window's own title and anything else it writes.
    pub fn language(&self) -> &Language {
        &self.language
    }

    /// The person's choices, for the window's own start.
    pub fn preferences(&self) -> &Preferences {
        &self.preferences
    }

    /// The daemon's link.
    pub fn link(&self) -> &Link {
        &self.link
    }

    /// The question's state.
    pub fn closing(&self) -> &Closing {
        &self.closing
    }

    /// Run one request from the page. Never fails: every answer, the
    /// refusals included, is an envelope.
    pub fn answer(self: &Arc<Self>, action: &str, payload: &Value) -> Envelope {
        let (spec, admitted) = match door::admit(action, payload) {
            Ok(admitted) => admitted,
            Err(refusal) => return door::refused(refusal),
        };
        if spec.answered_by == AnsweredBy::Daemon {
            // Sent as the page wrote it: the daemon admits it again, in
            // exactly these words.
            return self.link.call(action, payload);
        }
        match self.here(spec.name, &admitted) {
            Ok(data) => door::answered(data),
            Err(fault) => door::refused(fault.into()),
        }
    }

    fn here(self: &Arc<Self>, action: &str, payload: &Map<String, Value>) -> Result<Value, Fault> {
        match action {
            "files.places" => Ok(wire(&files::places())),
            "files.list" => files::listing(&text(&payload["path"]), &text(&payload["suffixes"]))
                .map(|view| wire(&view)),
            "files.read" => files::read(&text(&payload["path"])).map(Value::String),
            "system.locale" => Ok(wire(&self.language.locale())),
            "system.set_language" => {
                let view = self.language.set(&text(&payload["language"]))?;
                let value = wire(&view);
                // Every open screen re-reads its text, not only the one
                // that asked.
                if let Some(window) = self.window() {
                    window.push("locale", &value);
                }
                Ok(value)
            }
            "system.preferences" => Ok(wire(&self.preferences.view())),
            "system.set_preferences" => self.preferences.set(payload).map(|view| wire(&view)),
            "system.about" => Ok(wire(&about::about(&self.link))),
            "host.reveal" => about::reveal(&text(&payload["what"])).map(|()| json!(true)),
            "host.closing" => self.host_closing(&text(&payload["choice"])),
            "host.display" => {
                let Some(window) = self.window() else {
                    return Err(Fault::new("no-host", "There is no BeatBox window here."));
                };
                window
                    .display(
                        &text(&payload["id"]),
                        &text(&payload["name"]),
                        &text(&payload["url"]),
                        &text(&payload["password"]),
                    )
                    .map_err(|message| Fault::new("display", message))?;
                Ok(json!(true))
            }
            other => Err(Fault::new(
                "internal",
                format!("the window has no handler for {other}"),
            )),
        }
    }

    fn host_closing(self: &Arc<Self>, choice: &str) -> Result<Value, Fault> {
        // The choice first: an invented one never reaches the window.
        closing::check(choice)?;
        let Some(window) = self.window() else {
            return Err(Fault::new(
                "no-host",
                "There is no BeatBox window here to close.",
            ));
        };
        let (asked, then) = self.closing.answer(choice)?;
        match then {
            Then::Stay => {}
            Then::Go => {
                thread::spawn(move || window.go());
            }
            Then::StopAndGo => {
                let shell = Arc::clone(self);
                thread::spawn(move || {
                    shell.stop_everything();
                    window.go();
                });
            }
        }
        Ok(json!(asked))
    }

    /// What is still up, as the page's question counts it: `None` when no
    /// daemon answers, which is nothing.
    pub fn running(&self) -> Option<ClosingView> {
        let status = self.link.status()?;
        Some(ClosingView {
            sandboxes: status["running"].as_i64().unwrap_or(0),
            installing: status["installing"].as_bool().unwrap_or(false),
        })
    }

    /// The X was pressed. `Now` is the window's to act on; `Ask` has already
    /// put the question to the page. A remembered answer is given here
    /// instead of asked for, through the same door the page's would take,
    /// and the window waits for it as for a second press.
    pub fn close_requested(self: &Arc<Self>) -> Close {
        let decided = self.closing.requested(self.running());
        let remembered = self.preferences.on_close();
        if matches!(decided, Close::Ask(_)) && remembered != "ask" {
            if let Err(fault) = self.host_closing(remembered) {
                log::error!("BeatBox could not close as remembered: {}", fault.message);
            }
            return Close::Wait;
        }
        if let (Close::Ask(view), Some(window)) = (&decided, self.window()) {
            window.push(
                "closing",
                &serde_json::to_value(view).expect("a view is JSON"),
            );
        }
        decided
    }

    /// The page did not answer in time: go, leaving everything running.
    pub fn unanswered(&self) {
        if self.closing.unanswered() == Then::Go {
            log::warn!(
                "BeatBox: the page never answered the closing question; leaving everything running"
            );
            if let Some(window) = self.window() {
                window.go();
            }
        }
    }

    /// Stop every machine, newest first, and ask an install to shut down:
    /// the window's "stop". Failures are logged, never raised: the window
    /// goes either way.
    pub fn stop_everything(&self) {
        match self.link.daemon("stop-running") {
            Ok(answer) => {
                let stubborn = answer["stubborn"].as_array().cloned().unwrap_or_default();
                if !stubborn.is_empty() {
                    log::error!("BeatBox could not stop on close: {stubborn:?}");
                }
            }
            Err(error) => log::error!("BeatBox could not ask the daemon to stop: {error}"),
        }
        if self.running().is_some_and(|view| view.installing) {
            let _ = self.link.call("disks.finish", &json!({}));
        }
    }
}
