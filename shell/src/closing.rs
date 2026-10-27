//! The question the window asks on its way out, and what the answers do.
//!
//! The page draws the question (`Closing.tsx`) but can't hold a close open
//! while somebody reads, so the close is abandoned, the question published
//! as `closing`, and the answer comes back as `host.closing`:
//!
//! | | |
//! |---|---|
//! | `stop` | the daemon stops every machine, an install is asked to shut down, and the window goes |
//! | `leave` | the window goes; the daemon, and every machine, stay |
//! | `cancel` | the window stays |
//!
//! Nothing running, nothing asked. An answer nobody asked for -- a stale
//! click, a second answer -- changes nothing and says `false`.
//!
//! **If the page never answers** within [`ANSWER_WITHIN`], the window closes
//! as if told `leave`: the one answer that loses nothing, and a window that
//! can't close is worse than either.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use beatbox_contract::ClosingView;

use crate::Fault;

/// How long the page gets to put the question up before the window stops
/// waiting for it.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(4);

/// The three answers, in the order the contract lists them.
pub const CHOICES: [&str; 3] = ["stop", "leave", "cancel"];

/// One of [`CHOICES`], or the fault that says which they are.
pub fn check(choice: &str) -> Result<&'static str, Fault> {
    CHOICES
        .iter()
        .find(|known| **known == choice)
        .copied()
        .ok_or_else(|| {
            Fault::new(
                "bad-choice",
                format!("A closing answer is one of: {}.", CHOICES.join(", ")),
            )
        })
}

/// What to do about a close that was asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Close {
    /// Go: nothing is running, or it was already answered.
    Now,
    /// Put the question to the page, and wait.
    Ask(ClosingView),
    /// Already asking: this is a second press of the X.
    Wait,
}

/// What an answer asks of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Then {
    /// Nothing: it was not asking, or it was told to stay.
    Stay,
    /// Stop every machine, then go.
    StopAndGo,
    /// Go, and leave everything as it is.
    Go,
}

#[derive(Default)]
struct State {
    asking: bool,
    answered: Option<&'static str>,
}

/// The window's side of the question.
#[derive(Default)]
pub struct Closing {
    state: Mutex<State>,
}

impl Closing {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The X was pressed, and `running` is what the daemon says is still up
    /// -- `None` when it cannot say, which counts as nothing: a window must
    /// always be able to close.
    pub fn requested(&self, running: Option<ClosingView>) -> Close {
        let mut state = self.state();
        if state.answered.is_some() {
            return Close::Now;
        }
        if state.asking {
            return Close::Wait;
        }
        match running {
            Some(view) if view.sandboxes > 0 || view.installing => {
                state.asking = true;
                Close::Ask(view)
            }
            _ => Close::Now,
        }
    }

    /// `host.closing`. The choice is checked before anything else, so an
    /// invented one never reaches the window.
    pub fn answer(&self, choice: &str) -> Result<(bool, Then), Fault> {
        let known = check(choice)?;
        let mut state = self.state();
        if !state.asking {
            return Ok((false, Then::Stay));
        }
        state.asking = false;
        Ok(match known {
            "cancel" => (true, Then::Stay),
            "stop" => {
                state.answered = Some("stop");
                (true, Then::StopAndGo)
            }
            _ => {
                state.answered = Some("leave");
                (true, Then::Go)
            }
        })
    }

    /// The page had its chance and did not answer: go, as `leave`. Nothing,
    /// if it answered after all.
    pub fn unanswered(&self) -> Then {
        let mut state = self.state();
        if !state.asking {
            return Then::Stay;
        }
        state.asking = false;
        state.answered = Some("leave");
        Then::Go
    }

    /// Whether the question is up.
    pub fn asking(&self) -> bool {
        self.state().asking
    }

    /// What was answered, once it was.
    pub fn answered(&self) -> Option<&'static str> {
        self.state().answered
    }
}
