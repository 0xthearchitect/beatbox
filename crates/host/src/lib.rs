//! The host sandbox: one QEMU guest, as a sandbox.
//!
//! An engine provider built on `beatbox-isolation`, and the only crate that
//! sees both: the engine can't see QEMU, and the compiler keeps it so. Held
//! to `golden/host/spec.json`.

pub mod environment;
pub mod probe;
pub mod sandbox;
mod settings;

pub use environment::{AGENT_RETRY_EVERY, Agent, ConnectAgent, Environment, SHUTDOWN_GRACE};
pub use sandbox::{
    CAPABILITIES, HostSandbox, actions, folder_size, gigabytes, interface, parse_ps, parse_tasklist,
};
