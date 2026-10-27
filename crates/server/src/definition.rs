//! What a service is, written as data: the Service Definition.
//!
//! How to run a service, what it listens on and is configured with, how to
//! tell it's well and what it can be asked. Nothing here reads what it's
//! for: `http-static`, `dns` and a stranger's service on 3610 are three
//! values of one type.
//!
//! **Checked on load, not on start.** [`Definition::parse`] refuses with the
//! first thing wrong (`server.bad_definition`), so a typo like `{prot}` is
//! found here, not on a guest's command line.

use std::collections::{BTreeMap, BTreeSet};

use beatbox_engine::names::validate;
use beatbox_engine::{ActionSpec, Data, Field, Kind, Reason, reason};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::placeholders::{fill, placeholders};

/// The internal implementations this build has. A definition naming
/// another is refused when it is loaded.
pub const INTERNALS: &[&str] = &["dns"];

/// What a secret is shown as, everywhere but the service itself.
pub const SECRET_SHOWN: &str = "••••";

/// One service, as its author described it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    /// What it is called among the others: lowercase, digits and `-`.
    pub id: String,
    /// The definition's own version, recorded in each instance.
    pub version: String,
    /// What a person calls it.
    pub name: String,
    /// What it does, in a sentence.
    #[serde(default)]
    pub description: String,
    /// Who wrote it.
    #[serde(default)]
    pub author: String,
    /// What runs it.
    pub runtime: Runtime,
    /// The programs it needs where it runs, by name on the PATH or by an
    /// absolute path: read, never run, to say before a start whether a
    /// machine has them (`server.check`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// What it is configured with, by name.
    #[serde(default)]
    pub configuration: BTreeMap<String, Setting>,
    /// What it listens on.
    #[serde(default)]
    pub interfaces: Vec<Interface>,
    /// Files written where it runs before it starts.
    #[serde(default)]
    pub files: Vec<FileSpec>,
    /// Bounded prerequisite check after files are written and before launch.
    /// Exit 0 to proceed; JSON Reason on stdout can explain a refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepare: Option<Vec<String>>,
    /// How to tell it is well. None: its health is never more than
    /// `unknown`.
    #[serde(default)]
    pub health: Option<Health>,
    /// What it can be asked to do through `service.call`.
    #[serde(default)]
    pub capabilities: Vec<ServiceCapability>,
}

/// What runs a service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Runtime {
    /// A program inside the Host the Server uses.
    Process {
        /// The program and its arguments, as argv: never a shell line.
        command: Vec<String>,
        /// Where it starts; `/` when not said.
        #[serde(default)]
        directory: Option<String>,
    },
    /// Code in this crate, on the Server's own node on the switch.
    Internal {
        /// Which one, from [`INTERNALS`].
        implementation: String,
    },
}

/// One configuration key's shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    /// What it holds.
    pub kind: SettingKind,
    /// What it is when not given. None: it has to be given.
    #[serde(default)]
    pub default: Option<Value>,
    /// The allowed values, for an `enum`.
    #[serde(default)]
    pub values: Vec<String>,
}

/// What a configuration key holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    /// Text -- or a number or yes/no read as text, as an action's `Text` is.
    Text,
    /// Any number.
    Number,
    /// Yes or no.
    Bool,
    /// One of `values`.
    Enum,
    /// Text that is never shown back.
    Secret,
}

impl SettingKind {
    /// The word a definition and a refusal use for it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Bool => "bool",
            Self::Enum => "enum",
            Self::Secret => "secret",
        }
    }

    fn fits(self, value: &Value) -> bool {
        match self {
            Self::Text => value.is_string() || value.is_number() || value.is_boolean(),
            Self::Number => value.is_number(),
            Self::Bool => value.is_boolean(),
            Self::Enum => value.is_string(),
            Self::Secret => value.is_string() || shared_secret(value).is_some(),
        }
    }
}

/// One thing a service listens on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interface {
    /// TCP or UDP: all the Server itself ever uses.
    pub transport: Transport,
    /// A number, or text with placeholders that becomes one.
    pub port: Port,
    /// `http`, `dns`, `postgresql`: said. Acted on once: a Server
    /// sharing a certificate makes it for a service that says `https`.
    #[serde(default)]
    pub protocol: Option<String>,
}

/// TCP or UDP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// TCP.
    Tcp,
    /// UDP.
    Udp,
}

impl Transport {
    /// `tcp` or `udp`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

/// A port as a definition writes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Port {
    /// `8000`.
    Number(u16),
    /// `"{port}"`.
    Text(String),
}

impl Port {
    /// The port, with `values` in its placeholders, or `None` when that is
    /// not a port.
    pub fn resolve(&self, values: &BTreeMap<String, String>) -> Option<u16> {
        match self {
            Self::Number(port) => Some(*port),
            Self::Text(text) => fill(text, values).trim().parse().ok(),
        }
        .filter(|port| *port > 0)
    }

    /// Set to 0: the interface is turned off, which is not a bad port.
    pub fn is_off(&self, values: &BTreeMap<String, String>) -> bool {
        match self {
            Self::Number(port) => *port == 0,
            Self::Text(text) => fill(text, values).trim() == "0",
        }
    }
}

/// A file written where the service runs, before it starts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSpec {
    /// Where, with placeholders.
    pub path: String,
    /// What, with placeholders.
    pub content: String,
}

/// How to tell a service is well.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Health {
    /// Something accepts a connection on `port`.
    Tcp {
        /// Where.
        port: Port,
    },
    /// `GET path` on `port` answers 2xx or 3xx.
    Http {
        /// Where.
        port: Port,
        /// What is asked for.
        #[serde(default = "root")]
        path: String,
    },
    /// This argv exits 0.
    Command {
        /// The program and its arguments.
        command: Vec<String>,
    },
    /// The internal implementation says.
    Internal,
}

fn root() -> String {
    "/".to_owned()
}

/// One thing a service can be asked to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceCapability {
    /// `namespace.verb`, as every capability is.
    pub capability: String,
    /// What it takes.
    #[serde(default)]
    pub inputs: Vec<FieldDef>,
    /// What it gives back.
    #[serde(default)]
    pub outputs: Vec<FieldDef>,
    /// For a `process` service, the argv that does it, with the inputs and
    /// the configuration as placeholders. Never for an `internal` one.
    #[serde(default)]
    pub run: Option<Vec<String>>,
}

/// One value a service capability takes or gives back, as JSON says it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldDef {
    /// Its name.
    pub name: String,
    /// `text`, `number`, `bool`, `list`, `object` or `any`.
    pub kind: String,
    /// Whether it has to be there. Yes unless said.
    #[serde(default = "yes")]
    pub required: bool,
}

fn yes() -> bool {
    true
}

fn kind_of(word: &str) -> Option<Kind> {
    [
        Kind::Text,
        Kind::Number,
        Kind::Bool,
        Kind::List,
        Kind::Object,
        Kind::Any,
    ]
    .into_iter()
    .find(|kind| kind.as_str() == word)
}

impl FieldDef {
    fn field(&self) -> Field {
        let kind = kind_of(&self.kind).unwrap_or(Kind::Any);
        if self.required {
            Field::required(&self.name, kind)
        } else {
            Field::optional(&self.name, kind)
        }
    }
}

/// `[a-z][a-z0-9_]*`: a configuration key, an input, a placeholder.
pub fn is_key(text: &str) -> bool {
    let mut characters = text.chars();
    characters.next().is_some_and(|c| c.is_ascii_lowercase())
        && characters.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_id(text: &str) -> bool {
    let mut characters = text.chars();
    text.len() <= 40
        && characters.next().is_some_and(|c| c.is_ascii_lowercase())
        && characters.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Placeholders every instance has besides its configuration. `_tls` is
/// the folder holding its HTTPS certificate (`certificate.pem`, `key.pem`):
/// its own, or the one its Server shares.
pub const OWN: [&str; 3] = ["_name", "_id", "_tls"];

/// The shared secret a secret setting names instead of holding a value:
/// `{"shared": "db_password"}`.
pub fn shared_secret(value: &Value) -> Option<&str> {
    let object = value.as_object().filter(|object| object.len() == 1)?;
    object.get("shared")?.as_str().filter(|name| is_key(name))
}

impl Definition {
    /// Read one from JSON and check it, or the first thing wrong with it.
    pub fn parse(text: &str) -> Result<Self, Reason> {
        let raw: Value = serde_json::from_str(text).map_err(|error| bad("?", error))?;
        Self::from_value(&raw)
    }

    /// The same, from JSON already read -- an inline definition in a
    /// `service.create`.
    pub fn from_value(raw: &Value) -> Result<Self, Reason> {
        let id = raw.get("id").and_then(Value::as_str).unwrap_or("?");
        let definition: Self =
            serde_json::from_value(raw.clone()).map_err(|error| bad(id, error))?;
        definition.check()?;
        Ok(definition)
    }

    /// Whether it runs inside this crate rather than in a guest.
    pub fn is_internal(&self) -> bool {
        matches!(self.runtime, Runtime::Internal { .. })
    }

    /// What `capability` takes and gives back, if this service has it.
    pub fn spec(&self, capability: &str) -> Option<ActionSpec> {
        self.capabilities
            .iter()
            .find(|each| each.capability == capability)
            .map(|each| {
                ActionSpec::new(
                    &each.capability,
                    each.inputs.iter().map(FieldDef::field).collect(),
                    each.outputs.iter().map(FieldDef::field).collect(),
                )
            })
    }

    /// Every capability's spec, in the order written.
    pub fn specs(&self) -> Vec<ActionSpec> {
        self.capabilities
            .iter()
            .filter_map(|each| self.spec(&each.capability))
            .collect()
    }

    /// `given`, checked against the schema and completed with the defaults
    /// -- or the first key that is wrong, for the service called `service`.
    ///
    /// An unknown key is refused, not ignored: a misspelt key ignored is a
    /// configuration that doesn't do what it says.
    pub fn configure(&self, service: &str, given: &Data) -> Result<Data, Reason> {
        if let Some(key) = given
            .keys()
            .find(|key| !self.configuration.contains_key(*key))
        {
            return Err(reason!(
                "server.config_unknown",
                service = service,
                key = key
            ));
        }
        let mut config = Data::new();
        for (key, setting) in &self.configuration {
            let value = match given.get(key).filter(|value| !value.is_null()) {
                Some(value) => value.clone(),
                None => match &setting.default {
                    Some(value) => value.clone(),
                    None => {
                        return Err(reason!(
                            "server.config_missing",
                            service = service,
                            key = key
                        ));
                    }
                },
            };
            if !setting.kind.fits(&value) {
                return Err(reason!(
                    "server.config_wrong_kind",
                    service = service,
                    key = key,
                    kind = setting.kind.as_str(),
                ));
            }
            if setting.kind == SettingKind::Enum
                && !setting
                    .values
                    .iter()
                    .any(|allowed| Some(allowed.as_str()) == value.as_str())
            {
                return Err(reason!(
                    "server.config_not_allowed",
                    service = service,
                    key = key,
                    values = setting.values.join(", "),
                ));
            }
            config.insert(key.clone(), value);
        }
        Ok(config)
    }

    /// What every placeholder becomes for one instance: its configuration
    /// as text, and `_name`, `_id` and its own `_tls`. A shared secret is
    /// left empty: only its Server knows it.
    pub fn values(&self, name: &str, id: &str, config: &Data) -> BTreeMap<String, String> {
        let mut values: BTreeMap<String, String> = config
            .iter()
            .map(|(key, value)| {
                let text = match shared_secret(value) {
                    Some(_) => String::new(),
                    None => as_text(value),
                };
                (key.clone(), text)
            })
            .collect();
        values.insert("_name".into(), name.to_owned());
        values.insert("_id".into(), id.to_owned());
        values.insert("_tls".into(), format!("/etc/beatbox/{id}/tls"));
        values
    }

    /// The values of `config` that are secrets, as their placeholders write
    /// them; empty ones left out, since an empty text hides nothing.
    pub fn secrets(&self, config: &Data) -> Vec<String> {
        config
            .iter()
            .filter(|(key, _)| {
                self.configuration
                    .get(*key)
                    .is_some_and(|setting| setting.kind == SettingKind::Secret)
            })
            .filter_map(|(_, value)| value.as_str())
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// `config` as it may be shown: every secret as [`SECRET_SHOWN`].
    pub fn masked(&self, config: &Data) -> Data {
        config
            .iter()
            .map(|(key, value)| {
                let secret = self
                    .configuration
                    .get(key)
                    .is_some_and(|setting| setting.kind == SettingKind::Secret);
                // Which shared secret it uses is no secret.
                let shown = if secret && shared_secret(value).is_none() {
                    Value::from(SECRET_SHOWN)
                } else {
                    value.clone()
                };
                (key.clone(), shown)
            })
            .collect()
    }

    /// What it will do where it runs, to be read before it is trusted with
    /// a start: every argv it can run and every file it writes (paths, not
    /// contents), placeholders as written.
    pub fn runs(&self) -> Value {
        let (command, directory) = match &self.runtime {
            Runtime::Process { command, directory } => (Some(command), directory.as_deref()),
            Runtime::Internal { .. } => (None, None),
        };
        let check = match &self.health {
            Some(Health::Command { command }) => Some(command),
            _ => None,
        };
        let calls: Vec<Value> = self
            .capabilities
            .iter()
            .filter_map(|each| {
                let run = each.run.as_ref()?;
                Some(serde_json::json!({"capability": each.capability, "run": run}))
            })
            .collect();
        serde_json::json!({
            "command": command,
            "directory": directory,
            "prepare": self.prepare,
            "files": self.files.iter().map(|file| &file.path).collect::<Vec<_>>(),
            "health_command": check,
            "capabilities": calls,
        })
    }

    /// Everything that can be known without running it.
    fn check(&self) -> Result<(), Reason> {
        let wrong = |problem: String| Err(bad(&self.id, problem));
        if let Some(command) = &self.prepare {
            if self.is_internal() {
                return wrong("only a process service has a preparation command".into());
            }
            if command
                .first()
                .is_none_or(|program| program.trim().is_empty())
            {
                return wrong("preparation needs a command".into());
            }
        }
        if let Some(odd) = self
            .requires
            .iter()
            .find(|program| program.is_empty() || program.chars().any(char::is_whitespace))
        {
            return wrong(format!("{odd:?} is not a program it can require"));
        }
        if !is_id(&self.id) {
            return wrong("its id must be lowercase letters, digits and '-', at most 40".into());
        }
        if self.version.trim().is_empty() || self.name.trim().is_empty() {
            return wrong("it needs a version and a name".into());
        }
        match &self.runtime {
            Runtime::Process { command, .. } => {
                if command
                    .first()
                    .is_none_or(|program| program.trim().is_empty())
                {
                    return wrong("a process runtime needs a command".into());
                }
            }
            Runtime::Internal { implementation } => {
                if !INTERNALS.contains(&implementation.as_str()) {
                    return wrong(format!(
                        "there is no internal implementation called {implementation}"
                    ));
                }
                if self
                    .interfaces
                    .iter()
                    .any(|each| each.transport == Transport::Tcp)
                {
                    return Err(reason!("server.internal_udp_only", definition = self.id));
                }
            }
        }
        for (key, setting) in &self.configuration {
            if !is_key(key) {
                return wrong(format!(
                    "configuration key {key:?} must be lowercase letters, digits and '_'"
                ));
            }
            match (setting.kind, &setting.default) {
                (SettingKind::Enum, _) if setting.values.is_empty() => {
                    return wrong(format!("{key} is an enum with no values"));
                }
                (SettingKind::Secret, Some(_)) => {
                    return wrong(format!("{key} is a secret, and a secret has no default"));
                }
                (kind, Some(value)) if !kind.fits(value) => {
                    return wrong(format!("{key}'s default is not a {}", kind.as_str()));
                }
                (SettingKind::Enum, Some(value))
                    if !setting
                        .values
                        .iter()
                        .any(|v| Some(v.as_str()) == value.as_str()) =>
                {
                    return wrong(format!("{key}'s default is not one of its values"));
                }
                _ => {}
            }
        }
        let known: BTreeSet<&str> = self
            .configuration
            .keys()
            .map(String::as_str)
            .chain(OWN)
            .collect();
        for text in self.templated() {
            if let Some(orphan) = placeholders(text).into_iter().find(|p| !known.contains(p)) {
                return wrong(format!("{{{orphan}}} names nothing it is configured with"));
            }
        }
        for interface in &self.interfaces {
            if let Port::Number(0) = interface.port {
                return wrong("port 0 is not a port".into());
            }
            if let Port::Text(text) = &interface.port
                && placeholders(text).is_empty()
                && text.trim().parse::<u16>().is_err()
            {
                return wrong(format!("{text:?} is not a port"));
            }
        }
        match (&self.runtime, &self.health) {
            (Runtime::Internal { .. }, Some(health)) if *health != Health::Internal => {
                return wrong("an internal service's health is its implementation's to say".into());
            }
            (Runtime::Process { .. }, Some(Health::Internal)) => {
                return wrong("only an internal service has internal health".into());
            }
            (_, Some(Health::Command { command })) if command.is_empty() => {
                return wrong("a command health check needs a command".into());
            }
            _ => {}
        }
        let mut named = BTreeSet::new();
        for each in &self.capabilities {
            let capability = &each.capability;
            if validate(capability, "a service capability").is_err() {
                return wrong(format!("{capability:?} must read like 'namespace.verb'"));
            }
            if ["service.", "server.", "lifecycle."]
                .iter()
                .any(|kept| capability.starts_with(kept))
            {
                return wrong(format!(
                    "{capability} is in a namespace the Server keeps for itself"
                ));
            }
            if !named.insert(capability.as_str()) {
                return wrong(format!("{capability} is described twice"));
            }
            let mut inputs = BTreeSet::new();
            for field in each.inputs.iter().chain(&each.outputs) {
                if !is_key(&field.name) || kind_of(&field.kind).is_none() {
                    return wrong(format!(
                        "{capability}: {:?} is not a field this can read",
                        field.name
                    ));
                }
            }
            for field in &each.inputs {
                if !inputs.insert(field.name.as_str()) {
                    return wrong(format!("{capability} takes {} twice", field.name));
                }
                if self.configuration.contains_key(&field.name) {
                    return wrong(format!(
                        "{capability}'s input {} is also a setting: {{{}}} would mean two things",
                        field.name, field.name
                    ));
                }
            }
            match (&self.runtime, &each.run) {
                (Runtime::Process { .. }, None) => {
                    return wrong(format!(
                        "{capability} needs a run: a process service does it with a command"
                    ));
                }
                (Runtime::Process { .. }, Some(run)) => {
                    if run.first().is_none_or(|program| program.trim().is_empty()) {
                        return wrong(format!("{capability}'s run needs a command"));
                    }
                    let known: BTreeSet<&str> = known.iter().copied().chain(inputs).collect();
                    for text in run {
                        if let Some(orphan) =
                            placeholders(text).into_iter().find(|p| !known.contains(p))
                        {
                            return wrong(format!(
                                "{capability}: {{{orphan}}} names nothing it is given"
                            ));
                        }
                    }
                }
                (Runtime::Internal { .. }, Some(_)) => {
                    return wrong(format!(
                        "{capability}: an internal service does it in code, not with a run"
                    ));
                }
                (Runtime::Internal { .. }, None) => {}
            }
        }
        Ok(())
    }

    /// The folders that belong to one instance alone, placeholders as
    /// written: every absolute path it names, cut just after a `{_id}`
    /// segment. `/var/lib/postgresql/beatbox/{_id}/x` gives
    /// `/var/lib/postgresql/beatbox/{_id}`. What a definition means is never
    /// read; only `{_id}`, which no other instance has, makes a folder its own.
    pub fn own_folders(&self) -> Vec<String> {
        let mut texts = self.templated();
        for each in &self.capabilities {
            texts.extend(each.run.iter().flatten().map(String::as_str));
        }
        let mut found: Vec<String> = texts
            .into_iter()
            .filter(|text| text.starts_with('/'))
            .filter_map(|text| {
                let segments: Vec<&str> = text.split('/').collect();
                let at = segments.iter().position(|segment| *segment == "{_id}")?;
                Some(segments[..=at].join("/"))
            })
            .collect();
        found.sort();
        found.dedup();
        found
    }

    /// Whether placeholder `key` is in any text it runs or writes.
    pub fn uses(&self, key: &str) -> bool {
        self.templated()
            .into_iter()
            .any(|text| placeholders(text).contains(&key))
    }

    /// Whether, configured with `values`, it says it answers HTTPS: an
    /// interface of that protocol on a port that is on. The Server makes a
    /// shared certificate only for these.
    pub fn answers_https(&self, values: &BTreeMap<String, String>) -> bool {
        self.interfaces.iter().any(|each| {
            each.protocol.as_deref() == Some("https") && each.port.resolve(values).is_some()
        })
    }

    /// Every text a placeholder may sit in, but a capability's `run`.
    fn templated(&self) -> Vec<&str> {
        let mut texts: Vec<&str> = Vec::new();
        if let Some(command) = &self.prepare {
            texts.extend(command.iter().map(String::as_str));
        }
        if let Runtime::Process { command, directory } = &self.runtime {
            texts.extend(command.iter().map(String::as_str));
            texts.extend(directory.as_deref());
        }
        for interface in &self.interfaces {
            if let Port::Text(text) = &interface.port {
                texts.push(text);
            }
        }
        for file in &self.files {
            texts.push(&file.path);
            texts.push(&file.content);
        }
        match &self.health {
            Some(Health::Tcp {
                port: Port::Text(text),
            }) => texts.push(text),
            Some(Health::Http { port, path }) => {
                if let Port::Text(text) = port {
                    texts.push(text);
                }
                texts.push(path);
            }
            Some(Health::Command { command }) => texts.extend(command.iter().map(String::as_str)),
            _ => {}
        }
        texts
    }
}

/// A configuration value as a placeholder writes it: `8000`, not `8000.0`.
pub fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number
            .as_i64()
            .map(|n| n.to_string())
            .or_else(|| number.as_u64().map(|n| n.to_string()))
            .unwrap_or_else(|| number.to_string()),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn bad(definition: &str, problem: impl std::fmt::Display) -> Reason {
    reason!(
        "server.bad_definition",
        definition = definition,
        problem = problem
    )
}
