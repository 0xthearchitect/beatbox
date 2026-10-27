//! The QEMU argv against `golden/qemu/`: six machines under four hosts,
//! three installs, `qemu-img`, `-display help` and seven `os-release`s.
//! Every resource saving is a flag in these, so losing one fails here.
//!
//! `{DIR}` is put back where the recording's temporary directory was, and
//! `{UUID}` where its fixed UUID was.

mod support;

use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use beatbox_isolation::machine::{
    DiskBuild, Display, MachineSpec, Qemu, build_command, build_install_command, create_disk,
    create_overlay, disk_snapshots, display_available, display_install_hint_for,
};
use beatbox_isolation::runner::{Output, Runner};
use beatbox_reasons::Reason;
use serde_json::{Value, json};
use support::{Room, a_disc};

const UUID: &str = "00000000-0000-4000-8000-00000000b0b0";

fn golden(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../golden")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The recording's tree: the same files at the same places, under a
/// directory of our own.
struct Tree(Room);

impl Tree {
    fn new() -> Self {
        let room = Room::new("golden");
        room.file("disk.qcow2", b"QFI\xfb not really");
        room.file("room/disk.qcow2", b"QFI\xfb not really");
        room.file("installer.iso", &a_disc());
        room.file("Lab, test/Win10,x64.qcow2", b"QFI\xfb not really");
        room.file("Lab, test/inst,aller.iso", &a_disc());
        Self(room)
    }

    fn dir(&self) -> String {
        self.0.path().to_string_lossy().into_owned()
    }

    fn at(&self, text: &str) -> String {
        text.replace("{DIR}", &self.dir())
    }

    /// The recording's words for this run's: `{DIR}`, `{UUID}` -- and on
    /// Windows, `/` for the `\\` a resolved path is written with there,
    /// because the recording was made on Linux and the argv is what is being
    /// compared, not the separator.
    fn out(&self, text: &str) -> String {
        let text = text.replace(&self.dir(), "{DIR}").replace(UUID, "{UUID}");
        if cfg!(windows) {
            text.replace('\\', "/")
        } else {
            text
        }
    }

    fn argv(&self, argv: &[String]) -> Vec<String> {
        argv.iter().map(|part| self.out(part)).collect()
    }

    fn reasons(&self, reasons: &[Reason]) -> Value {
        Value::Array(
            reasons
                .iter()
                .map(|r| {
                    let values: serde_json::Map<String, Value> = r
                        .values
                        .iter()
                        .map(|(k, v)| (k.clone(), json!(self.out(v))))
                        .collect();
                    json!({"code": r.code, "values": values})
                })
                .collect(),
        )
    }
}

fn host(profile: &Value) -> Qemu {
    Qemu {
        system: "qemu-system-x86_64".into(),
        image: "qemu-img".into(),
        system_found: true,
        image_found: true,
        kvm: profile["kvm"].as_bool().unwrap(),
        whpx: profile["whpx"].as_bool().unwrap(),
        cpu: profile["cpu"].as_str().map(str::to_owned),
    }
}

fn spec_from(tree: &Tree, fields: &Value) -> MachineSpec {
    let mut spec = MachineSpec::default();
    let text = |key: &str| fields.get(key).and_then(Value::as_str).map(|s| tree.at(s));
    let flag = |key: &str| fields.get(key).and_then(Value::as_bool);
    let number = |key: &str| fields.get(key).and_then(Value::as_i64);
    if let Some(v) = text("name") {
        spec.name = v;
    }
    if let Some(v) = text("image") {
        spec.image = v;
    }
    if let Some(v) = number("memory_mb") {
        spec.memory_mb = v;
    }
    if let Some(v) = number("cpus") {
        spec.cpus = v;
    }
    if let Some(v) = text("network") {
        spec.network = v;
    }
    if let Some(v) = flag("headless") {
        spec.headless = v;
    }
    if let Some(v) = text("log_path") {
        spec.log_path = v;
    }
    if let Some(v) = flag("hostile") {
        spec.hostile = v;
    }
    if let Some(v) = text("capture_path") {
        spec.capture_path = v;
    }
    if let Some(v) = flag("instrument") {
        spec.instrument = v;
    }
    if let Some(v) = flag("disposable") {
        spec.disposable = v;
    }
    if let Some(v) = text("monitor_socket") {
        spec.monitor_socket = v;
    }
    if let Some(v) = text("agent_socket") {
        spec.agent_socket = v;
    }
    if let Some(ports) = fields.get("forwarded_ports").and_then(Value::as_array) {
        spec.forwarded_ports = ports
            .iter()
            .map(|pair| (pair[0].as_i64().unwrap(), pair[1].as_i64().unwrap()))
            .collect();
    }
    spec
}

fn build_from(tree: &Tree, fields: &Value) -> DiskBuild {
    let mut build = DiskBuild::default();
    let text = |key: &str| fields.get(key).and_then(Value::as_str).map(|s| tree.at(s));
    if let Some(v) = text("source") {
        build.source = v;
    }
    if let Some(v) = text("target") {
        build.target = v;
    }
    if let Some(v) = fields.get("size_gb").and_then(Value::as_i64) {
        build.size_gb = v;
    }
    if let Some(v) = fields.get("memory_mb").and_then(Value::as_i64) {
        build.memory_mb = v;
    }
    if let Some(v) = fields.get("cpus").and_then(Value::as_i64) {
        build.cpus = v;
    }
    if let Some(v) = fields.get("network").and_then(Value::as_bool) {
        build.network = v;
    }
    if let Some(v) = text("monitor_socket") {
        build.monitor_socket = v;
    }
    if let Some(v) = text("agent_socket") {
        build.agent_socket = v;
    }
    build
}

fn argv_of(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn every_machine_is_started_with_the_argv_the_python_built() {
    let recorded = golden("qemu/machine.json");
    let tree = Tree::new();
    let commands = recorded["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 24, "four hosts, six machines");
    for entry in commands {
        let case = format!("{} / {}", entry["profile"], entry["case"]);
        let qemu = host(&recorded["profiles"][entry["profile"].as_str().unwrap()]);
        let spec = spec_from(&tree, &entry["spec"]);
        let argv = build_command(&spec, &qemu, UUID).unwrap_or_else(|e| panic!("{case}: {e}"));
        assert_eq!(tree.argv(&argv), argv_of(&entry["argv"]), "{case}");
    }
    for entry in recorded["refused"].as_array().unwrap() {
        let spec = spec_from(&tree, &entry["spec"]);
        let host = host(&recorded["profiles"]["linux-kvm"]);
        let refused = build_command(&spec, &host, UUID).unwrap_err();
        assert_eq!(
            tree.reasons(&refused.reasons),
            entry["reasons"],
            "{}",
            entry["case"]
        );
    }
}

fn able() -> Display {
    Display {
        available: true,
        install_hint: String::new(),
    }
}

#[test]
fn every_install_is_started_with_the_argv_the_python_built() {
    let recorded = golden("qemu/install.json");
    let tree = Tree::new();
    let qemu = host(&json!({"kvm": true, "whpx": false, "cpu": null}));
    for entry in recorded["commands"].as_array().unwrap() {
        let build = build_from(&tree, &entry["build"]);
        let argv = build_install_command(&build, &qemu, &able(), UUID).unwrap();
        assert_eq!(
            tree.argv(&argv),
            argv_of(&entry["argv"]),
            "{}",
            entry["case"]
        );
    }
    for entry in recorded["refused"].as_array().unwrap() {
        let case = entry["case"].as_str().unwrap();
        let (build, qemu, display) = if case == "no-display-no-qemu-img" {
            let build = DiskBuild {
                source: tree.at("{DIR}/installer.iso"),
                target: tree.at("{DIR}/disks/y.qcow2"),
                ..DiskBuild::default()
            };
            let qemu = Qemu {
                image_found: false,
                ..qemu.clone()
            };
            let display = Display {
                available: false,
                install_hint: "{HINT}".into(),
            };
            (build, qemu, display)
        } else {
            (build_from(&tree, &entry["build"]), qemu.clone(), able())
        };
        let refused = build_install_command(&build, &qemu, &display, UUID).unwrap_err();
        assert_eq!(tree.reasons(&refused.reasons), entry["reasons"], "{case}");
    }
}

/// `subprocess.run` for qemu-img: records each argv, answers from a script.
struct Scripted {
    calls: Mutex<Vec<Vec<String>>>,
    answers: Mutex<Vec<Output>>,
}

impl Scripted {
    fn new(answers: Vec<Output>) -> Self {
        Self {
            calls: Mutex::default(),
            answers: Mutex::new(answers),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }
}

impl Runner for Scripted {
    fn run(&self, argv: &[String], _timeout: Option<Duration>) -> io::Result<Output> {
        self.calls.lock().unwrap().push(argv.to_vec());
        let mut answers = self.answers.lock().unwrap();
        Ok(if answers.is_empty() {
            Output::default()
        } else {
            answers.remove(0)
        })
    }
}

fn said(code: i32, stdout: &str, stderr: &str) -> Output {
    Output {
        code,
        stdout: stdout.into(),
        stderr: stderr.into(),
    }
}

#[test]
fn qemu_img_and_the_display_are_asked_what_the_python_asked() {
    let recorded = golden("qemu/qemu-img.json");
    let tree = Tree::new();
    let qemu = host(&json!({"kvm": true, "whpx": false, "cpu": null}));
    let calls = |runner: &Scripted| -> Value {
        json!(
            runner
                .calls()
                .iter()
                .map(|argv| tree.argv(argv))
                .collect::<Vec<_>>()
        )
    };

    let runner = Scripted::new(vec![]);
    let build = DiskBuild {
        source: tree.at("{DIR}/installer.iso"),
        target: tree.at("{DIR}/disks/new.qcow2"),
        size_gb: 40,
        ..DiskBuild::default()
    };
    create_disk(&build, &qemu, &able(), &runner).unwrap();
    assert_eq!(calls(&runner), recorded["create_disk"]);

    let runner = Scripted::new(vec![said(0, r#"{"format": "raw"}"#, ""), said(0, "", "")]);
    create_overlay(
        &tree.at("{DIR}/disk.qcow2"),
        &tree.at("{DIR}/room2/disk.qcow2"),
        &qemu,
        &runner,
    )
    .unwrap();
    assert_eq!(calls(&runner), recorded["create_overlay"]);

    for entry in recorded["disk_snapshots"].as_array().unwrap() {
        let case = entry["case"].as_str().unwrap();
        let code = if case == "qemu-img-failed" { 1 } else { 0 };
        let runner = Scripted::new(vec![said(code, entry["stdout"].as_str().unwrap(), "")]);
        let names = disk_snapshots(&tree.at("{DIR}/room/disk.qcow2"), &qemu, &runner);
        assert_eq!(json!(names), entry["names"], "{case}");
        assert_eq!(
            json!(tree.argv(&runner.calls()[0])),
            entry["argv"],
            "{case}"
        );
    }

    for entry in recorded["display_available"].as_array().unwrap() {
        let runner = Scripted::new(vec![said(0, entry["stdout"].as_str().unwrap(), "")]);
        let available = display_available(&qemu, &runner);
        assert_eq!(json!(available), entry["available"], "{}", entry["case"]);
        assert_eq!(json!(runner.calls()[0]), entry["argv"], "{}", entry["case"]);
    }

    for entry in recorded["display_install_hint"].as_array().unwrap() {
        let case = entry["case"].as_str().unwrap();
        let hint = match (case, entry["os_release"].as_str()) {
            ("windows", _) => display_install_hint_for(std::path::Path::new("/nowhere"), true),
            (_, None) => display_install_hint_for(&tree.0.join("os-release/missing"), false),
            (_, Some(text)) => {
                let path = tree.0.file(&format!("os-release/{case}"), text.as_bytes());
                display_install_hint_for(&path, false)
            }
        };
        assert_eq!(json!(hint), entry["hint"], "{case}");
    }
}
