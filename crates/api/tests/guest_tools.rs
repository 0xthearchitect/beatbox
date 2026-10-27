//! The Windows guest tools: fetched once, checked, and plugged in.
//!
//! Never touches the network: the download is served from memory and the
//! pinned hash is of those bytes.

mod support;

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use beatbox_api::guest_tools::{Fetch, Pinned, Tools};
use beatbox_api::jobs::{DONE, FAILED};
use beatbox_api::{Api, ApiParts};
use beatbox_contract::Job;
use beatbox_engine::SandboxConfig;
use serde_json::json;
use sha2::{Digest, Sha256};
use support::{Failing, Room, Serving, call, code, data, job, parts, settle};

fn good() -> Vec<u8> {
    let mut bytes = b"MZ".to_vec();
    for _ in 0..1000 {
        bytes.extend_from_slice(b"guest tools, honestly");
    }
    bytes
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn fetching(room: &Room, fetch: Arc<dyn Fetch>) -> Api {
    let good = good();
    Api::open(ApiParts {
        tools: Tools {
            pinned: Pinned {
                sha256: hex(&good),
                size: good.len() as u64,
                ..Pinned::default()
            },
            fetch,
        },
        ..parts(room.path())
    })
}

fn finished(api: &Api) -> Job {
    let started = job(call(api, "tools.fetch", json!({})));
    let done = settle(api, &started.id, Duration::from_secs(5));
    assert_ne!(
        done.state,
        beatbox_api::jobs::RUNNING,
        "the download never finished"
    );
    done
}

fn everything_under(folder: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    if let Ok(entries) = fs::read_dir(folder) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(everything_under(&path));
            } else {
                found.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    found
}

#[test]
fn test_nothing_is_here_until_it_is_asked_for() {
    let room = Room::new("tools");
    let api = fetching(&room, Arc::new(Serving(good())));
    let status = data(call(&api, "tools.status", json!({})));
    assert_eq!(status["ready"], false);
    assert_eq!(status["bytes"], good().len());
}

/// The folder is what the guest sees: one file, and no half-written
/// leftovers beside it.
#[test]
fn test_the_right_file_is_kept_and_the_folder_holds_nothing_else() {
    let room = Room::new("tools");
    let api = fetching(&room, Arc::new(Serving(good())));
    assert_eq!(finished(&api).state, DONE);
    assert!(api.tools_ready());
    let names: Vec<String> = fs::read_dir(api.tools_folder())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, [Pinned::default().filename]);
}

#[test]
fn test_a_file_that_does_not_match_the_hash_is_deleted_unused() {
    let room = Room::new("tools");
    let api = fetching(
        &room,
        Arc::new(Serving(b"MZ something else entirely".to_vec())),
    );
    let done = finished(&api);
    assert_eq!(done.state, FAILED);
    assert_eq!(done.reason.unwrap().code, "tools.wrong_file");
    assert!(!api.tools_ready());
    let left: Vec<String> = everything_under(&room.join("tools"))
        .into_iter()
        .filter(|name| name.contains(".exe"))
        .collect();
    assert!(left.is_empty(), "nothing left to plug in: {left:?}");
}

#[test]
fn test_no_network_says_so() {
    let room = Room::new("tools");
    let api = fetching(&room, Arc::new(Failing));
    let done = finished(&api);
    assert_eq!(done.state, FAILED);
    assert_eq!(done.reason.unwrap().code, "tools.download_failed");
    assert!(!api.tools_ready());
}

#[test]
fn test_once_is_enough() {
    let room = Room::new("tools");
    let api = fetching(&room, Arc::new(Serving(good())));
    finished(&api);
    assert_eq!(code(&call(&api, "tools.fetch", json!({}))), "already");
}

fn with_a_machine(room: &Room) -> (Api, beatbox_engine::Sandbox) {
    let api = support::open(room.path());
    let made = api
        .runtime()
        .create(SandboxConfig::new("mock").unwrap().named("m"))
        .unwrap();
    (api, made)
}

#[test]
fn test_before_the_download_the_answer_is_no_and_says_why() {
    let room = Room::new("tools");
    let (api, made) = with_a_machine(&room);
    let outcome = data(call(&api, "tools.insert", json!({"sandboxId": made.id()})));
    assert_eq!(outcome["ok"], false);
    assert_eq!(outcome["reason"]["code"], "tools.not_here");
}

/// A sandbox without `media.insert` (the mock, no screen, hostile) is left
/// alone, silently.
#[test]
fn test_a_machine_that_cannot_take_them_is_not_asked() {
    let room = Room::new("tools");
    let (api, made) = with_a_machine(&room);
    fs::create_dir_all(api.tools_folder()).unwrap();
    fs::write(api.tools_folder().join(Pinned::default().filename), good()).unwrap();
    assert!(api.tools_ready());
    let before = api.runtime().events().published();
    api.offer_tools(&made);
    assert_eq!(api.runtime().events().published(), before, "it was asked");
}
