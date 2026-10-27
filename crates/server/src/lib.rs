//! The server sandbox.
//!
//! ```text
//!   Server sandbox ── ServiceManager ── services.json
//!         │
//!   Service Instance ◄── Service Definition (data: runtime, configuration,
//!         │                interfaces, files, health, capabilities)
//!   runtime: process  -> a program in the Host it uses
//!   runtime: internal -> code here, on its own node on the switch
//! ```
//!
//! **The Server provides what it takes to run a service and knows nothing of
//! what any service is for.** What a service means belongs to its
//! [`Definition`]. Nothing in this crate branches on a service's id, name or
//! protocol; the two registries that look like exceptions -- the internal
//! implementations and the kinds of health check -- are tables, not `if`s.

pub mod builtins;
pub mod definition;
pub mod internal;
pub mod node;
pub mod placeholders;
pub mod process;
mod sandbox;

pub use builtins::builtins;
pub use definition::Definition;
pub use sandbox::{
    CAPABILITIES, HOST_NEEDS, NETWORK_NEEDS, START_PERIOD, ServerSandbox, State, Wellness, actions,
};
