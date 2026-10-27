//! Drive a sandbox the whole way through the contract, and print the story.
//!
//! The walk `tests/golden.rs` checks, printed so you can watch it:
//!
//! ```text
//! Create -> Configure -> Start -> Action -> Event -> Stop -> Destroy
//! ```
//!
//! Against `MockSandbox`, a map pretending to be a machine: nothing touches
//! this host.
//!
//! ```text
//! cargo run -p beatbox-engine --example engine_walk
//! cargo run -p beatbox-engine --example engine_walk -- --crash    # the unhappy path
//! ```
//!
//! `--crash` takes the sandbox away behind the runtime's back; the next
//! observation finds out.

use beatbox_engine::runtime::resources::{CPU, MEMORY};
use beatbox_engine::{
    MockSandbox, Resources, RuntimeParts, SandboxConfig, SandboxRegistry, SandboxRuntime, data,
};

const GREY: &str = "\x1b[90m";
const BOLD: &str = "\x1b[1m";
const OFF: &str = "\x1b[0m";

fn step(title: &str) {
    println!("\n{BOLD}{title}{OFF}");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let crash = std::env::args().any(|argument| argument == "--crash");

    let mut registry = SandboxRegistry::default();
    registry.register(MockSandbox::spec())?;
    let runtime = SandboxRuntime::new(RuntimeParts {
        registry,
        ..RuntimeParts::default()
    });
    runtime.events().subscribe("*", |event| {
        println!(
            "{GREY}  · {:20} {}{OFF}",
            event.name(),
            serde_json::Value::Object(event.data().clone())
        );
    })?;

    step("What can this runtime build?");
    for entry in runtime.registry().types() {
        println!(
            "  {} v{}, isolates at '{}'",
            entry.name, entry.version, entry.isolation
        );
        for capability in entry.capabilities.names() {
            println!("    {capability}");
        }
    }

    step("Who could run a process for me?");
    let who: Vec<String> = runtime
        .registry()
        .providing("process.execute")
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    println!("   {who:?}");

    step("Create");
    let config = SandboxConfig::new("mock")?
        .named("windows-client")
        .with_resources(Resources::new([(CPU, 4.0), (MEMORY, 8192.0)])?)
        .with_settings(data!({"files": {"/etc/hosts": "127.0.0.1 localhost"}}));
    let sandbox = runtime.create(config.clone())?;
    println!("  {sandbox}");

    step("Configuration is not state");
    let state = sandbox.state();
    println!(
        "  asked for : {}  isolation={}",
        serde_json::Value::Object(config.resources.to_data()),
        config.isolation.level
    );
    println!(
        "  is now    : {} since {}, facts={}",
        state.lifecycle,
        state.since,
        serde_json::Value::Object(state.facts)
    );

    step("Start");
    sandbox.start()?;
    println!(
        "  {sandbox}  using {}",
        serde_json::Value::Object(sandbox.state().resources.to_data())
    );

    step("Act");
    for (capability, arguments) in [
        ("process.execute", data!({"command": "/tmp/sample.exe"})),
        (
            "filesystem.write",
            data!({"path": "/tmp/note", "content": "it wrote something"}),
        ),
        ("snapshot.create", data!({"name": "before_attack"})),
        ("filesystem.read", data!({"path": "/tmp/note"})),
        ("filesystem.read", data!({"path": "/tmp/nowhere"})),
        ("traffic.capture", data!({})),
    ] {
        let outcome = sandbox.act(capability, arguments)?;
        let verdict = if outcome.ok {
            format!("ok {}", serde_json::Value::Object(outcome.data))
        } else {
            format!(
                "no {}",
                outcome
                    .reason
                    .map(|why| why.to_string())
                    .unwrap_or_default()
            )
        };
        println!("  {capability:18} -> {verdict}");
    }

    if crash {
        step("The world takes it away");
        sandbox.provider(|p: &mut MockSandbox| p.crash());
        println!("  (nothing failed, nothing was announced)");
        println!("  before asking: {}", sandbox.lifecycle());
        sandbox.refresh()?;
        let why = sandbox
            .state()
            .reason
            .map(|why| why.to_string())
            .unwrap_or_default();
        println!("  after asking : {}, because {why}", sandbox.lifecycle());
        step("Reset, which is the way back");
        sandbox.reset()?;
        println!("  {sandbox}");
    } else {
        step("Pause, resume, stop");
        sandbox.pause()?;
        sandbox.resume()?;
        sandbox.stop()?;
        println!("  {sandbox}");
    }

    step("Destroy");
    sandbox.destroy()?;
    println!("  {sandbox}");
    println!(
        "  the ledger holds {}",
        serde_json::Value::Object(runtime.resources().committed().to_data())
    );
    println!(
        "  {} events, and the runtime still answers for {}",
        runtime.events().published(),
        sandbox.id()
    );
    Ok(())
}
