//! The window proving itself, from inside: only with `--features proof`.
//!
//! WebKitGTK has no DevTools protocol, and its WebDriver isn't packaged
//! everywhere (Arch's `webkit2gtk-4.1` has none), so the window drives its
//! own page and reports back through a command. Never in a package: it's a
//! binary that can drive itself.
//!
//! ```text
//! beatbox --proof walk            every route at every width, checked for layout faults
//! beatbox --proof leave <disk>    a machine made from <disk> and started, listed, and the window
//!                                 closed with "leave it running", the page's own button pressed
//! beatbox --proof stop <name>     <name> found running and listed, and the window closed with "stop"
//! beatbox --proof install <disc> <name>
//!                                 a disk made from <disc>, through the install dialog's own
//!                                 actions: built, "I am done", the guest asked to power off
//! beatbox --proof server <disk>   a first server, through its pages: made on a machine (from
//!                                 <disk>, left off, no cable) and a network; the cable warned
//!                                 about; the empty page; a website refused with the machine off,
//!                                 saying how to turn it on; the overview's needs, the Library, Configuration
//! beatbox --proof leave <disk> windowed
//!                                 the same leave, with the machine in a window of its own and a
//!                                 save point taken before closing
//! ```
//!
//! Every request goes through the page -- `invoke('call')`, through its
//! policy -- and every close through the X and the question the page draws.
//!
//! Two more, with no window, for a script that has nothing else to ask the
//! daemon with (a container with nothing else installed):
//!
//! ```text
//! beatbox --proof status          the daemon's status, as JSON, or null when nothing answers
//! beatbox --proof clean           every machine thrown away, and the daemon asked to go
//! ```

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use beatbox_shell::Shell;
use serde_json::{Value, json};
use tauri::{AppHandle, LogicalSize, WebviewWindow};

/// The layout check's routes and widths.
const ROUTES: &[&str] = &[
    "hero",
    "account?next=host/machine",
    "setup",
    "host/machine",
    "host/processes",
    "host/pen",
    "settings",
    "engine",
    "foundations",
];
const WIDTHS: &[f64] = &[1400.0, 1100.0, 900.0, 700.0, 480.0, 380.0];

/// The layout check's own probe, byte for byte.
const PROBE: &str = include_str!("probe.js");

/// Run before the page, so that the warning Tauri gives when its IPC
/// protocol is refused -- and it falls back to `postMessage`, past the
/// policy -- is kept for [`Driver::loaded`] to read.
pub const WATCH: &str = "window.__proofWarnings = []; \
    const warn = console.warn.bind(console); \
    console.warn = (...args) => { window.__proofWarnings.push(args.map(String).join(' ')); warn(...args); };";

/// Where the page's answers arrive.
pub struct Reports(pub Mutex<Sender<Value>>);

/// What the page said.
#[tauri::command]
pub fn report(reports: tauri::State<Reports>, value: Value) {
    let _ = reports.0.lock().expect("one reporter").send(value);
}

/// The channel the page reports on.
pub fn reports() -> (Reports, Receiver<Value>) {
    let (sender, receiver) = channel();
    (Reports(Mutex::new(sender)), receiver)
}

struct Driver {
    window: WebviewWindow,
    answers: Receiver<Value>,
}

impl Driver {
    /// Run `expression` in the page and hand back its value, awaited.
    fn ask(&self, expression: &str) -> Option<Value> {
        while self.answers.try_recv().is_ok() {}
        let script = format!(
            "(async () => {{ let value; try {{ value = await ({expression}); }} \
             catch (error) {{ value = {{ thrown: String(error) }}; }} \
             window.__TAURI_INTERNALS__.invoke('report', {{ value: value ?? null }}); }})()"
        );
        self.window.eval(script).ok()?;
        self.answers.recv_timeout(Duration::from_secs(60)).ok()
    }

    /// An action, sent the way the page sends one.
    fn call(&self, action: &str, payload: Value) -> Value {
        let payload = payload.to_string();
        self.ask(&format!(
            "window.__TAURI_INTERNALS__.invoke('call', {{ action: {action:?}, payload: {payload} }})"
        ))
        .unwrap_or(Value::Null)
    }

    fn until(&self, what: &str, within: Duration, test: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if test(self) {
                return;
            }
            thread::sleep(Duration::from_millis(200));
        }
        fail(&format!("{what}, within {}s", within.as_secs()));
    }

    fn text(&self) -> String {
        self.ask("document.body.innerText")
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    fn go(&self, route: &str) {
        let _ = self
            .window
            .eval(format!("window.location.hash = '/{route}';"));
    }

    fn loaded(&self) {
        self.until("the page to load", Duration::from_secs(30), |driver| {
            driver.ask("document.readyState") == Some(json!("complete"))
                && !driver.text().is_empty()
        });
        // Every request so far went over the custom protocol the policy
        // admits, not around it.
        let warned = self.ask("window.__proofWarnings").unwrap_or_default();
        let refused = warned
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find(|warning| warning.contains("IPC custom protocol failed"));
        if let Some(warning) = refused {
            fail(&format!("the page's policy refused Tauri's IPC: {warning}"));
        }
    }

    /// Press the first `selector` whose text has `label` in it, or fail.
    fn press(&self, selector: &str, label: &str) {
        let (selector, label) = (
            serde_json::to_string(selector).expect("JSON"),
            serde_json::to_string(label).expect("JSON"),
        );
        let pressed = self.ask(&format!(
            "(() => {{ const found = [...document.querySelectorAll({selector})] \
             .find((b) => b.textContent.includes({label}) && !b.disabled); if (found) found.click(); \
             return !!found; }})()"
        ));
        if pressed != Some(json!(true)) {
            fail(&format!("nothing to press: {selector} with {label}"));
        }
    }

    /// Wait until the page says `words`.
    fn shows(&self, words: &str, what: &str) {
        self.until(what, Duration::from_secs(20), |driver| {
            driver.text().contains(words)
        });
        say(what);
    }

    /// Press the X, wait for the page's question, and press `button` in it.
    fn close_answering(&self, button: &str) {
        let _ = self.window.close();
        self.until(
            "the page to ask what to do",
            Duration::from_secs(15),
            |driver| driver.ask("!!document.querySelector('.bb-closing')") == Some(json!(true)),
        );
        let label = serde_json::to_string(button).expect("JSON");
        let pressed = self.ask(&format!(
            "(() => {{ const found = [...document.querySelectorAll('.bb-modal button')] \
             .find((b) => b.textContent.trim() === {label}); if (found) found.click(); \
             return !!found; }})()"
        ));
        if pressed != Some(json!(true)) {
            fail(&format!("the question has no button called {button}"));
        }
    }
}

fn say(step: &str) {
    println!("  ok  {step}");
}

fn fail(what: &str) -> ! {
    println!("FAILED: {what}");
    std::process::exit(1)
}

fn ok(envelope: Value, what: &str) -> Value {
    if envelope["ok"] != json!(true) {
        fail(&format!("{what}: {}", envelope["error"]));
    }
    envelope["data"].clone()
}

/// `status` and `clean`, which need no window: the exit code, or `None` for
/// a proof that does.
pub fn without_a_window(what: &[String]) -> Option<std::process::ExitCode> {
    let link = beatbox_shell::link::Link::new(beatbox_contract::wire::socket_path(), false);
    match what.first().map(String::as_str) {
        Some("status") => {
            println!("{}", link.status().unwrap_or(Value::Null));
            Some(std::process::ExitCode::SUCCESS)
        }
        Some("clean") => {
            let listed = link.call("sandbox.list", &json!({}));
            for row in listed
                .data
                .as_ref()
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                link.call(
                    "sandbox.transition",
                    &json!({"id": row["id"], "transition": "destroy"}),
                );
            }
            let _ = link.daemon("exit");
            Some(std::process::ExitCode::SUCCESS)
        }
        _ => None,
    }
}

/// Run `what` in this window, on a thread of its own, and exit with its
/// verdict.
pub fn run(
    app: AppHandle,
    window: WebviewWindow,
    shell: Arc<Shell>,
    what: Vec<String>,
    answers: Receiver<Value>,
) {
    thread::spawn(move || {
        let driver = Driver { window, answers };
        driver.loaded();
        match what.first().map(String::as_str) {
            Some("walk") => walk(&app, &driver),
            Some("leave") => leave(
                &driver,
                &shell,
                what.get(1).cloned().unwrap_or_default(),
                what.get(2).is_some_and(|how| how == "windowed"),
            ),
            Some("install") => install(
                &app,
                &driver,
                what.get(1).cloned().unwrap_or_default(),
                what.get(2).cloned().unwrap_or_default(),
            ),
            Some("stop") => stop(&driver, &shell, what.get(1).cloned().unwrap_or_default()),
            Some("server") => server(
                &app,
                &driver,
                &shell,
                what.get(1).cloned().unwrap_or_default(),
            ),
            other => fail(&format!("no proof called {other:?}")),
        }
    });
}

fn walk(app: &AppHandle, driver: &Driver) {
    let mut findings = Vec::new();
    let mut checked = 0;
    for route in ROUTES {
        for width in WIDTHS {
            let _ = driver.window.set_size(LogicalSize::new(*width, 900.0));
            driver.go(route);
            thread::sleep(Duration::from_millis(400));
            checked += 1;
            let Some(raw) = driver.ask(PROBE.trim().trim_end_matches(';')) else {
                findings.push(format!("{route} @ {width}px: the page did not answer"));
                continue;
            };
            let answer: Value = raw
                .as_str()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or_default();
            let bad = answer["bad"].as_array().cloned().unwrap_or_default();
            if answer["sideways"] == json!(true) || !bad.is_empty() {
                findings.push(format!("{route} @ {width}px: {bad:?}"));
            }
        }
    }
    println!("{checked} route x width combinations checked");
    for finding in &findings {
        println!("  {finding}");
    }
    if findings.is_empty() {
        println!("nothing is being clipped");
    }
    app.exit(i32::from(!findings.is_empty()));
}

fn install(app: &AppHandle, driver: &Driver, disc: String, name: String) {
    let job = ok(
        driver.call(
            "disks.build",
            json!({"source": disc, "name": name, "sizeGb": 1, "network": false, "unattended": false, "machine": null}),
        ),
        "disks.build",
    );
    say(&format!(
        "an install of {name} started, as job {}",
        job["id"]
    ));
    let current = |driver: &Driver| driver.call("jobs.current", json!({}))["data"].clone();
    driver.until(
        "the installer to be up",
        Duration::from_secs(120),
        |driver| current(driver)["message"] == json!("working"),
    );
    say("its guest is up, waiting on a person");
    ok(driver.call("disks.finish", json!({})), "disks.finish");
    driver.until("the install to end", Duration::from_secs(300), |driver| {
        current(driver)["state"] != json!("running")
    });
    // The disc boots nothing, so nothing was installed: BeatBox has to say
    // so and throw the blank disk away, not hand it over as a disk.
    let ended = current(driver);
    if ended["reason"]["code"] != json!("disks.nothing_installed") {
        fail(&format!("the install ended as {ended}"));
    }
    let disks = ok(driver.call("disks.list", json!({})), "disks.list");
    if disks
        .as_array()
        .is_some_and(|disks| disks.iter().any(|disk| disk["name"] == json!(name)))
    {
        fail(&format!("{name} was kept, and nothing is on it"));
    }
    say(
        "told it was done: the guest was asked to power off, nothing was on the disk, and it was not kept",
    );
    app.exit(0);
}

fn leave(driver: &Driver, shell: &Shell, disk: String, windowed: bool) {
    say("the page loaded, and asked the daemon through the window, over the IPC its policy admits");
    let made = ok(
        driver.call(
            "sandbox.create",
            json!({"type": "host", "name": "shell-walk", "cpu": 1, "memory": 256,
                   "settings": {"disk": disk, "display": windowed}, "uses": {}}),
        ),
        "sandbox.create",
    );
    let id = made["id"].as_str().unwrap_or_default().to_owned();
    let started = ok(
        driver.call(
            "sandbox.transition",
            json!({"id": id, "transition": "start"}),
        ),
        "start",
    );
    if started["state"]["lifecycle"] != json!("running") {
        fail(&format!("it did not start: {}", started["state"]));
    }
    say(&format!(
        "made {id} from {disk} and started it, through the page{}",
        if windowed {
            ", in a window of its own"
        } else {
            ""
        }
    ));
    if windowed {
        let saved = ok(
            driver.call(
                "sandbox.act",
                json!({"id": id, "capability": "snapshot.create",
                       "arguments": {"name": "before leaving"}}),
            ),
            "snapshot.create",
        );
        if saved["ok"] != json!(true) {
            fail(&format!("no save point: {saved}"));
        }
        say("took a save point");
    }
    driver.go("host/machine");
    driver.until(
        "the Sandbox screen to list it",
        Duration::from_secs(20),
        |driver| driver.text().contains("shell-walk"),
    );
    say("the Sandbox screen lists it");
    let leave = shell.language().text("beatbox.closing.leave");
    driver.close_answering(&leave);
    say("pressed the X, and answered the page's question: leave it running");
}

fn stop(driver: &Driver, shell: &Shell, name: String) {
    driver.go("host/machine");
    driver.until(
        &format!("the Sandbox screen to list {name}"),
        Duration::from_secs(20),
        |driver| driver.text().contains(&name),
    );
    let listed = ok(driver.call("sandbox.list", json!({})), "sandbox.list");
    let Some(id) = listed
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["name"] == json!(name)))
        .and_then(|row| row["id"].as_str())
        .map(str::to_owned)
    else {
        fail(&format!("{name} is not in the list"));
    };
    let seen = ok(driver.call("sandbox.refresh", json!({"id": id})), "refresh");
    if seen["state"]["lifecycle"] != json!("running") {
        fail(&format!("{name} is not running: {}", seen["state"]));
    }
    say(&format!("opened again: {name} listed, and still running"));
    let stop = shell.language().text("beatbox.closing.stop");
    driver.close_answering(&stop);
    say("pressed the X, and answered the page's question: stop");
}

/// Every control on the page a screen reader could not name: a button,
/// field or choice with no text, label, `aria-label` or title (#71).
const UNNAMED: &str = r#"(() => {
  const named = (el) => (el.getAttribute('aria-label') || el.getAttribute('aria-labelledby')
    || el.title || (el.innerText || '').trim()
    || (el.id && document.querySelector('label[for="' + el.id + '"]')) || el.closest('label'));
  return [...document.querySelectorAll('button, input, select, textarea, [role=radio], [role=tab], [role=switch]')]
    .filter((el) => el.type !== 'hidden' && el.offsetParent !== null && !named(el))
    .map((el) => el.outerHTML.slice(0, 160));
})()"#;

fn all_named(driver: &Driver, page: &str) {
    let unnamed = driver.ask(UNNAMED).unwrap_or_default();
    if unnamed.as_array().is_some_and(|list| !list.is_empty()) {
        fail(&format!("{page}: controls with no name: {unnamed}"));
    }
}

/// A first server, the way a person meets one (#71): every step through the
/// page, every check what the page says, in the window's own language.
fn server(app: &AppHandle, driver: &Driver, shell: &Shell, disk: String) {
    let words = |key: &str| shell.language().text(key);
    let host = ok(
        driver.call(
            "sandbox.create",
            json!({"type": "host", "name": "proof-web", "cpu": 1, "memory": 256,
                   "settings": {"disk": disk}, "uses": {}}),
        ),
        "a machine",
    );
    let network = ok(
        driver.call(
            "sandbox.create",
            json!({"type": "network", "name": "proof-lab", "cpu": 0, "memory": 0,
                   "settings": {"cidr": "10.77.0.0/24"}, "uses": {}}),
        ),
        "a network",
    );
    say("a machine, off and with no cable, and a network");

    driver.go("server/services");
    driver.shows(
        &words("beatbox.srv.new.title"),
        "the server pages start with a new server",
    );
    driver.press(".bb-srv-radio", "proof-web");
    driver.press(".bb-srv-radio", "proof-lab");
    let apart = words("beatbox.srv.new.apart")
        .replace("{machine}", "proof-web")
        .replace("{network}", "proof-lab");
    driver.until(
        "the cable to be warned about",
        Duration::from_secs(20),
        |driver| driver.text().contains(&apart),
    );
    say("the machine with no cable to the network is said before the server is made");
    driver.press("button", &words("beatbox.srv.new.make"));
    driver.shows(
        &words("beatbox.srv.empty.title"),
        "made: the empty page says where to start",
    );
    all_named(driver, "the empty page");

    driver.press(".bb-srv-tile", &words("beatbox.srv.start.http-static"));
    driver.shows(
        &words("beatbox.srv.put_to_work"),
        "the form to make a website",
    );
    all_named(driver, "the form to make a website");
    driver.press(".bb-modal button", &words("beatbox.srv.put_to_work"));
    driver.until(
        "the refusal with the machine off",
        Duration::from_secs(30),
        |driver| driver.ask("!!document.querySelector('.bb-srv-form-notice')") == Some(json!(true)),
    );
    let said = driver
        .ask("document.querySelector('.bb-srv-form-notice').innerText")
        .unwrap_or_default();
    if !said
        .as_str()
        .unwrap_or_default()
        .contains(&words("beatbox.srv.add.open_host"))
    {
        fail(&format!("the refusal says nothing to do: {said}"));
    }
    say("a website with the machine off is refused, with the way to turn it on");
    let _ = driver.ask("document.querySelector('.bb-modal-head button')?.click()");

    driver.go("server/services");
    driver.shows(
        &words("beatbox.srv.helpers.title"),
        "the overview says what the server needs",
    );
    let cable = words("beatbox.srv.helpers.cable_no").replace("{network}", "proof-lab");
    driver.shows(&cable, "and that its machine has no cable to the network");
    all_named(driver, "the overview");
    driver.go("server/library");
    driver.shows(
        &words("beatbox.srv.lib.filter.ready"),
        "the Library sorts kinds by what the machine has",
    );
    all_named(driver, "the Library");
    driver.go("server/settings");
    driver.shows(
        &words("beatbox.srv.set.where"),
        "Configuration says where the server is",
    );
    // The section's label is drawn in capitals; its first choice is not.
    driver.shows(
        &words("beatbox.srv.set.on_exit.leave"),
        "what to do when a service stops on its own",
    );
    driver.shows(
        &words("beatbox.srv.shared.title"),
        "and what the services share",
    );
    all_named(driver, "Configuration");
    say("every control on the server pages has a name a screen reader can say");

    let servers = ok(driver.call("sandbox.list", json!({})), "sandbox.list");
    for row in servers.as_array().into_iter().flatten() {
        if row["type"] == json!("server") {
            let _ = driver.call(
                "sandbox.transition",
                json!({"id": row["id"], "transition": "destroy"}),
            );
        }
    }
    for made in [host, network] {
        let _ = driver.call(
            "sandbox.transition",
            json!({"id": made["id"], "transition": "destroy"}),
        );
    }
    say("thrown away");
    app.exit(0);
}
