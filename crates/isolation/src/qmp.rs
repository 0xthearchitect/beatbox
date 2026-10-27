//! Talking to a running guest: its monitor, and the agent inside it.
//!
//! **The monitor (QMP)** is QEMU's, outside the guest: it works before boot,
//! and nothing inside can refuse it or lie to it.
//!
//! **The guest agent (QGA)** runs inside, so it needs cooperation, is
//! missing on most disks, and a compromised guest can lie through it.
//!
//! Both speak line-delimited JSON, so they share a transport.
//!
//! **No lock inside:** a channel belongs to one provider, reached only
//! through its sandbox's lane. Message ids still drop the late answer to a
//! request that gave up waiting.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use beatbox_reasons::{Reason, reason};
use serde_json::{Map, Value, json};

use crate::base64;
use crate::endpoint::{self, Stream};
use crate::random;

/// How long to wait for a socket QEMU has not created yet.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one request may take before the channel is declared unusable.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a save point, going back to one, or forgetting one may take:
/// the whole memory (2.46 GB for a 3 GB Windows guest) with the guest
/// paused. Generous, because giving up early is worse.
pub const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(600);

/// The names the guest tools' drive goes by inside QEMU: see
/// [`Monitor::insert_media`].
pub const MEDIA_NODE: &str = "bbmedia";
/// See [`MEDIA_NODE`].
pub const MEDIA_DEVICE: &str = "bbmediadisk";
/// A pen somebody plugs in themselves: a slot of its own, so the tools'
/// drive -- plugged in at every windowed start -- never takes its place.
pub const PEN_NODE: &str = "bbpen";
/// See [`PEN_NODE`].
pub const PEN_DEVICE: &str = "bbpendisk";
/// How long a pulled pen's drive gets to be let go of by QEMU.
pub const EJECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How many unprompted events a channel keeps for whoever asks next.
const EVENTS_KEPT: usize = 256;

/// A channel could not be opened, or refused what was asked of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlError {
    /// Why, named.
    pub reason: Reason,
}

impl ControlError {
    /// A control error for `reason`.
    pub fn new(reason: Reason) -> Self {
        Self { reason }
    }

    /// The reason's code.
    pub fn code(&self) -> &str {
        &self.reason.code
    }
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.reason)
    }
}

impl std::error::Error for ControlError {}

fn gone(error: &std::io::Error) -> ControlError {
    let detail = if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) {
        "timed out".to_owned()
    } else {
        error.to_string()
    };
    ControlError::new(reason!("machine.channel_gone", detail = detail))
}

/// Line-delimited JSON over a socket, with a deadline on everything.
///
/// Small and synchronous: one request and its answer at a time. Events QEMU
/// pushes in between are kept, because "the guest shut itself down" arrives
/// that way.
pub struct Channel {
    stream: Stream,
    buffer: Vec<u8>,
    events: VecDeque<Value>,
    timeout: Duration,
    next_id: u64,
}

impl Channel {
    /// A channel over an already open stream, answering within `timeout`.
    pub fn new(stream: Stream, timeout: Duration) -> Self {
        let _ = stream.set_read_timeout(Some(timeout));
        let _ = stream.set_write_timeout(Some(timeout));
        Self {
            stream,
            buffer: Vec::new(),
            events: VecDeque::new(),
            timeout,
            next_id: 0,
        }
    }

    /// Open the socket, waiting for it to exist.
    ///
    /// QEMU creates it on start, so connecting right after spawn races it;
    /// retry until the deadline. Resolved each try: on Windows the port file
    /// is written just before QEMU starts.
    pub fn connect(path: &Path, timeout: Duration) -> Result<Self, ControlError> {
        let deadline = Instant::now() + timeout;
        loop {
            let last = match endpoint::connect(path, timeout) {
                Ok(stream) => return Ok(Self::new(stream, timeout)),
                Err(error) => error.to_string(),
            };
            if Instant::now() >= deadline {
                return Err(ControlError::new(reason!(
                    "machine.no_channel",
                    path = path.display(),
                    detail = last,
                )));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Send one request and read its answer, with nobody in between.
    ///
    /// `timeout` overrides the channel's for commands that take minutes
    /// (`savevm`). Giving up doesn't stop the command; its late answer is
    /// dropped by id instead of read as the next one's.
    pub fn exchange(
        &mut self,
        message: &Value,
        timeout: Option<Duration>,
    ) -> Result<Map<String, Value>, ControlError> {
        self.next_id += 1;
        let tag = format!("bb-{}", self.next_id);
        if let Some(timeout) = timeout {
            let _ = self.stream.set_read_timeout(Some(timeout));
        }
        let result = (|| {
            let mut request = message.as_object().cloned().unwrap_or_default();
            request.insert("id".into(), Value::String(tag.clone()));
            self.send(&Value::Object(request))?;
            loop {
                let mut answer = self.reply()?;
                // No id at all: a server that does not echo one. Taken as it
                // comes.
                let matches = match answer.get("id") {
                    None => true,
                    Some(id) => id.as_str() == Some(tag.as_str()),
                };
                if matches {
                    answer.remove("id");
                    return Ok(answer);
                }
            }
        })();
        if timeout.is_some() {
            let _ = self.stream.set_read_timeout(Some(self.timeout));
        }
        result
    }

    /// Throw away whatever an earlier client left in the stream.
    ///
    /// The agent's stream outlives connections, so an answer nobody waited
    /// for would be read as the next one's. `guest-sync-delimited` fixes it:
    /// 0xFF abandons a half-parsed request, then a sync with a random id comes
    /// back behind a 0xFF sentinel. Anything before it, or another id, is stale.
    pub fn resync(&mut self, timeout: Duration) -> bool {
        let token = (random::next() % (i32::MAX as u64 - 1)) + 1;
        let deadline = Instant::now() + timeout;
        let _ = self.stream.set_read_timeout(Some(timeout));
        let synced = (|| -> Result<bool, ControlError> {
            self.stream.write_all(&[0xff]).map_err(|e| gone(&e))?;
            self.send(&json!({"execute": "guest-sync-delimited", "arguments": {"id": token}}))?;
            while !self.buffer.contains(&0xff) {
                if Instant::now() >= deadline {
                    return Ok(false);
                }
                let mut chunk = [0u8; 65536];
                let read = self.stream.read(&mut chunk).map_err(|e| gone(&e))?;
                if read == 0 {
                    return Ok(false);
                }
                self.buffer.extend_from_slice(&chunk[..read]);
            }
            let last = self.buffer.iter().rposition(|b| *b == 0xff).unwrap_or(0);
            self.buffer.drain(..=last);
            while Instant::now() < deadline {
                let answer = self.reply()?;
                if answer.get("return").and_then(Value::as_u64) == Some(token) {
                    return Ok(true);
                }
            }
            Ok(false)
        })()
        .unwrap_or(false);
        let _ = self.stream.set_read_timeout(Some(self.timeout));
        synced
    }

    /// Write one message, as a line.
    pub fn send(&mut self, message: &Value) -> Result<(), ControlError> {
        let mut line = serde_json::to_vec(message).expect("a request is JSON");
        line.push(b'\n');
        self.stream.write_all(&line).map_err(|e| gone(&e))
    }

    /// One JSON object, or a failure. Never blocks past the timeout.
    pub fn receive(&mut self) -> Result<Map<String, Value>, ControlError> {
        while !self.buffer.contains(&b'\n') {
            let mut chunk = [0u8; 65536];
            let read = self.stream.read(&mut chunk).map_err(|e| gone(&e))?;
            if read == 0 {
                return Err(ControlError::new(reason!(
                    "machine.channel_gone",
                    detail = "closed"
                )));
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
        let end = self
            .buffer
            .iter()
            .position(|b| *b == b'\n')
            .expect("found above");
        let line: Vec<u8> = self.buffer.drain(..=end).collect();
        match serde_json::from_slice::<Value>(&line[..line.len() - 1]) {
            Ok(Value::Object(message)) => Ok(message),
            Ok(other) => Err(ControlError::new(reason!(
                "machine.channel_noise",
                detail = format!("not an object: {other}")
            ))),
            Err(error) => Err(ControlError::new(reason!(
                "machine.channel_noise",
                detail = error
            ))),
        }
    }

    /// The next *answer*, with anything pushed in the meantime kept aside.
    pub fn reply(&mut self) -> Result<Map<String, Value>, ControlError> {
        loop {
            let message = self.receive()?;
            if message.contains_key("event") {
                self.keep(Value::Object(message));
                continue;
            }
            return Ok(message);
        }
    }

    fn keep(&mut self, event: Value) {
        self.events.push_back(event);
        if self.events.len() > EVENTS_KEPT {
            self.events.pop_front();
        }
    }

    /// Everything that arrived unprompted since this was last asked.
    ///
    /// Read after a request rather than waited for: this channel does not own
    /// a thread, so an event only surfaces when somebody is talking anyway.
    pub fn drain_events(&mut self) -> Vec<Value> {
        self.events.drain(..).collect()
    }

    /// Let anything pending arrive for up to `seconds`, then hand over
    /// everything kept. Silence is the normal state of a guest that is simply
    /// running, so running out of time ends the wait rather than failing it.
    pub fn poll_events(&mut self, seconds: Duration) -> Vec<Value> {
        let deadline = Instant::now() + seconds;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() && !self.buffer.contains(&b'\n') {
                break;
            }
            let _ = self
                .stream
                .set_read_timeout(Some(remaining.max(Duration::from_millis(10))));
            match self.receive() {
                Ok(message) if message.contains_key("event") => self.keep(Value::Object(message)),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let _ = self.stream.set_read_timeout(Some(self.timeout));
        self.drain_events()
    }

    /// Close the socket. Closing one that already went is not an error.
    pub fn close(&mut self) {
        self.stream.shutdown();
    }

    /// The stream underneath, for a test that asks how long it waits.
    pub fn stream(&self) -> &Stream {
        &self.stream
    }
}

/// Send one command and return its `return` value, whatever shape it is.
///
/// Not coerced to an object: `guest-file-open` answers a bare integer,
/// `guest-network-get-interfaces` a list, `human-monitor-command` a string,
/// and `stop` nothing.
fn ask(
    channel: &mut Channel,
    message: Value,
    what: &str,
    timeout: Option<Duration>,
) -> Result<Value, ControlError> {
    let mut answer = channel.exchange(&message, timeout)?;
    if let Some(error) = answer.get("error") {
        let detail = match error.get("desc") {
            Some(Value::String(desc)) if !desc.is_empty() => desc.clone(),
            _ => error.to_string(),
        };
        return Err(ControlError::new(reason!(
            "machine.refused_command",
            command = what,
            detail = detail,
        )));
    }
    Ok(answer.remove("return").unwrap_or(Value::Null))
}

fn command(name: &str, arguments: Option<Value>) -> Value {
    match arguments {
        Some(arguments) if arguments.as_object().is_some_and(|a| !a.is_empty()) => {
            json!({"execute": name, "arguments": arguments})
        }
        _ => json!({"execute": name}),
    }
}

/// QEMU's own control channel. Outside the guest, and not negotiable.
///
/// Everything a host sandbox can promise *unconditionally* is here: whether
/// the machine is executing, freezing it, thawing it, snapshotting it, and
/// telling it to go. A guest cannot decline any of it and cannot pretend it
/// happened.
pub struct Monitor {
    channel: Channel,
}

impl Monitor {
    /// A monitor over an open channel whose greeting was already answered.
    pub fn new(channel: Channel) -> Self {
        Self { channel }
    }

    /// Open the monitor and get past its greeting.
    ///
    /// QMP refuses every command until `qmp_capabilities`, so it's sent here
    /// and no caller has to know.
    pub fn connect(path: &Path, timeout: Duration) -> Result<Self, ControlError> {
        let mut channel = Channel::connect(path, timeout)?;
        let greeting = channel.reply()?;
        if !greeting.contains_key("QMP") {
            channel.close();
            return Err(ControlError::new(reason!(
                "machine.channel_noise",
                detail = "no QMP greeting"
            )));
        }
        ask(
            &mut channel,
            json!({"execute": "qmp_capabilities"}),
            "qmp_capabilities",
            None,
        )?;
        Ok(Self { channel })
    }

    /// Any QMP command, and whatever it returned.
    pub fn execute(&mut self, name: &str, arguments: Option<Value>) -> Result<Value, ControlError> {
        ask(&mut self.channel, command(name, arguments), name, None)
    }

    /// `running`, `paused`, `shutdown`... QEMU's own word for it.
    pub fn status(&mut self) -> Result<String, ControlError> {
        let answer = self.execute("query-status", None)?;
        Ok(answer
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }

    /// Whether the guest's processors are executing.
    pub fn running(&mut self) -> Result<bool, ControlError> {
        Ok(self.status()? == "running")
    }

    /// Freeze.
    pub fn pause(&mut self) -> Result<(), ControlError> {
        self.execute("stop", None).map(drop)
    }

    /// Thaw.
    pub fn resume(&mut self) -> Result<(), ControlError> {
        self.execute("cont", None).map(drop)
    }

    /// What is on the guest's screen, written by QEMU to `path` as a PNG.
    pub fn screendump(&mut self, path: &Path) -> Result<(), ControlError> {
        self.execute(
            "screendump",
            Some(json!({"filename": path, "format": "png"})),
        )
        .map(drop)
    }

    /// The reset button: the guest boots again from its disk, memory lost.
    pub fn reset(&mut self) -> Result<(), ControlError> {
        self.execute("system_reset", None).map(drop)
    }

    /// Press the guest's power button (ACPI). A guest with an operating
    /// system that listens shuts itself down, and QEMU leaves with 0 when it
    /// has; one with nothing listening ignores it.
    pub fn power_button(&mut self) -> Result<(), ControlError> {
        self.execute("system_powerdown", None).map(drop)
    }

    /// Ask QEMU to exit. The polite half of stopping a guest.
    ///
    /// A channel dying mid-answer is success: QEMU may go before replying.
    pub fn quit(&mut self) -> Result<(), ControlError> {
        match self.execute("quit", None) {
            Ok(_) => Ok(()),
            Err(error) if error.code() == "machine.channel_gone" => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Run one human-monitor command and return what it printed.
    ///
    /// The escape hatch: `savevm` and `loadvm` have no QMP equivalent for a
    /// plain disk. Nothing else should use it.
    pub fn command_line(
        &mut self,
        text: &str,
        timeout: Option<Duration>,
    ) -> Result<String, ControlError> {
        let message =
            json!({"execute": "human-monitor-command", "arguments": {"command-line": text}});
        let printed = ask(&mut self.channel, message, "human-monitor-command", timeout)?;
        Ok(match printed {
            Value::Null => String::new(),
            Value::String(text) => text,
            other => other.to_string(),
        })
    }

    /// Plug `folder` into the guest as a read-only USB drive, now.
    ///
    /// `vvfat` shows the folder as a FAT disk without copying anything.
    /// Read-only at the block layer, so the guest can't write back and
    /// `savevm` skips it. Needs a screen's xHCI controller; `false` if it's
    /// already in.
    pub fn insert_media(&mut self, folder: &str, label: &str) -> Result<bool, ControlError> {
        self.plug(MEDIA_NODE, MEDIA_DEVICE, folder, label)
    }

    /// [`Monitor::insert_media`], into the pen's own slot.
    pub fn insert_pen(&mut self, folder: &str, label: &str) -> Result<bool, ControlError> {
        self.plug(PEN_NODE, PEN_DEVICE, folder, label)
    }

    /// Pull the pen out: the USB device first, then -- once QEMU has let go
    /// of it, which `device_del` only asks for -- the drive behind it. A pen
    /// that is not there is already out.
    pub fn eject_pen(&mut self) -> Result<(), ControlError> {
        if let Err(error) = self.execute("device_del", Some(json!({"id": PEN_DEVICE}))) {
            let detail = error
                .reason
                .values
                .get("detail")
                .cloned()
                .unwrap_or_default();
            if !detail.contains("not found") {
                return Err(error);
            }
        }
        let deadline = Instant::now() + EJECT_TIMEOUT;
        loop {
            match self.execute("blockdev-del", Some(json!({"node-name": PEN_NODE}))) {
                Ok(_) => return Ok(()),
                Err(error) => {
                    let detail = error
                        .reason
                        .values
                        .get("detail")
                        .cloned()
                        .unwrap_or_default();
                    if detail.contains("Failed to find") || detail.contains("not found") {
                        return Ok(());
                    }
                    if Instant::now() >= deadline {
                        return Err(error);
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    fn plug(
        &mut self,
        node: &str,
        device: &str,
        folder: &str,
        label: &str,
    ) -> Result<bool, ControlError> {
        let label: String = label.chars().take(11).collect();
        let added = self.execute(
            "blockdev-add",
            Some(json!({
                "driver": "vvfat",
                "node-name": node,
                "dir": folder,
                "label": label,
                "read-only": true,
            })),
        );
        if let Err(error) = added {
            let detail = error
                .reason
                .values
                .get("detail")
                .cloned()
                .unwrap_or_default();
            if detail.contains("Duplicate") || detail.contains("already") {
                return Ok(false);
            }
            return Err(error);
        }
        self.execute(
            "device_add",
            Some(json!({
                "driver": "usb-storage",
                "id": device,
                "drive": node,
                "bus": "usb0.0",
                "removable": true,
            })),
        )?;
        Ok(true)
    }

    /// Take a save point called `name`.
    pub fn snapshot_save(&mut self, name: &str) -> Result<(), ControlError> {
        let text = format!("savevm {}", tag(name)?);
        self.monitor_snapshot(&text, name, "machine.snapshot_failed")
    }

    /// Go back to the save point called `name`.
    ///
    /// QEMU puts the disks back first and reads the memory and the devices
    /// after, so a failure here can leave a guest paused with the save
    /// point's disk under memory that is not its own. What to do about that
    /// is the caller's: this only says it failed.
    pub fn snapshot_load(&mut self, name: &str) -> Result<(), ControlError> {
        let text = format!("loadvm {}", tag(name)?);
        self.monitor_snapshot(&text, name, "machine.snapshot_restore_failed")
    }

    /// Forget the save point called `name`.
    pub fn snapshot_delete(&mut self, name: &str) -> Result<(), ControlError> {
        let text = format!("delvm {}", tag(name)?);
        self.monitor_snapshot(&text, name, "machine.snapshot_delete_failed")
    }

    /// The human monitor prints failures instead of returning a QMP error,
    /// so checking only for errors records snapshots never taken. These say
    /// nothing on success; any output is a failure, under each verb's code.
    fn monitor_snapshot(&mut self, text: &str, name: &str, code: &str) -> Result<(), ControlError> {
        let printed = self.command_line(text, Some(SNAPSHOT_TIMEOUT))?;
        let printed = printed.trim();
        if !printed.is_empty() {
            return Err(ControlError::new(match code {
                "machine.snapshot_restore_failed" => {
                    reason!(
                        "machine.snapshot_restore_failed",
                        name = name,
                        detail = printed
                    )
                }
                "machine.snapshot_delete_failed" => {
                    reason!(
                        "machine.snapshot_delete_failed",
                        name = name,
                        detail = printed
                    )
                }
                _ => reason!("machine.snapshot_failed", name = name, detail = printed),
            }));
        }
        Ok(())
    }

    /// Whatever QEMU pushed, after waiting up to `seconds` for more.
    pub fn events(&mut self, seconds: Duration) -> Vec<Value> {
        self.channel.poll_events(seconds)
    }

    /// Close the channel.
    pub fn close(&mut self) {
        self.channel.close();
    }

    /// The channel underneath. For tests that break it on purpose.
    pub fn channel_mut(&mut self) -> &mut Channel {
        &mut self.channel
    }
}

/// A program running inside the guest, which is what makes it different.
///
/// Needs `qemu-guest-agent` running inside, and isn't to be believed about a
/// hostile guest: it's cooperative, and a compromised one cooperates
/// dishonestly.
pub struct GuestAgent {
    channel: Channel,
}

impl GuestAgent {
    /// How long to wait for a command started inside the guest.
    pub const RUN_TIMEOUT: Duration = Duration::from_secs(30);

    /// An agent over an open channel.
    pub fn new(channel: Channel) -> Self {
        Self { channel }
    }

    /// Open the agent's channel, waiting for it to exist.
    pub fn connect(path: &Path, timeout: Duration) -> Result<Self, ControlError> {
        Ok(Self::new(Channel::connect(path, timeout)?))
    }

    /// Whether an agent answers, on a stream cleared of anything stale.
    ///
    /// Asked before the first request on every new connection -- see
    /// [`Channel::resync`]. `false` is also the answer for "nothing inside is
    /// listening".
    pub fn sync(&mut self, timeout: Duration) -> bool {
        self.channel.resync(timeout)
    }

    /// Any agent command, and whatever it returned.
    pub fn execute(&mut self, name: &str, arguments: Option<Value>) -> Result<Value, ControlError> {
        ask(&mut self.channel, command(name, arguments), name, None)
    }

    /// Ask the guest to power itself off, from inside.
    ///
    /// No answer when it obeys (the agent goes down too); the caller watches
    /// the machine go.
    pub fn shutdown(&mut self) {
        let _ = ask(
            &mut self.channel,
            command("guest-shutdown", Some(json!({"mode": "powerdown"}))),
            "guest-shutdown",
            Some(Duration::from_secs(3)),
        );
    }

    /// Whether anything is listening inside.
    pub fn ping(&mut self) -> bool {
        self.execute("guest-ping", None).is_ok()
    }

    /// What the agent says about itself.
    pub fn info(&mut self) -> Result<Map<String, Value>, ControlError> {
        Ok(self
            .execute("guest-info", None)?
            .as_object()
            .cloned()
            .unwrap_or_default())
    }

    /// What the guest believes its interfaces are: the only place a DHCP
    /// lease is visible, so addresses are facts, not configuration.
    pub fn interfaces(&mut self) -> Result<Vec<Value>, ControlError> {
        Ok(match self.execute("guest-network-get-interfaces", None)? {
            Value::Array(interfaces) => interfaces,
            _ => Vec::new(),
        })
    }

    /// Run one program inside and wait for it, up to a deadline.
    ///
    /// Exit code and output, hiding the `guest-exec`/`guest-exec-status`
    /// dance. A command past the deadline is not killed: the agent can't,
    /// and saying otherwise would lie about something still running.
    pub fn run(
        &mut self,
        path: &str,
        arguments: &[Value],
        timeout: Option<Duration>,
    ) -> Result<Map<String, Value>, ControlError> {
        let started = self.execute(
            "guest-exec",
            Some(json!({"path": path, "arg": arguments, "capture-output": true})),
        )?;
        let pid = started.get("pid").and_then(Value::as_i64).unwrap_or(0);
        let deadline = Instant::now() + timeout.unwrap_or(Self::RUN_TIMEOUT);
        while Instant::now() < deadline {
            let status = self.execute("guest-exec-status", Some(json!({"pid": pid})))?;
            if status.get("exited").is_some_and(truthy) {
                let text = |key: &str| decode(status.get(key));
                let mut result = Map::new();
                result.insert("pid".into(), json!(pid));
                result.insert(
                    "exit_code".into(),
                    json!(status.get("exitcode").and_then(Value::as_i64).unwrap_or(0)),
                );
                result.insert("stdout".into(), json!(text("out-data")));
                result.insert("stderr".into(), json!(text("err-data")));
                result.insert(
                    "truncated".into(),
                    json!(
                        status.get("out-truncated").is_some_and(truthy)
                            || status.get("err-truncated").is_some_and(truthy)
                    ),
                );
                return Ok(result);
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err(ControlError::new(reason!(
            "machine.command_outlived_us",
            command = path,
            pid = pid,
        )))
    }

    /// Read a file inside the guest, up to `limit` bytes.
    ///
    /// Capped: it lands in our memory and the caller can't know its size.
    pub fn read_file(&mut self, path: &str, limit: u64) -> Result<String, ControlError> {
        let handle = self.open(path, "r")?;
        let mut collected = String::new();
        let result = (|| {
            let mut read = 0u64;
            while read < limit {
                let count = 65536.min(limit - read);
                let chunk = self.execute(
                    "guest-file-read",
                    Some(json!({"handle": handle, "count": count})),
                )?;
                collected.push_str(&decode(chunk.get("buf-b64")));
                read += chunk.get("count").and_then(Value::as_u64).unwrap_or(0);
                if chunk.get("eof").is_some_and(truthy) {
                    break;
                }
            }
            Ok(())
        })();
        let _ = self.execute("guest-file-close", Some(json!({"handle": handle})));
        result.map(|()| collected)
    }

    /// Write a file inside the guest, and say how much went in.
    pub fn write_file(&mut self, path: &str, content: &str) -> Result<i64, ControlError> {
        let handle = self.open(path, "w")?;
        let payload = base64::encode(content.as_bytes());
        let written = self.execute(
            "guest-file-write",
            Some(json!({"handle": handle, "buf-b64": payload})),
        );
        let _ = self.execute("guest-file-close", Some(json!({"handle": handle})));
        Ok(written?.get("count").and_then(Value::as_i64).unwrap_or(0))
    }

    /// Write `bytes` into a file inside the guest, in pieces the agent's
    /// channel carries comfortably, and say how many went in.
    ///
    /// For a program or sample: `write_file` sends one message, and the agent
    /// may refuse one the size of an installer.
    pub fn write_bytes(&mut self, path: &str, bytes: &[u8]) -> Result<u64, ControlError> {
        const PIECE: usize = 512 * 1024;
        let handle = self.open(path, "wb")?;
        let written = (|| {
            let mut total = 0u64;
            for piece in bytes.chunks(PIECE) {
                let answer = self.execute(
                    "guest-file-write",
                    Some(json!({"handle": handle, "buf-b64": base64::encode(piece)})),
                )?;
                total += answer.get("count").and_then(Value::as_u64).unwrap_or(0);
            }
            Ok(total)
        })();
        let _ = self.execute("guest-file-close", Some(json!({"handle": handle})));
        written
    }

    fn open(&mut self, path: &str, mode: &str) -> Result<i64, ControlError> {
        let handle = self.execute("guest-file-open", Some(json!({"path": path, "mode": mode})))?;
        handle.as_i64().ok_or_else(|| {
            ControlError::new(reason!(
                "machine.channel_noise",
                detail = format!("guest-file-open answered {handle}, not a handle"),
            ))
        })
    }

    /// Close the channel.
    pub fn close(&mut self) {
        self.channel.close();
    }
}

/// A snapshot name, as the human monitor has to be given it.
///
/// Quoted, since the monitor splits on whitespace (`savevm before the
/// attack` fails). A quote or backslash is refused, not escaped: the monitor
/// has no defined escaping, and guessing one is how a tag becomes a command.
pub fn tag(name: &str) -> Result<String, ControlError> {
    if name.chars().any(|c| matches!(c, '"' | '\\' | '\r' | '\n')) {
        return Err(ControlError::new(reason!(
            "machine.bad_snapshot_name",
            name = name
        )));
    }
    Ok(format!("\"{name}\""))
}

/// Base64 out of the agent, as text. Nothing, for nothing or nonsense.
fn decode(encoded: Option<&Value>) -> String {
    match encoded.and_then(Value::as_str) {
        Some(text) if !text.is_empty() => base64::decode(text)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Truthiness for the flags an agent answers with.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}
