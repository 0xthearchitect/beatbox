//! The six states a sandbox can be in, and the only ways between them.
//!
//! Everything reads this table, from a waiting caller to a greyed-out
//! button, so it's data in one place and every transition goes through
//! [`next_state`].
//!
//! ```text
//!                  create                 (the runtime, not a transition)
//!                     v
//!                  CREATED <------------------ reset ------------------+
//!                     |                                                |
//!                   start                                              |
//!                     v                                                |
//!         +------> RUNNING ---- stop ----> STOPPED ---- start -----> RUNNING
//!         |        |     ^                    |
//!       resume   pause   |                  reset ---------------------+
//!         |        |     |
//!         +----- PAUSED -+          PAUSED ---- stop ----> STOPPED
//!
//!         any but destroyed ---- destroy ----> DESTROYED   (terminal)
//!         any but destroyed ----- fail ------> FAILED ---- reset ---> CREATED
//! ```
//!
//! `Failed` is the one state only the world puts a sandbox in. Without it a
//! dead guest reads as `running` (a lie) or `stopped` (as if asked to stop).
//! So `fail` is in the table but not in [`CALLABLE`]: only an observation
//! takes it.
//!
//! No `starting` or `stopping`: a transition completed or it didn't.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Where a sandbox is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lifecycle {
    /// Made and ready, but nothing is executing yet.
    Created,
    /// Executing. The only state in which a sandbox can be doing something.
    Running,
    /// Frozen, and resumable. Gated on the `lifecycle.pause` capability.
    Paused,
    /// Was running, is not, and was asked to stop. Startable again.
    Stopped,
    /// Was one of the above, and the provider says it is gone. The world did
    /// this, not a caller.
    Failed,
    /// Gone for good. Nothing leads out of here.
    Destroyed,
}

/// Every state, in declaration order.
pub const LIFECYCLES: [Lifecycle; 6] = [
    Lifecycle::Created,
    Lifecycle::Running,
    Lifecycle::Paused,
    Lifecycle::Stopped,
    Lifecycle::Failed,
    Lifecycle::Destroyed,
];

impl Lifecycle {
    /// The word on the wire and on disk.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::Destroyed => "destroyed",
        }
    }

    /// Whether the sandbox still exists and can still be acted on.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            Self::Created | Self::Running | Self::Paused | Self::Stopped
        )
    }
}

impl fmt::Display for Lifecycle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Lifecycle {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        LIFECYCLES
            .into_iter()
            .find(|state| state.as_str() == text)
            .ok_or_else(|| format!("not a lifecycle state: {text:?}"))
    }
}

/// A way from one state to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transition {
    /// Begin executing.
    Start,
    /// Stop executing, keeping whatever was made.
    Stop,
    /// Freeze.
    Pause,
    /// Thaw.
    Resume,
    /// Back to how it was created.
    Reset,
    /// Take it all away.
    Destroy,
    /// Not callable: only an observation can take this edge.
    Fail,
}

/// The six a caller can ask for, in the order an interface offers them.
pub const CALLABLE: [Transition; 6] = [
    Transition::Start,
    Transition::Stop,
    Transition::Pause,
    Transition::Resume,
    Transition::Reset,
    Transition::Destroy,
];

/// Every edge in the table, including the one only an observation can take.
pub const TRANSITIONS: [Transition; 7] = [
    Transition::Start,
    Transition::Stop,
    Transition::Pause,
    Transition::Resume,
    Transition::Reset,
    Transition::Destroy,
    Transition::Fail,
];

impl Transition {
    /// The word on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Reset => "reset",
            Self::Destroy => "destroy",
            Self::Fail => "fail",
        }
    }
}

impl fmt::Display for Transition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Transition {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        TRANSITIONS
            .into_iter()
            .find(|transition| transition.as_str() == text)
            .ok_or_else(|| format!("not a transition: {text:?}"))
    }
}

/// Where `transition` leads from `state`, or `None` if nowhere.
///
/// Anything missing is refused, so read the gaps: no `created -> pause`
/// (nothing to freeze), no `failed -> start` (reset or destroy first),
/// nothing out of `destroyed`. Stopping a paused guest needs no resume:
/// every real provider can kill one.
pub fn next_state(state: Lifecycle, transition: Transition) -> Option<Lifecycle> {
    use Lifecycle::*;
    use Transition as T;
    match (state, transition) {
        (Destroyed, _) => None,
        (_, T::Destroy) => Some(Destroyed),
        (Created | Running | Paused | Stopped, T::Fail) => Some(Failed),
        (Created | Stopped, T::Start) => Some(Running),
        (Running, T::Pause) => Some(Paused),
        (Paused, T::Resume) => Some(Running),
        (Running | Paused, T::Stop) => Some(Stopped),
        (Running | Paused | Stopped | Failed, T::Reset) => Some(Created),
        _ => None,
    }
}

/// Whether the table has this edge.
pub fn allows(state: Lifecycle, transition: Transition) -> bool {
    next_state(state, transition).is_some()
}

/// Everything a caller could ask for from here, in [`CALLABLE`] order.
///
/// What an interface greys out buttons with, so `fail` is left out.
pub fn transitions_from(state: Lifecycle) -> Vec<Transition> {
    CALLABLE
        .into_iter()
        .filter(|transition| allows(state, *transition))
        .collect()
}
