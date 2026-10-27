//! The one port a debugger inside the guest is reached through.
//!
//! `-netdev user,restrict=on` stops the guest reaching the host, but an
//! explicit `hostfwd` still reaches in (measured). So the channel runs host
//! to guest only, and attaching a debugger doesn't hand a sample a way out.
//! `frida-server` must already run in the image; BeatBox never installs it.

/// frida-server's default, forwarded on the same number so an operator
/// reading the argv sees something they recognise.
pub const FRIDA_PORT: u16 = 27042;

/// The one port the host is allowed to reach in on: `(host, guest)`.
pub fn instrumentation_forward(port: u16) -> (u16, u16) {
    (port, FRIDA_PORT)
}
