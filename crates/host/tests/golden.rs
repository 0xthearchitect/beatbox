//! A host sandbox's configuration, turned into a machine, against
//! `golden/host/spec.json`: ten configurations with the capabilities each
//! keeps, the `MachineSpec` (or refusal) and the QEMU argv. Recorded as this
//! test runs: build the provider, put the overlay beside it, ask for the spec.

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use beatbox_engine::runtime::resources::{CPU, MEMORY};
use beatbox_engine::{Emitter, EventBus, Resources, SandboxConfig, SandboxIdentity};
use beatbox_host::{Environment, HostSandbox};
use beatbox_isolation::machine::build_command;
use serde_json::{Value, json};
use support::{Room, fake_disc, qemu};

const UUID: &str = "00000000-0000-4000-8000-00000000b0b0";

fn golden(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../golden")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// `value`, with `change` applied to every string in it.
fn strings(value: &Value, change: &dyn Fn(&str) -> String) -> Value {
    match value {
        Value::String(text) => Value::String(change(text)),
        Value::Array(items) => Value::Array(items.iter().map(|v| strings(v, change)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (change(k), strings(v, change)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[test]
fn every_configuration_becomes_the_machine_the_python_made_of_it() {
    let recorded = golden("host/spec.json");
    let room = Room::new("golden");
    std::fs::write(room.join("disk.qcow2"), b"QFI\xfb not really").unwrap();
    fake_disc(&room.join("installer.iso"));
    let dir = room.path().to_string_lossy().into_owned();
    // Every string, one at a time: a Windows path's backslashes would be
    // escapes if this were done on the JSON's text. On Windows, `\` reads
    // as `/` afterwards -- the recording was made on Linux, and what is
    // compared is the machine, not the separator.
    let at = |value: &Value| strings(value, &|text| text.replace("{DIR}", &dir));
    let out = |value: Value| -> Value {
        strings(&value, &|text| {
            let text = text.replace(&dir, "{DIR}").replace(UUID, "{UUID}");
            if cfg!(windows) {
                text.replace('\\', "/")
            } else {
                text
            }
        })
    };

    let mut environment = Environment::here();
    environment.qemu = Arc::new(|| beatbox_isolation::machine::Qemu {
        kvm: true,
        ..qemu()
    });
    environment.display_available = Arc::new(|_| true);
    let host = beatbox_isolation::machine::Qemu {
        kvm: true,
        ..qemu()
    };

    let cases = recorded["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 10);
    for (index, entry) in cases.iter().enumerate() {
        let case = entry["case"].as_str().unwrap();
        let settings = at(&entry["settings"]).as_object().cloned().unwrap();
        let mut amounts = Vec::new();
        if let Some(cpu) = entry["resources"].get("cpu").and_then(Value::as_f64) {
            amounts.push((CPU, cpu));
        }
        if let Some(memory) = entry["resources"].get("memory").and_then(Value::as_f64) {
            amounts.push((MEMORY, memory));
        }
        let config = SandboxConfig::new("host")
            .unwrap()
            .named(case)
            .with_resources(Resources::new(amounts).unwrap())
            .with_settings(settings);
        let identity = SandboxIdentity::new(&format!("sbx-{:012}", index + 1), "host", case, "1");
        let emitter = Emitter::standalone(&identity.id, Arc::new(EventBus::default()));
        let provider = HostSandbox::new(identity, config, emitter, environment.clone());
        // The overlay create() would have made, put there after the provider
        // is built -- as the recording did.
        let workspace = room.join(&format!("rooms/{case}"));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("disk.qcow2"), b"QFI\xfb").unwrap();

        let kept = beatbox_engine::SandboxProvider::capabilities(&provider).unwrap();
        // Capabilities newer than the recording.
        let kept: Vec<String> = kept
            .to_list()
            .into_iter()
            .filter(|name| {
                !matches!(
                    name.as_str(),
                    "process.shell" | "network.probe" | "snapshot.save_state" | "display.picture"
                )
            })
            .collect();
        assert_eq!(json!(kept), entry["capabilities"], "{case}: capabilities");
        match provider.machine_spec() {
            Ok(spec) => {
                assert_eq!(out(spec.to_value()), entry["spec"], "{case}: spec");
                let argv = build_command(&spec, &host, UUID).unwrap();
                assert_eq!(out(json!(argv)), entry["argv"], "{case}: argv");
            }
            Err(error) => {
                let refused = json!({"code": error.reason.as_ref().unwrap().code,
                    "values": error.reason.as_ref().unwrap().values});
                assert_eq!(out(refused), entry["refused"], "{case}: refused");
            }
        }
    }
}
