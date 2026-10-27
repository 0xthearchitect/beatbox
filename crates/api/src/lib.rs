//! The daemon's half of the facade: one door in, one envelope out.
//!
//! What belongs to the person at the screen (their files, language, the
//! closing question) is the shell's. Held to `golden/contract/` and
//! `golden/api/`.
//!
//! **No globals.** Everything is one [`Api`]; the daemon builds one and each
//! test builds its own in its own folder. Dropping it lets go of the owner's
//! lock and stops nothing: guests outlive the daemon.
//!
//! **Only one BeatBox drives the machines.** QEMU's monitor takes one client,
//! so a second BeatBox would read a live guest as gone and overwrite its
//! record. The first to open takes a lock beside the records; any other
//! restores, sweeps and records nothing.
//!
//! This crate can't see the shell, and mustn't.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use beatbox_contract::{AnsweredBy, ApiError, Envelope, door};
use beatbox_engine::{
    ContractError, Event, Lifecycle, ProviderSpec, Recorder, RuntimeParts, Sandbox, SandboxRuntime,
};
use beatbox_host::HostSandbox;
use beatbox_isolation::host_os;
use beatbox_network::NetworkSandbox;
use beatbox_server::ServerSandbox;
use serde_json::{Map, Value};

pub mod beatdrop;
pub mod commands;
pub mod disks;
pub mod events;
pub mod fault;
pub mod foreign;
pub mod guest_tools;
pub mod jobs;
pub mod paths;
pub mod py;
pub mod records;
pub mod sandbox;
pub mod setup;
pub mod storage;
pub mod supervise;
pub mod system;
pub mod watchers;

mod leftovers;

pub use disks::{Guest, Installer, Launch};
pub use events::Hub;
pub use fault::ApiFault;
pub use guest_tools::{Fetch, Pinned, Tools};
pub use jobs::Jobs;
pub use paths::Paths;
pub use setup::Setup;

/// How many events are kept for a screen that opens late and asks for
/// everything since 0.
pub const HISTORY: usize = 500;

/// What the page is told when a sandbox says anything.
pub const SANDBOX_EVENT: &str = "sandbox";

/// What time it is, the way every record and event writes it.
pub fn now() -> String {
    beatbox_engine::clock::now()
}

/// Where each new machine's room is: a folder name under `machines/`.
pub type Workspace = Arc<dyn Fn() -> String + Send + Sync>;

/// What an [`Api`] is built from. Everything left out is this computer's.
pub struct ApiParts {
    /// Where things are kept.
    pub paths: Paths,
    /// What can be built. Explicit, never discovered: a file dropped in a
    /// folder must not become a capability.
    pub types: Vec<ProviderSpec>,
    /// The runtime's clock, ids and ledger; its registry is `types`.
    pub runtime: RuntimeParts,
    /// Where a new machine lives. Default: `machines/sbx-<12 hex>`.
    pub workspace: Option<Workspace>,
    /// What an install needs from this computer.
    pub installer: Installer,
    /// The guest tools.
    pub tools: Tools,
    /// What BeatBox needs from this computer, and what fixes it.
    pub setup: Setup,
}

impl Default for ApiParts {
    fn default() -> Self {
        Self {
            paths: Paths::here(),
            types: vec![
                HostSandbox::spec(),
                NetworkSandbox::spec(),
                ServerSandbox::spec(),
            ],
            runtime: RuntimeParts::default(),
            workspace: None,
            installer: Installer::default(),
            tools: Tools::default(),
            setup: Setup::default(),
        }
    }
}

/// The facade: every action the daemon answers, and what they share.
pub struct Api {
    paths: Paths,
    runtime: Arc<SandboxRuntime>,
    owner: Mutex<Option<File>>,
    history: Recorder,
    hub: Hub,
    jobs: Jobs,
    install: Arc<disks::Install>,
    capture: Arc<beatdrop::Capture>,
    installer: Installer,
    tools: Tools,
    setup: Setup,
    workspace: Option<Workspace>,
    /// Held while a kept list (commands) is read, changed and written.
    keep: Mutex<()>,
    watchers: Arc<watchers::Watchers>,
}

impl Api {
    /// Build the facade, and with it the application's runtime.
    ///
    /// Registers the types and listens. If this BeatBox owns the machines,
    /// it then records every change, restores what the records name and
    /// sweeps unclaimed rooms, in that order.
    pub fn open(parts: ApiParts) -> Self {
        let ApiParts {
            paths,
            types,
            runtime,
            workspace,
            installer,
            tools,
            setup,
        } = parts;
        let runtime = Arc::new(SandboxRuntime::new(runtime));
        for spec in types {
            if let Err(error) = runtime.register(spec) {
                log::error!("BeatBox could not register a sandbox type: {error}");
            }
        }
        let history = Recorder::new(HISTORY);
        let hub = Hub::default();
        let events = runtime.events();
        events
            .subscribe("*", history.sink())
            .expect("* is a pattern");
        let page = hub.clone();
        events
            .subscribe("*", move |event| {
                let view = sandbox::event_view(event);
                page.publish(
                    SANDBOX_EVENT,
                    &serde_json::to_value(view).expect("an event is JSON"),
                );
            })
            .expect("* is a pattern");

        let watching = watchers::Watchers::open(paths.root.join(watchers::FILE));
        let api = Self {
            jobs: Jobs::new(hub.clone()),
            keep: Mutex::new(()),
            watchers: watching,
            paths,
            runtime,
            owner: Mutex::new(None),
            history,
            hub,
            install: Arc::default(),
            capture: Arc::default(),
            installer,
            tools,
            setup,
            workspace,
        };
        if api.claim() {
            beatdrop::sweep_partials(&api.paths.disks);
            let weak = Arc::downgrade(&api.runtime);
            let state = api.paths.state.clone();
            api.runtime
                .events()
                .subscribe("sandbox.*", move |event| {
                    records::persist(&weak, &state, event);
                })
                .expect("sandbox.* is a pattern");
            records::restore_all(&api.runtime, &api.paths.state);
            api.sweep_leftovers(&leftovers::claimed(&api.runtime));
            // Only the BeatBox that owns the machines runs their watchers: two
            // would run every line twice.
            api.watchers.watch(Arc::downgrade(&api.runtime));
            supervise::watch(Arc::downgrade(&api.runtime));
        } else {
            log::warn!(
                "BeatBox: another BeatBox owns {}; this one restores, sweeps and records nothing",
                api.paths.state.display()
            );
        }
        api
    }

    /// Become the one BeatBox that owns the machines here, or not. Held until
    /// dropped or the process ends; the kernel lets go even on a crash.
    pub fn claim(&self) -> bool {
        let mut owner = self.owner.lock().unwrap_or_else(PoisonError::into_inner);
        if owner.is_some() {
            return true;
        }
        if fs::create_dir_all(&self.paths.state).is_err() {
            return false;
        }
        // Created if missing, never truncated.
        let Ok(file) = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(self.paths.state.join(".owner"))
        else {
            return false;
        };
        if !host_os::lock_exclusively(&file) {
            return false;
        }
        *owner = Some(file);
        true
    }

    /// Whether this is the BeatBox that owns the machines.
    pub fn owns(&self) -> bool {
        self.owner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// Where things are kept.
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// The application's runtime.
    pub fn runtime(&self) -> &Arc<SandboxRuntime> {
        &self.runtime
    }

    /// Where the page's events are published.
    pub fn hub(&self) -> &Hub {
        &self.hub
    }

    /// The jobs.
    pub fn jobs(&self) -> &Jobs {
        &self.jobs
    }

    /// The watchers.
    pub fn watchers(&self) -> &watchers::Watchers {
        &self.watchers
    }

    fn keeping(&self) -> std::sync::MutexGuard<'_, ()> {
        self.keep.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Make one more type buildable, and bring back its sandboxes.
    ///
    /// Restores again, or that type's sandboxes on disk would be missed.
    pub fn register(&self, spec: ProviderSpec) -> Result<(), ContractError> {
        self.runtime.register(spec)?;
        if self.owns() {
            records::restore_all(&self.runtime, &self.paths.state);
        }
        Ok(())
    }

    /// A room for one new machine.
    pub fn workspace(&self) -> String {
        match &self.workspace {
            Some(make) => make(),
            None => leftovers::workspace(&self.paths.machines),
        }
    }

    /// Remove what machines from an earlier run left behind, and nothing
    /// else, sparing the rooms in `claimed`. Names what went.
    pub fn sweep_leftovers(&self, claimed: &BTreeSet<PathBuf>) -> Vec<String> {
        leftovers::sweep(&self.paths.machines, &self.paths.temp, claimed)
    }

    /// Everything said after `since`, for a screen that has just opened.
    pub fn history(&self, since: i64) -> Vec<Event> {
        self.history
            .events()
            .into_iter()
            .filter(|event| i128::from(event.sequence()) > i128::from(since))
            .collect()
    }

    /// Every sandbox running or paused: the window's "is anything still
    /// going?". Created and never started doesn't count.
    pub fn running(&self) -> Vec<Sandbox> {
        self.runtime
            .sandboxes()
            .into_iter()
            .filter(|sandbox| matches!(sandbox.lifecycle(), Lifecycle::Running | Lifecycle::Paused))
            .collect()
    }

    /// Stop everything that is going, newest first, and name what wouldn't.
    /// Doesn't give up at the first stubborn guest.
    pub fn stop_running(&self) -> Vec<String> {
        let mut stubborn = Vec::new();
        for sandbox in self.running().into_iter().rev() {
            if let Err(error) = sandbox.stop() {
                log::error!("BeatBox could not stop {}: {error}", sandbox.id());
                stubborn.push(sandbox.id());
            }
        }
        stubborn
    }

    /// Run one action and answer with the envelope `contract.json` declares.
    ///
    /// Never panics across the socket: a failure without an envelope is a
    /// blank screen. A shell action is refused by name, not as unknown.
    pub fn dispatch(&self, action: &str, payload: &Value) -> Envelope {
        let (spec, payload) = match door::admit(action, payload) {
            Ok(admitted) => admitted,
            Err(refusal) => return door::refused(refusal),
        };
        if spec.answered_by == AnsweredBy::Shell {
            return door::refused(door::refusal(
                "not-here",
                format!("{action} is answered by the BeatBox window, not by beatboxd"),
            ));
        }
        match catch_unwind(AssertUnwindSafe(|| self.handle(spec.name, &payload))) {
            Ok(Ok(data)) => door::answered(data),
            Ok(Err(fault)) => door::refused(ApiError {
                code: fault.code,
                message: fault.message,
                reason: fault.reason.as_ref().map(sandbox::reason_view),
            }),
            Err(payload) => {
                let detail = payload
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "an action panicked".into());
                log::error!("BeatBox API action failed: {action}: {detail}");
                door::refused(door::refusal("internal", format!("panic: {detail}")))
            }
        }
    }

    fn handle(&self, action: &str, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        match action {
            "system.probe" => self.system_probe(),
            "jobs.current" => Ok(serde_json::to_value(self.jobs.current()).expect("JSON")),
            "disks.list" => self.disks_list(),
            "disks.build" => self.disks_build(payload),
            "disks.finish" => self.disks_finish(),
            "disks.forget" => self.disks_forget(payload),
            "disks.inspect" => self.disks_inspect(payload),
            "disks.unpack" => self.disks_unpack(payload),
            "beatdrop.inspect" => self.beatdrop_inspect(),
            "beatdrop.start" => self.beatdrop_start(payload),
            "beatdrop.folders" => self.beatdrop_folders(payload),
            "beatdrop.measure" => self.beatdrop_measure(payload),
            "beatdrop.cancel" => self.beatdrop_cancel(),
            "tools.status" => self.tools_status(),
            "tools.fetch" => self.tools_fetch(),
            "tools.insert" => self.tools_insert(payload),
            "setup.status" => self.setup_status(),
            "setup.fix" => self.setup_fix(payload),
            "storage.usage" => self.storage_usage(),
            "storage.sweep" => self.storage_sweep(),
            "sandbox.types" => self.sandbox_types(),
            "sandbox.list" => self.sandbox_list(),
            "sandbox.view" => self.sandbox_view(payload),
            "sandbox.refresh" => self.sandbox_refresh(payload),
            "sandbox.create" => self.sandbox_create(payload),
            "sandbox.transition" => self.sandbox_transition(payload),
            "sandbox.act" => self.sandbox_act(payload),
            "sandbox.rename" => self.sandbox_rename(payload),
            "sandbox.reconfigure" => self.sandbox_reconfigure(payload),
            "sandbox.use" => self.sandbox_use(payload),
            "sandbox.events" => self.sandbox_events(payload),
            "commands.list" => self.commands_list(),
            "commands.save" => self.commands_save(payload),
            "commands.forget" => self.commands_forget(payload),
            "watchers.list" => self.watchers_list(),
            "watchers.save" => self.watchers_save(payload),
            "watchers.forget" => self.watchers_forget(payload),
            other => Err(ApiFault::new(
                "internal",
                format!("beatboxd has no handler for {other}"),
            )),
        }
    }
}

/// What a window on another contract gets for every action: an upgrade in
/// progress, named, so the page can say it in the reader's language.
pub fn wrong_contract(ours: u32, theirs: u32) -> ApiError {
    let why = beatbox_engine::reason!("daemon.wrong_contract", ours = ours, theirs = theirs);
    ApiError {
        code: "wrong-contract".into(),
        message: format!(
            "beatboxd speaks contract {ours}, this window {theirs}; ask it to exit, and start the one that came with the window"
        ),
        reason: Some(sandbox::reason_view(&why)),
    }
}

/// Every action this half answers: what `contract.json` says the daemon
/// does, checked against [`Api::dispatch`] by the tests.
pub const HANDLED: [&str; 37] = [
    "system.probe",
    "storage.usage",
    "storage.sweep",
    "jobs.current",
    "disks.list",
    "disks.build",
    "disks.finish",
    "disks.forget",
    "disks.inspect",
    "disks.unpack",
    "beatdrop.inspect",
    "beatdrop.start",
    "beatdrop.folders",
    "beatdrop.measure",
    "beatdrop.cancel",
    "tools.status",
    "tools.fetch",
    "tools.insert",
    "setup.status",
    "setup.fix",
    "sandbox.types",
    "sandbox.list",
    "sandbox.view",
    "sandbox.refresh",
    "sandbox.create",
    "sandbox.transition",
    "sandbox.act",
    "sandbox.rename",
    "sandbox.reconfigure",
    "sandbox.use",
    "sandbox.events",
    "commands.list",
    "commands.save",
    "commands.forget",
    "watchers.list",
    "watchers.save",
    "watchers.forget",
];
