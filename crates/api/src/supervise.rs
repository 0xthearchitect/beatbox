//! Every running Server is asked now and then to restart what stopped on
//! its own, if its settings say so (`service.supervise`). One that doesn't
//! costs a lookup every [`EVERY`].
//!
//! One thread for all, only in the BeatBox that owns the machines.

use std::sync::Weak;
use std::thread;
use std::time::Duration;

use beatbox_engine::{Lifecycle, SandboxRuntime};
use beatbox_server::ServerSandbox;

/// How often a Server is asked.
pub const EVERY: Duration = Duration::from_secs(20);

/// Ask, for as long as the runtime is there.
pub fn watch(runtime: Weak<SandboxRuntime>) {
    thread::Builder::new()
        .name("beatboxd-services".into())
        .spawn(move || {
            loop {
                thread::sleep(EVERY);
                let Some(runtime) = runtime.upgrade() else {
                    return;
                };
                for sandbox in runtime.sandboxes() {
                    if sandbox.lifecycle() != Lifecycle::Running
                        || !sandbox.capabilities().supports("service.supervise")
                    {
                        continue;
                    }
                    // Not through `act`: an idle look isn't worth an event.
                    let started = sandbox.provider(ServerSandbox::supervise_now);
                    if let Some(names) = started.filter(|names| !names.is_empty()) {
                        log::info!("{} started again: {}", sandbox.id(), names.join(", "));
                    }
                }
            }
        })
        .expect("a thread can be started");
}
