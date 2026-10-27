//! The `internal` runtime: services this crate runs itself.
//!
//! Each answers UDP on its ports, does its own capabilities and says what to
//! remember across a restart. [`make`] is the one table in this crate that
//! names a service, so no branch anywhere else has to.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use beatbox_engine::{Data, Reason, data, reason};
use beatbox_frame::{dns_answer, dns_question};
use serde_json::{Value, json};

/// One running internal service.
pub trait Internal: Send {
    /// The answer to `data`, sent to UDP `port`, if it has one.
    fn udp(&mut self, port: u16, data: &[u8]) -> Option<Vec<u8>>;
    /// Do one of its own capabilities, its inputs already checked.
    fn call(&mut self, capability: &str, arguments: &Data) -> Result<Data, Reason>;
    /// What it keeps across a restart, written into `services.json`.
    fn memory(&self) -> Value;
    /// Names it gives addresses to, for the network's own table too.
    fn names(&self) -> BTreeMap<String, Ipv4Addr> {
        BTreeMap::new()
    }
}

/// The implementation called `name`, picking up from `memory`.
pub fn make(name: &str, memory: &Value) -> Option<Box<dyn Internal>> {
    match name {
        "dns" => Some(Box::new(Dns::new(memory))),
        _ => None,
    }
}

/// A names server: A records it is given, NXDOMAIN for the rest.
#[derive(Debug, Default)]
pub struct Dns {
    records: BTreeMap<String, Ipv4Addr>,
}

/// `Web.Lab.` -> `web.lab`.
fn normal(name: &str) -> String {
    name.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn text<'a>(arguments: &'a Data, key: &str) -> &'a str {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

impl Dns {
    fn new(memory: &Value) -> Self {
        let records = memory["records"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(name, address)| Some((name.clone(), address.as_str()?.parse().ok()?)))
            .collect();
        Self { records }
    }
}

impl Internal for Dns {
    fn udp(&mut self, _port: u16, data: &[u8]) -> Option<Vec<u8>> {
        let question = dns_question(data)?;
        let address = self.records.get(&question.name).copied();
        Some(dns_answer(
            &question,
            address.is_some(),
            address.map(Into::into),
        ))
    }

    fn call(&mut self, capability: &str, arguments: &Data) -> Result<Data, Reason> {
        let name = normal(text(arguments, "name"));
        if name.is_empty() || name.split('.').any(str::is_empty) {
            return Err(reason!("server.dns_bad_name", name = name));
        }
        match capability {
            "dns.record.add" => {
                let given = text(arguments, "address");
                let address: Ipv4Addr = given
                    .trim()
                    .parse()
                    .map_err(|_| reason!("server.dns_bad_address", address = given))?;
                self.records.insert(name.clone(), address);
                Ok(data!({"name": name, "address": address.to_string()}))
            }
            "dns.record.remove" => match self.records.remove(&name) {
                Some(_) => Ok(data!({"name": name})),
                None => Err(reason!("server.dns_no_such_name", name = name)),
            },
            "dns.resolve" => {
                let address = self.records.get(&name).map(ToString::to_string);
                Ok(data!({"name": name, "address": address}))
            }
            other => Err(reason!("engine.unsupported", capability = other)),
        }
    }

    fn memory(&self) -> Value {
        let records: serde_json::Map<String, Value> = self
            .records
            .iter()
            .map(|(name, address)| (name.clone(), json!(address.to_string())))
            .collect();
        json!({"records": records})
    }

    fn names(&self) -> BTreeMap<String, Ipv4Addr> {
        self.records.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::INTERNALS;

    #[test]
    fn every_internal_a_definition_may_name_exists() {
        for name in INTERNALS {
            assert!(make(name, &Value::Null).is_some(), "{name}");
        }
    }

    #[test]
    fn records_are_kept_and_answered() {
        let mut dns = make("dns", &Value::Null).unwrap();
        dns.call(
            "dns.record.add",
            &data!({"name": "Web.Lab.", "address": "10.10.10.10"}),
        )
        .unwrap();
        let again = make("dns", &dns.memory()).unwrap();
        let mut again = again;
        let said = again
            .call("dns.resolve", &data!({"name": "web.lab"}))
            .unwrap();
        assert_eq!(said["address"], "10.10.10.10");
        let wrong = again.call("dns.record.add", &data!({"name": "x", "address": "ten"}));
        assert_eq!(wrong.unwrap_err().code, "server.dns_bad_address");
        let gone = again.call("dns.record.remove", &data!({"name": "nope"}));
        assert_eq!(gone.unwrap_err().code, "server.dns_no_such_name");
    }
}
