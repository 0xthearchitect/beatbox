//! The network sandbox.
//!
//! ```text
//!        Host Sandbox            Host Sandbox
//!             |  -netdev dgram        |
//!             +--------+     +--------+
//!                      v     v
//!                 switch.sock (one thread)
//!                      |
//!     port security -> gateway (ARP, DHCP, DNS, ping) -> firewall
//!                      |
//!           latency, jitter, loss, bandwidth -> the other node
//! ```
//!
//! A host knows only that it holds a cable: frames as datagrams, from its
//! `port` to a `switch`. This crate knows only frames. Neither sees the
//! other's code -- the crate graph does not let it -- and what passes
//! between them is the [`Data`](beatbox_engine::Data) `network.attach`
//! returns.

/// The frames this switch reads and writes, shared with the Server's
/// internal services.
pub use beatbox_frame as frame;
pub mod capture;
pub mod light;
pub mod renumber;
#[cfg(unix)]
mod router;
mod sandbox;
pub mod switch;
pub mod topology;

pub use sandbox::{CAPABILITIES, NetworkSandbox, actions, baseline};
