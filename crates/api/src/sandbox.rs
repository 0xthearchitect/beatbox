//! The sandbox runtime, as the page is allowed to see it.
//!
//! **Nothing here decides anything.** The runtime owns the rules; this turns
//! its objects into `contract.json` shapes and its errors into faults. A view
//! carries `transitions` so the page greys buttons by the runtime's word,
//! not a second copy of the table.

use beatbox_contract::{
    ActionFieldView, ActionOutcomeView, ActionSpecView, ReasonView, SandboxConfigView,
    SandboxEventView, SandboxStateView, SandboxSummary, SandboxTypeView, SandboxView,
};
use beatbox_engine::runtime::lifecycle::{CALLABLE, transitions_from};
use beatbox_engine::runtime::resources::{CPU, MEMORY};
use beatbox_engine::{
    ActionOutcome, ActionSpec, ContractError, EngineError, Event, Field, Reason, Resources,
    Sandbox, SandboxConfig, Transition,
};
use serde_json::{Map, Value};

use crate::fault::ApiFault;
use crate::{Api, py};

/// A reason, for the wire: its values as text, which they already are.
pub fn reason_view(reason: &Reason) -> ReasonView {
    ReasonView {
        code: reason.code.clone(),
        values: serde_json::to_value(&reason.values).expect("text is JSON"),
    }
}

fn reason_of(reason: Option<&Reason>) -> Option<ReasonView> {
    reason.map(reason_view)
}

/// An error nobody handled, as `internal` with `Type: text`.
pub(crate) fn uncaught(error: &EngineError) -> ApiFault {
    let kind = match error {
        EngineError::Refused(_) => "Refused",
        EngineError::Provider(_) => "ProviderError",
        EngineError::Contract(ContractError::UnknownSandbox(_)) => "UnknownSandbox",
        EngineError::Contract(_) => "ContractError",
    };
    ApiFault::new("internal", format!("{kind}: {error}"))
}

pub(crate) fn contract(error: &ContractError) -> ApiFault {
    uncaught(&EngineError::Contract(error.clone()))
}

fn to_wire<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("a contract type is JSON")
}

impl Api {
    /// The sandbox with this id, or the fault that says there is none.
    pub(crate) fn sandbox(&self, sandbox_id: &Value) -> Result<Sandbox, ApiFault> {
        self.runtime
            .get(&py::text_or_empty(sandbox_id))
            .map_err(|_| {
                ApiFault::new(
                    "no-sandbox",
                    format!("There is no sandbox {} here.", py::repr(sandbox_id)),
                )
            })
    }

    pub(crate) fn sandbox_types(&self) -> Result<Value, ApiFault> {
        let types: Vec<SandboxTypeView> = self
            .runtime
            .registry()
            .types()
            .into_iter()
            .map(|entry| SandboxTypeView {
                name: entry.name.clone(),
                version: entry.version.clone(),
                isolation: entry.isolation.as_str().to_owned(),
                capabilities: entry.capabilities.to_list(),
                actions: entry.spec().actions.iter().map(action_view).collect(),
            })
            .collect();
        Ok(to_wire(&types))
    }

    pub(crate) fn sandbox_list(&self) -> Result<Value, ApiFault> {
        let rows: Vec<SandboxSummary> = self
            .runtime
            .sandboxes()
            .iter()
            .map(|sandbox| {
                let state = sandbox.state();
                SandboxSummary {
                    id: sandbox.id(),
                    r#type: sandbox.type_name(),
                    name: sandbox.name(),
                    lifecycle: state.lifecycle.as_str().to_owned(),
                    since: state.since,
                }
            })
            .collect();
        Ok(to_wire(&rows))
    }

    pub(crate) fn sandbox_view(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let sandbox = self.sandbox(&payload["id"])?;
        self.view(&sandbox)
    }

    /// The one read that costs a round trip to the guest, apart from `view`
    /// so the page decides when it's worth it.
    pub(crate) fn sandbox_refresh(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let sandbox = self.sandbox(&payload["id"])?;
        match sandbox.refresh() {
            Ok(_) => {}
            Err(EngineError::Provider(error)) => {
                return Err(ApiFault::because(
                    "provider",
                    format!("{} could not be observed", sandbox.id()),
                    error.reason,
                ));
            }
            Err(error) => return Err(uncaught(&error)),
        }
        self.view(&sandbox)
    }

    /// Make one. `cpu` and `memory` sit beside `settings`, not in it: they're
    /// the runtime's, not the provider's.
    pub(crate) fn sandbox_create(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let config = self.sandbox_config(payload)?;
        match self.runtime.create(config) {
            Ok(sandbox) => self.view(&sandbox),
            Err(EngineError::Refused(why)) => Err(ApiFault::because(
                "refused",
                "This sandbox cannot be created",
                Some(why),
            )),
            Err(EngineError::Provider(error)) => Err(ApiFault::because(
                "provider",
                "This sandbox could not be created",
                error.reason,
            )),
            Err(EngineError::Contract(error)) => Err(ApiFault::new("bad-payload", error.message())),
        }
    }

    /// What `sandbox.create` would make of `payload`, for an install that
    /// makes its machine later (`disks.build`).
    pub(crate) fn sandbox_config(
        &self,
        payload: &Map<String, Value>,
    ) -> Result<SandboxConfig, ApiFault> {
        let resources = resources(&payload["cpu"], &payload["memory"])?;
        let mut settings = py::mapping(&payload["settings"])?;
        // `machines/`, never the temporary directory (tmpfs).
        if !settings.get("workspace").is_some_and(py::truthy) {
            settings.insert("workspace".into(), Value::String(self.workspace()));
        }
        let kind = py::text_or_empty(&payload["type"]);
        // The user's service library, unless the caller named one.
        if kind == "server" && !settings.get("definitions").is_some_and(py::truthy) {
            let library = self.paths.services.to_string_lossy().into_owned();
            settings.insert("definitions".into(), Value::String(library));
        }
        let mut config = SandboxConfig::new(&kind)
            .map_err(|error| contract(&error))?
            .named(&py::text_or_empty(&payload["name"]))
            .with_resources(resources)
            .with_settings(settings);
        for (role, target) in py::mapping(&payload["uses"])? {
            config = config.using(&role, &py::text_or_empty(&target));
        }
        Ok(config)
    }

    /// Start, stop, pause, resume, reset or destroy: one action for all six,
    /// because the lifecycle table is data and stays data.
    pub(crate) fn sandbox_transition(
        &self,
        payload: &Map<String, Value>,
    ) -> Result<Value, ApiFault> {
        let sandbox = self.sandbox(&payload["id"])?;
        let name = payload["transition"].as_str().unwrap_or_default();
        let Some(transition) = CALLABLE.iter().copied().find(|t| t.as_str() == name) else {
            let names: Vec<&str> = CALLABLE.iter().map(|t| t.as_str()).collect();
            return Err(ApiFault::new(
                "bad-payload",
                format!("A transition is one of: {}.", names.join(", ")),
            ));
        };
        match sandbox.apply(transition) {
            Ok(_) => {}
            Err(EngineError::Refused(why)) => {
                return Err(ApiFault::because(
                    "refused",
                    format!("{} cannot {name}", sandbox.name()),
                    Some(why),
                ));
            }
            Err(EngineError::Provider(error)) => {
                return Err(ApiFault::because(
                    "provider",
                    format!("{} could not {name}", sandbox.name()),
                    error.reason,
                ));
            }
            Err(error) => return Err(uncaught(&error)),
        }
        if transition == Transition::Start {
            // Every machine that can take the guest tools gets them at start.
            self.offer_tools(&sandbox);
        }
        self.view(&sandbox)
    }

    /// New processors, memory and settings for a machine that is off.
    /// `settings` is laid over the old ones: the page sends only what it
    /// edits, and the disk and workspace must survive.
    pub(crate) fn sandbox_reconfigure(
        &self,
        payload: &Map<String, Value>,
    ) -> Result<Value, ApiFault> {
        let sandbox = self.sandbox(&payload["id"])?;
        let current = sandbox.config();
        let resources = resources(&payload["cpu"], &payload["memory"])?;
        let mut settings = current.settings.clone();
        settings.extend(py::mapping(&payload["settings"])?);
        let config = SandboxConfig::new(current.type_name())
            .map_err(|error| contract(&error))?
            .named(&current.name)
            .with_resources(resources)
            .with_isolation(current.isolation.clone())
            .with_settings(settings);
        match sandbox.reconfigure(config) {
            Ok(_) => self.view(&sandbox),
            Err(EngineError::Refused(why)) => Err(ApiFault::because(
                "refused",
                format!("{} cannot be reconfigured", sandbox.name()),
                Some(why),
            )),
            Err(EngineError::Provider(error)) => Err(ApiFault::because(
                "provider",
                format!("{} could not be reconfigured", sandbox.name()),
                error.reason,
            )),
            Err(error) => Err(uncaught(&error)),
        }
    }

    /// Point a sandbox that is off at other sandboxes: the machine and the
    /// network a server works on. Checked and refused by the engine, whole.
    pub(crate) fn sandbox_use(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let sandbox = self.sandbox(&payload["id"])?;
        let uses: std::collections::BTreeMap<String, String> = py::mapping(&payload["uses"])?
            .into_iter()
            .map(|(role, target)| (role, py::text_or_empty(&target)))
            .collect();
        match self.runtime.reuse(&sandbox.id(), &uses) {
            Ok(_) => self.view(&sandbox),
            Err(EngineError::Refused(why)) => Err(ApiFault::because(
                "refused",
                format!("{} cannot be pointed there", sandbox.name()),
                Some(why),
            )),
            Err(error) => Err(uncaught(&error)),
        }
    }

    /// A new name for a machine or a network, in whatever state it is in.
    pub(crate) fn sandbox_rename(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let sandbox = self.sandbox(&payload["id"])?;
        sandbox.rename(&py::text_or_empty(&payload["name"]));
        self.view(&sandbox)
    }

    /// Ask a sandbox to do something it declared. Never a fault when the
    /// answer is no: that is an outcome with a reason on it.
    pub(crate) fn sandbox_act(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let sandbox = self.sandbox(&payload["id"])?;
        let arguments = py::mapping(&payload["arguments"])?;
        let outcome = sandbox
            .act(&py::text_or_empty(&payload["capability"]), arguments)
            .map_err(|error| contract(&error))?;
        Ok(to_wire(&outcome_view(&outcome)))
    }

    /// The backlog for a screen that just opened; the rest arrives as
    /// `sandbox` events.
    pub(crate) fn sandbox_events(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let since = py::int_or_zero(&payload["since"])?;
        let told: Vec<SandboxEventView> = self.history(since).iter().map(event_view).collect();
        Ok(to_wire(&told))
    }

    /// One sandbox, whole, without asking the provider anything.
    pub(crate) fn view(&self, sandbox: &Sandbox) -> Result<Value, ApiFault> {
        let identity = sandbox.identity();
        let config = sandbox.config();
        let state = sandbox.state();
        let isolation = self
            .runtime
            .registry()
            .get(&identity.type_name)
            .map_err(|error| uncaught(&error))?
            .isolation;
        let view = SandboxView {
            id: identity.id.clone(),
            r#type: identity.type_name.clone(),
            name: identity.name.clone(),
            version: identity.version.clone(),
            isolation: isolation.as_str().to_owned(),
            capabilities: sandbox.capabilities().to_list(),
            transitions: transitions_from(state.lifecycle)
                .iter()
                .map(|t| t.as_str().to_owned())
                .collect(),
            config: SandboxConfigView {
                r#type: config.type_name().to_owned(),
                name: config.name.clone(),
                resources: Value::Object(config.resources.to_data()),
                isolation: config.isolation.level.as_str().to_owned(),
                settings: Value::Object(config.settings.clone()),
                uses: serde_json::json!(config.uses),
            },
            state: SandboxStateView {
                lifecycle: state.lifecycle.as_str().to_owned(),
                since: state.since.clone(),
                reason: reason_of(state.reason.as_ref()),
                resources: Value::Object(state.resources.to_data()),
                facts: Value::Object(state.facts.clone()),
            },
        };
        Ok(to_wire(&view))
    }
}

pub(crate) fn outcome_view(outcome: &ActionOutcome) -> ActionOutcomeView {
    ActionOutcomeView {
        capability: outcome.capability().to_owned(),
        ok: outcome.ok,
        data: Value::Object(outcome.data.clone()),
        reason: reason_of(outcome.reason.as_ref()),
    }
}

/// One engine event, for the page.
pub(crate) fn event_view(event: &Event) -> SandboxEventView {
    SandboxEventView {
        name: event.name().to_owned(),
        sandbox_id: event.sandbox_id().to_owned(),
        at: event.at().to_owned(),
        sequence: i64::try_from(event.sequence()).unwrap_or(i64::MAX),
        data: Value::Object(event.data().clone()),
    }
}

/// `cpu` and `memory` as ledger amounts. Left out, the provider's default
/// applies.
fn resources(cpu: &Value, memory: &Value) -> Result<Resources, ApiFault> {
    let mut amounts = Vec::new();
    if py::truthy(cpu) {
        amounts.push((CPU, py::float(cpu)?));
    }
    if py::truthy(memory) {
        amounts.push((MEMORY, py::float(memory)?));
    }
    Resources::new(amounts).map_err(|error| contract(&error))
}

/// What one capability takes and gives back, as the page reads it.
fn action_view(spec: &ActionSpec) -> ActionSpecView {
    let fields = |fields: &[Field]| {
        fields
            .iter()
            .map(|field| ActionFieldView {
                name: field.name.clone(),
                kind: field.kind.as_str().to_owned(),
                required: field.required,
            })
            .collect()
    };
    ActionSpecView {
        capability: spec.capability.clone(),
        inputs: fields(&spec.inputs),
        outputs: fields(&spec.outputs),
    }
}
