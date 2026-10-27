//! The sandbox runtime.
//!
//! A lifecycle, capabilities, actions, an event bus, a resource ledger, a
//! provider contract, a registry, and [`MockSandbox`] to prove them.
//! `golden/engine/` holds the walk through all of it, event by event.
//!
//! ```text
//!                 SandboxRuntime
//!                       |
//!          +------------+------------+
//!          |                         |
//!       Sandbox                   Sandbox          one lane each
//!          |                         |
//!     SandboxProvider          SandboxProvider     MockSandbox, HostSandbox, ...
//!          \                         /
//!           +------- EventBus ------+              one turn at a time
//! ```
//!
//! Nothing global, no filesystem, no threads of its own, no async. It can't
//! see QEMU: the crate graph doesn't allow it. Every sandbox is a handle
//! behind its own lock, reading one never waits, and the bus delivers in
//! sequence order whichever thread publishes.

pub mod clock;
pub mod events;
pub mod faults;
pub mod names;
pub mod runtime;

pub use beatbox_reasons::{Reason, reason};
pub use events::{Data, Event, EventBus, Handler, Recorder, Subscription};
pub use faults::{ContractError, EngineError, ProviderError};
pub use runtime::actions::{Action, ActionOutcome};
pub use runtime::capabilities::CapabilitySet;
pub use runtime::config::SandboxConfig;
pub use runtime::identity::SandboxIdentity;
pub use runtime::isolation::{Isolation, IsolationPolicy};
pub use runtime::lifecycle::{Lifecycle, Transition};
pub use runtime::peers::{Peers, Requirement};
pub use runtime::provider::{Emitter, ProviderSpec, SandboxProvider};
pub use runtime::providers::MockSandbox;
pub use runtime::registry::{SandboxRegistry, SandboxType};
pub use runtime::resources::{NOTHING, ResourceKind, ResourceLedger, Resources};
pub use runtime::runtime::{RuntimeParts, SandboxRuntime};
pub use runtime::sandbox::Sandbox;
pub use runtime::specs::{ActionSpec, Field, Kind};
pub use runtime::state::{Observation, SandboxState};

/// A little [`Data`], written the way JSON is: `data!({"pid": 1})`.
///
/// Anything but an object is an empty map, because data is always a map.
#[macro_export]
macro_rules! data {
    ($($json:tt)+) => {
        match ::serde_json::json!($($json)+) {
            ::serde_json::Value::Object(map) => map,
            _ => $crate::Data::new(),
        }
    };
}
