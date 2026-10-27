//! What a sandbox asks for, what it is using, and the ledger between them.
//!
//! Five kinds, fixed units, a ledger that adds up. The runtime knows four
//! `cpu` were promised and six are left; what four CPUs mean (pinning, a
//! `-smp` flag) is the provider's business.
//!
//! [`Resources`] is both what was asked for and what is used, so the two
//! compare directly. Units come from the kind and are never parsed: memory
//! is MiB, so nobody guesses which gigabyte `8GB` meant.

use std::collections::BTreeMap;
use std::fmt;

use beatbox_reasons::reason;
use serde_json::{Number, Value};

use crate::events::Data;
use crate::faults::{ContractError, EngineError, repr};

/// One of the five kinds, in the order a reader looks at them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResourceKind {
    /// Processors, fractional. `0.5` is half of one.
    Cpu,
    /// Working memory, in mebibytes.
    Memory,
    /// Disk, in mebibytes.
    Storage,
    /// Network attachments -- interfaces, taps, veth pairs. A count.
    Network,
    /// Anything passed through from the host: a GPU, a USB dongle. A count.
    Device,
}

/// Processors, fractional.
pub const CPU: ResourceKind = ResourceKind::Cpu;
/// Working memory, in mebibytes.
pub const MEMORY: ResourceKind = ResourceKind::Memory;
/// Disk, in mebibytes.
pub const STORAGE: ResourceKind = ResourceKind::Storage;
/// Network attachments, a count.
pub const NETWORK: ResourceKind = ResourceKind::Network;
/// Devices passed through, a count.
pub const DEVICE: ResourceKind = ResourceKind::Device;

/// Every kind, in declaration order.
pub const RESOURCE_KINDS: [ResourceKind; 5] = [CPU, MEMORY, STORAGE, NETWORK, DEVICE];

impl ResourceKind {
    /// The word a configuration and a record use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "memory",
            Self::Storage => "storage",
            Self::Network => "network",
            Self::Device => "device",
        }
    }

    /// What each number means, for the sentence a refusal writes.
    pub fn unit(self) -> &'static str {
        match self {
            Self::Cpu => "cores",
            Self::Memory | Self::Storage => "MiB",
            Self::Network => "interfaces",
            Self::Device => "devices",
        }
    }

    /// A kind from its word, or a broken contract naming the ones there are.
    pub fn parse(text: &str) -> Result<Self, ContractError> {
        RESOURCE_KINDS
            .into_iter()
            .find(|kind| kind.as_str() == text)
            .ok_or_else(|| {
                ContractError::broken(format!(
                    "not a resource kind: {} (have cpu, memory, storage, network, device)",
                    repr(text)
                ))
            })
    }
}

impl fmt::Display for ResourceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `4.0` -> `4`, `0.5` -> `0.5`. Whole numbers read as whole.
fn amount(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// `4 cores`, and `1 core`. Used by refusals, so it reads as English.
///
/// A person reads these, and "1 cores" makes them trust the rest less.
pub fn describe(kind: ResourceKind, value: f64) -> String {
    let mut unit = kind.unit();
    if value == 1.0 {
        unit = unit.strip_suffix('s').unwrap_or(unit);
    }
    format!("{} {unit}", amount(value))
}

/// An amount of each kind, with everything unmentioned meaning none.
///
/// Zero is dropped rather than stored, so asking for no CPU and not asking
/// are the same request.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Resources {
    amounts: BTreeMap<ResourceKind, f64>,
}

/// The empty request, named so it reads as a decision, not an oversight.
pub const NOTHING: Resources = Resources {
    amounts: BTreeMap::new(),
};

impl Resources {
    /// These amounts, or a broken contract for one that is negative (or not
    /// a number at all).
    pub fn new(
        amounts: impl IntoIterator<Item = (ResourceKind, f64)>,
    ) -> Result<Self, ContractError> {
        let mut cleaned = BTreeMap::new();
        for (kind, value) in amounts {
            // NaN too: it can't be added up in a ledger.
            if value.is_nan() || value < 0.0 {
                return Err(ContractError::broken(format!(
                    "{kind} cannot be negative: {value}"
                )));
            }
            if value != 0.0 {
                cleaned.insert(kind, value);
            }
        }
        Ok(Self { amounts: cleaned })
    }

    /// Amounts that are known to be valid: the result of arithmetic on two
    /// that already were.
    fn from_valid(amounts: impl IntoIterator<Item = (ResourceKind, f64)>) -> Self {
        Self {
            amounts: amounts.into_iter().filter(|(_, v)| *v != 0.0).collect(),
        }
    }

    /// How much of `kind`: none is zero.
    pub fn get(&self, kind: ResourceKind) -> f64 {
        self.amounts.get(&kind).copied().unwrap_or(0.0)
    }

    /// Whatever was asked for, in declaration order, so a refusal names the
    /// CPU first.
    pub fn kinds(&self) -> Vec<ResourceKind> {
        self.amounts.keys().copied().collect()
    }

    /// Whether this is the empty request.
    pub fn is_empty(&self) -> bool {
        self.amounts.is_empty()
    }

    /// Both together.
    #[must_use]
    pub fn plus(&self, other: &Resources) -> Resources {
        Self::from_valid(RESOURCE_KINDS.map(|k| (k, self.get(k) + other.get(k))))
    }

    /// Never below zero: a negative ledger means nothing to anyone.
    #[must_use]
    pub fn minus(&self, other: &Resources) -> Resources {
        Self::from_valid(RESOURCE_KINDS.map(|k| (k, (self.get(k) - other.get(k)).max(0.0))))
    }

    /// Whether every kind fits within the budget.
    pub fn fits_within(&self, budget: &Resources) -> bool {
        RESOURCE_KINDS
            .into_iter()
            .all(|kind| self.get(kind) <= budget.get(kind))
    }

    /// `{"cpu": 4.0, "memory": 8192.0}`.
    pub fn to_data(&self) -> Data {
        self.amounts
            .iter()
            .map(|(kind, value)| {
                let number = Number::from_f64(*value).expect("amounts are finite");
                (kind.as_str().to_owned(), Value::Number(number))
            })
            .collect()
    }

    /// Read back what [`Resources::to_data`] wrote, or a broken contract for
    /// a kind nobody declared or an amount that is not a number.
    pub fn from_value(raw: &Value) -> Result<Self, ContractError> {
        let Some(map) = raw.as_object() else {
            return Ok(NOTHING);
        };
        let mut amounts = Vec::new();
        for (kind, value) in map {
            let kind = ResourceKind::parse(kind)?;
            let number = match value {
                Value::Number(number) => number.as_f64(),
                Value::String(text) => text.trim().parse::<f64>().ok(),
                Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
                _ => None,
            }
            .ok_or_else(|| ContractError::broken(format!("{kind} is not a number: {value}")))?;
            amounts.push((kind, number));
        }
        Self::new(amounts)
    }
}

/// Who has been promised what, and whether there is any left.
///
/// Book-keeping, not enforcement: it lets a runtime with a budget refuse a
/// sandbox before creating half of it. No budget means unbounded, the
/// default, since a limit BeatBox can't verify would be fiction.
#[derive(Debug, Clone, Default)]
pub struct ResourceLedger {
    /// The most there is to give, if anything limits it.
    pub budget: Option<Resources>,
    held: Vec<(String, Resources)>,
}

impl ResourceLedger {
    /// A ledger that refuses what `budget` cannot cover.
    pub fn with_budget(budget: Resources) -> Self {
        Self {
            budget: Some(budget),
            held: Vec::new(),
        }
    }

    /// Promise `request` to `sandbox_id`, or refuse and promise nothing.
    ///
    /// The refusal names the first kind that does not fit, in declaration
    /// order, so the answer is stable.
    pub fn allocate(
        &mut self,
        sandbox_id: &str,
        request: &Resources,
    ) -> Result<Resources, EngineError> {
        if self.held.iter().any(|(id, _)| id == sandbox_id) {
            return Err(
                ContractError::broken(format!("{sandbox_id} already holds an allocation")).into(),
            );
        }
        if let Some(budget) = &self.budget {
            let free = budget.minus(&self.committed());
            for kind in request.kinds() {
                if request.get(kind) > free.get(kind) {
                    return Err(EngineError::Refused(reason!(
                        "engine.over_budget",
                        resource = kind.unit(),
                        requested = describe(kind, request.get(kind)),
                        available = describe(kind, free.get(kind)),
                    )));
                }
            }
        }
        self.held.push((sandbox_id.to_owned(), request.clone()));
        Ok(request.clone())
    }

    /// Give back whatever this sandbox held. Twice is fine: a repeated
    /// destroy shouldn't fail.
    pub fn release(&mut self, sandbox_id: &str) -> Resources {
        match self.held.iter().position(|(id, _)| id == sandbox_id) {
            Some(index) => self.held.remove(index).1,
            None => NOTHING,
        }
    }

    /// What this sandbox holds.
    pub fn allocated(&self, sandbox_id: &str) -> Resources {
        self.held
            .iter()
            .find(|(id, _)| id == sandbox_id)
            .map(|(_, held)| held.clone())
            .unwrap_or(NOTHING)
    }

    /// Everything promised, to everyone.
    pub fn committed(&self) -> Resources {
        self.held
            .iter()
            .fold(NOTHING, |total, (_, held)| total.plus(held))
    }

    /// What is left, or `None` with no budget.
    pub fn available(&self) -> Option<Resources> {
        self.budget
            .as_ref()
            .map(|budget| budget.minus(&self.committed()))
    }

    /// Who holds something, in the order they were given it.
    pub fn holders(&self) -> Vec<String> {
        self.held.iter().map(|(id, _)| id.clone()).collect()
    }
}
