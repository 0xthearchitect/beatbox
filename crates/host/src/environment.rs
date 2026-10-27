//! What a host sandbox asks of the computer it runs on, handed in.
//!
//! Built once from the real computer ([`Environment::here`]) and swapped
//! piece by piece in tests: a display that's there, memory that isn't, an
//! agent that answers.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use beatbox_engine::Data;
use beatbox_isolation::firmware::{self, Uefi};
use beatbox_isolation::host_os;
use beatbox_isolation::machine::{self, Qemu};
use beatbox_isolation::qmp::{ControlError, GuestAgent};
use beatbox_isolation::runner::{Runner, SystemRunner};
use beatbox_isolation::uuid4;
use serde_json::Value;

/// How long before asking an agent-less guest again. The agent can start
/// minutes after the socket (end of boot, or installed later), but a failed
/// ask waits out its whole timeout and a screen observes every few seconds.
pub const AGENT_RETRY_EVERY: Duration = Duration::from_secs(15);

/// How long a guest gets to shut down before it's switched off. Windows
/// takes 10 to 30 s; a Linux desktop may show a dialog first.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(60);

/// The program inside a guest, as far as a host sandbox needs it.
///
/// A trait so a test can fake one that answers, or fails.
pub trait Agent: Send {
    /// Whether it answers, on a stream cleared of anything stale.
    fn sync(&mut self, timeout: Duration) -> bool;
    /// What the guest believes its network interfaces are.
    fn interfaces(&mut self) -> Result<Vec<Value>, ControlError>;
    /// Run one program inside and wait for it.
    fn run(
        &mut self,
        path: &str,
        arguments: &[Value],
        timeout: Option<Duration>,
    ) -> Result<Data, ControlError>;
    /// Read a file inside, up to a mebibyte.
    fn read_file(&mut self, path: &str) -> Result<String, ControlError>;
    /// Write a file inside.
    fn write_file(&mut self, path: &str, content: &str) -> Result<i64, ControlError>;
    /// What the guest says it is (`guest-get-osinfo`): `id` is `mswindows`
    /// on Windows, the distribution on Linux. `Null` from an agent that
    /// cannot say, which is read as Linux.
    fn os(&mut self) -> Result<Value, ControlError> {
        Ok(Value::Null)
    }
    /// Ask the guest to power itself off. Nothing to say back: whoever asked
    /// watches the machine go. An agent that cannot do it does nothing.
    fn shutdown(&mut self) {}
    /// Let go of the channel.
    fn close(&mut self);
}

impl Agent for GuestAgent {
    fn sync(&mut self, timeout: Duration) -> bool {
        GuestAgent::sync(self, timeout)
    }

    fn interfaces(&mut self) -> Result<Vec<Value>, ControlError> {
        GuestAgent::interfaces(self)
    }

    fn run(
        &mut self,
        path: &str,
        arguments: &[Value],
        timeout: Option<Duration>,
    ) -> Result<Data, ControlError> {
        GuestAgent::run(self, path, arguments, timeout)
    }

    fn read_file(&mut self, path: &str) -> Result<String, ControlError> {
        GuestAgent::read_file(self, path, 1 << 20)
    }

    fn write_file(&mut self, path: &str, content: &str) -> Result<i64, ControlError> {
        GuestAgent::write_file(self, path, content)
    }

    fn os(&mut self) -> Result<Value, ControlError> {
        self.execute("guest-get-osinfo", None)
    }

    fn shutdown(&mut self) {
        GuestAgent::shutdown(self);
    }

    fn close(&mut self) {
        GuestAgent::close(self);
    }
}

/// Finding this computer's UEFI firmware, as the tests stand in for it.
pub type FindUefi = Arc<dyn Fn(&Qemu) -> Option<Uefi> + Send + Sync>;

/// Opening an agent's channel.
pub type ConnectAgent =
    Arc<dyn Fn(&Path, Duration) -> Result<Box<dyn Agent>, ControlError> + Send + Sync>;

/// Everything a host sandbox asks of this computer.
#[derive(Clone)]
pub struct Environment {
    /// Where QEMU is and what accelerates it. Asked at every argv build.
    pub qemu: Arc<dyn Fn() -> Qemu + Send + Sync>,
    /// Whether this QEMU can serve BeatBox's VNC WebSocket display.
    pub display_available: Arc<dyn Fn(&Qemu) -> bool + Send + Sync>,
    /// The command that would give it one.
    pub install_hint: Arc<dyn Fn() -> String + Send + Sync>,
    /// How much memory can be handed out now without swapping.
    pub available_memory_mb: Arc<dyn Fn() -> Option<u64> + Send + Sync>,
    /// What runs `qemu-img`.
    pub runner: Arc<dyn Runner>,
    /// What opens an agent's channel.
    pub connect_agent: ConnectAgent,
    /// A fresh SMBIOS UUID, for every start.
    pub uuid: Arc<dyn Fn() -> String + Send + Sync>,
    /// See [`AGENT_RETRY_EVERY`].
    pub agent_retry_every: Duration,
    /// See [`SHUTDOWN_GRACE`].
    pub shutdown_grace: Duration,
    /// Where this computer's UEFI firmware is, if it has one.
    pub uefi: FindUefi,
}

impl Environment {
    /// This computer, as it is.
    pub fn here() -> Self {
        Self {
            qemu: Arc::new(Qemu::here),
            display_available: Arc::new(|qemu| machine::vnc_display_available(qemu, &SystemRunner)),
            install_hint: Arc::new(machine::display_install_hint),
            available_memory_mb: Arc::new(|| host_os::memory_mb().0),
            runner: Arc::new(SystemRunner),
            connect_agent: Arc::new(|path, timeout| {
                GuestAgent::connect(path, timeout).map(|agent| Box::new(agent) as Box<dyn Agent>)
            }),
            uuid: Arc::new(uuid4),
            agent_retry_every: AGENT_RETRY_EVERY,
            shutdown_grace: SHUTDOWN_GRACE,
            uefi: Arc::new(firmware::find_uefi),
        }
    }
}
