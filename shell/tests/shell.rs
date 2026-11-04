//! The window's half of the contract, with a pretend window.
//!
//! Language, routing, the window's answers, and the two closing answers in
//! `golden/contract/envelopes.json`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use beatbox_contract::{ClosingView, Envelope};
use beatbox_shell::closing::{Close, Closing, Then};
use beatbox_shell::language::{Language, SUPPORTED};
use beatbox_shell::link::Link;
use beatbox_shell::preferences::Preferences;
use beatbox_shell::{Shell, Window, outside_image};
use serde_json::{Value, json};

struct Room(PathBuf);

impl Room {
    fn new() -> Self {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("beatbox-shell-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Room {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A window, as far as the shell is allowed to know about it.
#[derive(Default)]
struct Pretend {
    pushed: Mutex<Vec<(String, Value)>>,
    gone: Mutex<bool>,
    displays: Mutex<Vec<(String, String, String, String)>>,
}

impl Window for Pretend {
    fn push(&self, name: &str, payload: &Value) {
        self.pushed
            .lock()
            .unwrap()
            .push((name.to_owned(), payload.clone()));
    }

    fn go(&self) {
        *self.gone.lock().unwrap() = true;
    }

    fn display(&self, id: &str, name: &str, url: &str, password: &str) -> Result<(), String> {
        self.displays.lock().unwrap().push((
            id.to_owned(),
            name.to_owned(),
            url.to_owned(),
            password.to_owned(),
        ));
        Ok(())
    }
}

impl Pretend {
    fn gone_within(&self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if *self.gone.lock().unwrap() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }
}

/// A shell whose daemon is nowhere: whatever it would have answered comes
/// back as `no-daemon`, which is how a test can tell it was sent there.
fn shell(room: &Room) -> Arc<Shell> {
    Shell::new(
        Link::new(room.0.join("nobody.sock"), false),
        Language::at(room.0.join("shell.json")),
        Preferences::at(room.0.join("shell.json")),
    )
}

fn with_a_window(room: &Room) -> (Arc<Shell>, Arc<Pretend>) {
    let shell = shell(room);
    let window = Arc::new(Pretend::default());
    shell.attach(window.clone());
    (shell, window)
}

fn code(envelope: &Envelope) -> String {
    envelope
        .error
        .as_ref()
        .map(|e| e.code.clone())
        .unwrap_or_default()
}

// --- the language (LanguageTests) -----------------------------------------------------

#[test]
fn test_the_locale_carries_every_beatbox_string() {
    let room = Room::new();
    let locale = shell(&room).answer("system.locale", &json!({}));
    let data = locale.data.unwrap();
    assert!(SUPPORTED.contains(&data["language"].as_str().unwrap()));
    let strings = data["strings"].as_object().unwrap();
    assert!(strings.len() > 100);
    assert!(strings.keys().all(|key| key.starts_with("beatbox.")));
}

#[test]
fn test_switching_changes_what_the_next_caller_is_handed() {
    let room = Room::new();
    let shell = shell(&room);
    let say = |language: &str| {
        shell
            .answer("system.set_language", &json!({"language": language}))
            .data
            .unwrap()["strings"]["beatbox.nav.settings"]
            .clone()
    };
    let english = say("en");
    let portuguese = say("pt_PT");
    let locale = shell.answer("system.locale", &json!({})).data.unwrap();
    assert_eq!(locale["language"], "pt_PT");
    // Not a claim about the words -- a claim that the file changed.
    assert_ne!(portuguese, say("en"));
    assert!(english.as_str().is_some_and(|text| !text.is_empty()));
}

#[test]
fn test_a_language_nobody_ships_is_refused() {
    let room = Room::new();
    let shell = shell(&room);
    let before = shell.language().active();
    let refused = shell.answer("system.set_language", &json!({"language": "klingon"}));
    assert_eq!(code(&refused), "no-such-language");
    assert_eq!(
        refused.error.unwrap().message,
        "klingon is not one of: en, pt_PT."
    );
    assert_eq!(shell.language().active(), before);
}

/// The screen that asked is not the only one showing text.
#[test]
fn test_the_change_is_announced_so_every_open_screen_hears_it() {
    let room = Room::new();
    let (shell, window) = with_a_window(&room);
    shell.answer("system.set_language", &json!({"language": "pt_PT"}));
    let pushed = window.pushed.lock().unwrap();
    let (name, payload) = pushed.last().expect("nothing was published");
    assert_eq!(name, "locale");
    assert_eq!(payload["language"], "pt_PT");
    assert!(payload["strings"].get("beatbox.nav.sandbox").is_some());
}

/// The window remembers it and reads it back at start, or every window
/// would open in English.
#[test]
fn test_the_host_is_what_remembers_it() {
    let room = Room::new();
    shell(&room).answer("system.set_language", &json!({"language": "pt_PT"}));
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(room.0.join("shell.json")).unwrap()).unwrap();
    assert_eq!(saved["language"], "pt_PT");
    let reopened = shell(&room);
    assert_eq!(reopened.language().active(), "pt_PT");
    assert_ne!(
        reopened.language().text("beatbox.window.title"),
        "beatbox.window.title"
    );
}

// --- host.closing (FacadeTests) -------------------------------------------------------

#[test]
fn test_the_action_exists_in_the_contract() {
    let spec = beatbox_contract::action("host.closing").unwrap();
    assert!(spec.mutates);
    assert_eq!(spec.answered_by, beatbox_contract::AnsweredBy::Shell);
}

#[test]
fn test_a_machine_display_reaches_the_window() {
    let room = Room::new();
    let (shell, window) = with_a_window(&room);
    let answer = shell.answer(
        "host.display",
        &json!({
            "id": "sbx-12",
            "name": "pc1",
            "url": "ws://127.0.0.1:5901",
            "password": "eight888",
        }),
    );
    assert!(answer.ok);
    assert_eq!(
        *window.displays.lock().unwrap(),
        vec![(
            "sbx-12".into(),
            "pc1".into(),
            "ws://127.0.0.1:5901".into(),
            "eight888".into(),
        )]
    );
}

#[test]
fn test_without_a_shell_it_refuses_instead_of_being_unknown() {
    let room = Room::new();
    let refused = shell(&room).answer("host.closing", &json!({"choice": "stop"}));
    assert_eq!(code(&refused), "no-host");
}

#[test]
fn test_the_choice_reaches_the_window() {
    let room = Room::new();
    let (shell, window) = with_a_window(&room);
    assert_eq!(
        shell.closing().requested(Some(ClosingView {
            sandboxes: 1,
            installing: false
        })),
        Close::Ask(ClosingView {
            sandboxes: 1,
            installing: false
        })
    );
    let answered = shell.answer("host.closing", &json!({"choice": "leave"}));
    assert_eq!(answered.data, Some(json!(true)));
    assert!(window.gone_within(Duration::from_secs(2)));
    assert_eq!(shell.closing().answered(), Some("leave"));
}

/// A stale click, or a second answer. Better false than a closed window.
#[test]
fn test_a_window_that_was_not_asking_says_so() {
    let room = Room::new();
    let (shell, window) = with_a_window(&room);
    let answered = shell.answer("host.closing", &json!({"choice": "stop"}));
    assert_eq!(answered.data, Some(json!(false)));
    assert!(!window.gone_within(Duration::from_millis(200)));
}

#[test]
fn test_an_invented_answer_is_refused_before_it_reaches_the_window() {
    let room = Room::new();
    let (shell, window) = with_a_window(&room);
    shell.closing().requested(Some(ClosingView {
        sandboxes: 1,
        installing: false,
    }));
    let refused = shell.answer(
        "host.closing",
        &json!({"choice": "quit-and-delete-everything"}),
    );
    assert_eq!(code(&refused), "bad-choice");
    assert!(shell.closing().asking(), "it was passed through anyway");
    assert!(!window.gone_within(Duration::from_millis(200)));
}

#[test]
fn test_a_released_shell_is_not_spoken_to() {
    let room = Room::new();
    let (shell, _window) = with_a_window(&room);
    shell.detach();
    assert_eq!(
        code(&shell.answer("host.closing", &json!({"choice": "stop"}))),
        "no-host"
    );
}

// --- the person's choices ------------------------------------------------------------

#[test]
fn test_the_choices_start_as_the_window_always_behaved() {
    let room = Room::new();
    let seen = shell(&room)
        .answer("system.preferences", &json!({}))
        .data
        .unwrap();
    assert_eq!(seen["onClose"], "ask");
    // A test build is a debug build: the builder's pages show.
    assert_eq!(seen["builderTools"], cfg!(debug_assertions));
    // A new machine starts where the form always did.
    assert_eq!(seen["newMemoryMb"], 2048);
    assert_eq!(seen["newCpus"], 2);
    assert_eq!(seen["newReach"], "none");
    assert_eq!(seen["newAgent"], true);
}

#[test]
fn test_about_names_this_build_and_reveal_opens_only_what_it_names() {
    let room = Room::new();
    let shell = shell(&room);
    let about = shell.answer("system.about", &json!({})).data.unwrap();
    assert_eq!(about["version"], env!("CARGO_PKG_VERSION"));
    assert!(about["log"].as_str().unwrap().ends_with("beatboxd.log"));
    let refused = shell.answer("host.reveal", &json!({"what": "/etc/passwd"}));
    assert_eq!(code(&refused), "bad-choice");
}

#[test]
fn test_the_choices_are_remembered_beside_the_language() {
    let room = Room::new();
    let first = shell(&room);
    first.answer("system.set_language", &json!({"language": "pt_PT"}));
    let set = first.answer(
        "system.set_preferences",
        &json!({"onClose": "leave", "builderTools": false, "newMemoryMb": 4096,
               "newCpus": 4, "newReach": "nat", "newAgent": false}),
    );
    assert!(set.ok, "{:?}", set.error);
    let reopened = shell(&room);
    assert_eq!(reopened.preferences().on_close(), "leave");
    assert!(!reopened.preferences().builder_tools());
    let seen = reopened
        .answer("system.preferences", &json!({}))
        .data
        .unwrap();
    assert_eq!(seen["newMemoryMb"], 4096);
    assert_eq!(seen["newCpus"], 4);
    assert_eq!(seen["newReach"], "nat");
    assert_eq!(seen["newAgent"], false);
    // Writing them kept the language that was already there.
    assert_eq!(reopened.language().active(), "pt_PT");
}

#[test]
fn test_a_choice_nobody_offers_is_refused_and_changes_nothing() {
    let room = Room::new();
    let shell = shell(&room);
    for payload in [
        ("explode", json!(true), json!(2048), json!(2), json!("none")),
        ("stop", json!("yes"), json!(2048), json!(2), json!("none")),
        ("stop", json!(true), json!(0), json!(2), json!("none")),
        ("stop", json!(true), json!(2048), json!(0), json!("none")),
        ("stop", json!(true), json!(2048), json!(2), json!("bridge")),
    ]
    .map(|(on_close, builder, memory, cpus, reach)| {
        json!({"onClose": on_close, "builderTools": builder, "newMemoryMb": memory,
               "newCpus": cpus, "newReach": reach, "newAgent": true})
    }) {
        let refused = shell.answer("system.set_preferences", &payload);
        assert_eq!(code(&refused), "bad-choice", "{payload}");
    }
    assert_eq!(shell.preferences().on_close(), "ask");
}

// --- where each request goes (ChannelRoutingTests) ------------------------------------

#[test]
fn test_a_host_action_never_reaches_the_daemon() {
    let room = Room::new();
    let (shell, _window) = with_a_window(&room);
    // No daemon at all, and it is still answered.
    let answered = shell.answer("host.closing", &json!({"choice": "stop"}));
    assert!(answered.ok, "{:?}", answered.error);
    for (action, payload) in [
        ("files.places", json!({})),
        ("files.list", json!({"path": "", "suffixes": ".bbx"})),
        ("system.locale", json!({})),
    ] {
        let answered = shell.answer(action, &payload);
        assert!(answered.ok, "{action}: {:?}", answered.error);
    }
}

#[test]
fn test_everything_else_still_goes_to_the_daemon() {
    let room = Room::new();
    let shell = shell(&room);
    for spec in beatbox_contract::ACTIONS
        .iter()
        .filter(|spec| spec.answered_by == beatbox_contract::AnsweredBy::Daemon)
    {
        let payload: serde_json::Map<String, Value> = spec
            .payload
            .iter()
            .map(|(field, _)| (beatbox_contract::door::camel(field), json!("")))
            .collect();
        let sent = shell.answer(spec.name, &Value::Object(payload));
        assert_eq!(code(&sent), "no-daemon", "{} was answered here", spec.name);
        let reason = sent.error.unwrap().reason.unwrap();
        assert_eq!(reason.code, "daemon.unreachable");
    }
}

/// A small file the person chose comes back as text; a folder, or nothing
/// there, is said so.
#[test]
fn test_a_chosen_file_is_read_by_the_window() {
    let room = Room::new();
    let shell = shell(&room);
    let file = std::env::temp_dir().join(format!("bb-read-{}.json", std::process::id()));
    std::fs::write(&file, "{\"format\": \"beatbox-network\"}").unwrap();
    let answered = shell.answer("files.read", &json!({"path": file.to_string_lossy()}));
    assert!(answered.ok, "{:?}", answered.error);
    assert_eq!(
        answered.data,
        Some(json!("{\"format\": \"beatbox-network\"}"))
    );
    let _ = std::fs::remove_file(&file);
    let missing = shell.answer("files.read", &json!({"path": file.to_string_lossy()}));
    assert_eq!(code(&missing), "no-such-file");
    let folder = shell.answer(
        "files.read",
        &json!({"path": std::env::temp_dir().to_string_lossy()}),
    );
    assert_eq!(code(&folder), "no-such-file");
}

/// It goes through the same door, so a bad payload fails the same way.
#[test]
fn test_a_host_action_answers_in_the_same_envelope_as_any_other() {
    let room = Room::new();
    let refused = shell(&room).answer("host.closing", &json!({}));
    assert_eq!(code(&refused), "bad-payload");
    assert_eq!(refused.error.unwrap().message, "host.closing needs: choice");
}

#[test]
fn test_a_camelcase_payload_is_understood() {
    let room = Room::new();
    let answered = shell(&room).answer("files.list", &json!({"path": "", "suffixes": ".bbx"}));
    assert!(answered.ok, "{:?}", answered.error);
}

// --- the window's end of the question (WindowAnswerTests) -----------------------------

fn two_running() -> Option<ClosingView> {
    Some(ClosingView {
        sandboxes: 2,
        installing: false,
    })
}

/// The X was never pressed. A click from a stale page must not close it.
#[test]
fn test_an_answer_nobody_asked_for_changes_nothing() {
    let closing = Closing::default();
    assert_eq!(closing.answer("stop").unwrap(), (false, Then::Stay));
    assert_eq!(closing.answered(), None);
}

#[test]
fn test_the_count_is_a_count_and_not_a_sentence() {
    let room = Room::new();
    let (shell, window) = with_a_window(&room);
    // Asked of a daemon that is not there: nothing running, nothing asked.
    assert_eq!(shell.close_requested(), Close::Now);
    assert!(window.pushed.lock().unwrap().is_empty());
    assert_eq!(
        Closing::default().requested(two_running()),
        Close::Ask(ClosingView {
            sandboxes: 2,
            installing: false
        })
    );
}

#[test]
fn test_a_runtime_that_cannot_answer_still_produces_a_question() {
    // Nothing to ask about when nothing could be read: the window closes,
    // because a window must always be able to close.
    assert_eq!(Closing::default().requested(None), Close::Now);
}

#[test]
fn test_cancelling_leaves_the_window_open_and_touches_nothing() {
    let closing = Closing::default();
    closing.requested(two_running());
    assert_eq!(closing.answer("cancel").unwrap(), (true, Then::Stay));
    assert_eq!(closing.answered(), None);
    assert!(!closing.asking());
}

#[test]
fn test_answering_twice_only_counts_once() {
    let closing = Closing::default();
    closing.requested(two_running());
    assert_eq!(closing.answer("leave").unwrap(), (true, Then::Go));
    assert_eq!(closing.answer("stop").unwrap(), (false, Then::Stay));
    assert_eq!(closing.answered(), Some("leave"));
}

/// The fallback, which leaves everything running: the one answer that
/// loses nothing (see closing.rs for why it is not a message box).
#[test]
fn test_a_page_that_never_answers_does_not_trap_the_window() {
    let closing = Closing::default();
    closing.requested(two_running());
    assert_eq!(closing.unanswered(), Then::Go);
    assert_eq!(closing.answered(), Some("leave"));
    assert_eq!(closing.requested(two_running()), Close::Now);
}

#[test]
fn test_the_fallback_does_not_fire_once_the_page_has_answered() {
    let closing = Closing::default();
    closing.requested(two_running());
    closing.answer("cancel").unwrap();
    assert_eq!(closing.unanswered(), Then::Stay);
}

#[test]
fn test_a_second_press_of_the_x_while_asking_asks_nothing_more() {
    let closing = Closing::default();
    closing.requested(two_running());
    assert_eq!(closing.requested(two_running()), Close::Wait);
}

// --- golden/contract/envelopes.json: the window's two closing answers ----------------

#[test]
fn the_closing_answers_are_the_pythons() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../golden/contract/envelopes.json");
    let golden: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let room = Room::new();
    let windowless = shell(&room);
    let sent = |envelope: Envelope| serde_json::to_value(envelope).unwrap();
    assert_eq!(
        sent(windowless.answer("host.closing", &json!({"choice": "stop"}))),
        golden["closing-without-a-window"]
    );
    assert_eq!(
        sent(windowless.answer("host.closing", &json!({"choice": "later"}))),
        golden["closing-with-a-bad-choice"]
    );
    // And every refusal at the door, through this half too.
    for spec in beatbox_contract::ACTIONS {
        if golden["unexpected"].get(spec.name).is_none() {
            continue;
        }
        assert_eq!(
            sent(windowless.answer(spec.name, &json!({"bogusField": 1, "zeta": 2}))),
            golden["unexpected"][spec.name],
            "{}",
            spec.name
        );
    }
}

// A machine's display window closes when the machine stops, shuts down from
// inside (the engine says `failed`, reason host.guest_shut_down) or is
// destroyed -- it showed "The machine has shut down." and stayed open.
#[test]
fn a_display_window_ends_with_its_machine() {
    use beatbox_shell::ended;
    use serde_json::json;
    for said in ["sandbox.stopped", "sandbox.failed", "sandbox.destroyed"] {
        assert_eq!(
            ended("sandbox", &json!({"name": said, "sandboxId": "sbx-1"})).as_deref(),
            Some("sbx-1")
        );
    }
    assert_eq!(
        ended(
            "sandbox",
            &json!({"name": "sandbox.started", "sandboxId": "sbx-1"})
        ),
        None
    );
    assert_eq!(
        ended(
            "job",
            &json!({"name": "sandbox.stopped", "sandboxId": "sbx-1"})
        ),
        None
    );
}

#[test]
fn what_an_appimage_added_is_taken_off_and_the_rest_stays() {
    let image = std::path::Path::new("/tmp/.mount_BeatBox");
    let kept = |value: &str| {
        outside_image(std::ffi::OsStr::new(value), image).map(|kept| kept.into_string().unwrap())
    };

    assert_eq!(
        kept("/tmp/.mount_BeatBox/usr/bin/:/tmp/.mount_BeatBox/bin/:/usr/bin:/bin").as_deref(),
        Some("/usr/bin:/bin"),
    );
    // Unset before AppRun: the trailing `:` must not become the current folder.
    assert_eq!(
        kept("/tmp/.mount_BeatBox/usr/lib/:/tmp/.mount_BeatBox/lib64/:").as_deref(),
        Some("")
    );
    assert_eq!(
        kept("/tmp/.mount_BeatBox//usr/lib/gtk-3.0").as_deref(),
        Some("")
    );
    assert_eq!(kept("/tmp/.mount_BeatBox").as_deref(), Some(""));
    // Not ours: left exactly as it was.
    assert_eq!(kept("/usr/local/bin:/usr/bin"), None);
    assert_eq!(kept("/tmp/.mount_BeatBox2/usr/lib"), None);
    assert_eq!(kept(":0"), None);
}
