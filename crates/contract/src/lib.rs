//! The bridge types, generated from `contract.json` by build.rs (camelCase
//! on the wire, `deny_unknown_fields`), plus the action/event tables.
//! Hand-written: [`door`] (request checks) and [`wire`] (shell to daemon).
//!
//! The daemon and the shell share this and nothing else. Checked against
//! `golden/contract/` and `golden/api/walk.json`.

use serde::{Deserialize, Serialize};

pub mod door;
pub mod wire;

/// Which half of BeatBox answers an action or sends an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnsweredBy {
    /// `beatboxd`, where sandboxes run.
    Daemon,
    /// The window: the person's files, language, preferences.
    Shell,
}

/// One action, as `contract.json` declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionSpec {
    /// `namespace.verb`, as the page calls it.
    pub name: &'static str,
    /// One sentence on what it does.
    pub summary: &'static str,
    /// Changes state. Never call speculatively (see `mutates` in contract.json).
    pub mutates: bool,
    /// Payload fields (snake_case) and their contract types.
    pub payload: &'static [(&'static str, &'static str)],
    /// What it answers with, in the contract's type language.
    pub response: &'static str,
    /// Which half answers it.
    pub answered_by: AnsweredBy,
}

/// One event, as `contract.json` declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventSpec {
    /// The name the page subscribes to.
    pub name: &'static str,
    /// One sentence on when it fires.
    pub summary: &'static str,
    /// What it carries, in the contract's type language.
    pub payload: &'static str,
    /// Which half sends it.
    pub answered_by: AnsweredBy,
}

/// The action called `name`, if the contract declares one.
pub fn action(name: &str) -> Option<&'static ActionSpec> {
    ACTIONS.iter().find(|spec| spec.name == name)
}

include!(concat!(env!("OUT_DIR"), "/contract.rs"));
