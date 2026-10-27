//! The host's side of a machine: where QEMU is, how to start it, and how to
//! talk to it once it is running.
//!
//! The host OS ([`host_os`]), control endpoints ([`endpoint`]), QMP and the
//! guest agent ([`qmp`]), the machine and its argv ([`machine`]), short
//! programs ([`runner`]), the pcap reader ([`capture`]) and the debugger's
//! forward ([`instrument`]). The argv is held flag for flag to `golden/qemu/`.
//!
//! Knows nothing of the engine; `deny.toml` keeps it that way.

pub mod base64;
pub mod capture;
pub mod endpoint;
pub mod firmware;
pub mod host_os;
pub mod instrument;
pub mod machine;
pub mod qmp;
mod random;
pub mod runner;

pub use random::uuid4;
