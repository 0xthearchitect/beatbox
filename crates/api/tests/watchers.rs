//! Commands kept, and watchers run: through the same door the page uses.
//!
//! The machine is a provider of its own here, whose `process.shell` answers
//! from a queue, so a watcher's three ways of telling -- a changed answer, a
//! failure, a text that appears -- are each seen happen on the bus.

mod support;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use beatbox_api::Api;
use beatbox_engine::{
    Action, Data, Isolation, Observation, ProviderError, ProviderSpec, SandboxProvider, data,
};
use serde_json::{Value, json};
use support::{Room, call, code, data, eventually, parts};

type Answers = Arc<Mutex<VecDeque<(String, i64)>>>;

struct Shell(Answers);

impl SandboxProvider for Shell {
    fn create(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    fn start(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    fn stop(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    fn destroy(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    fn observe(&mut self) -> Observation {
        Observation {
            alive: Some(true),
            ..Observation::default()
        }
    }
    fn perform(&mut self, _action: &Action) -> Result<Data, ProviderError> {
        let (stdout, exit) = self.0.lock().unwrap().pop_front().unwrap_or_default();
        Ok(data!({"stdout": stdout, "stderr": "", "exit_code": exit}))
    }
}

fn open(room: &Room, answers: &Answers) -> Api {
    let mut parts = parts(room.path());
    let queue = Arc::clone(answers);
    parts.types.push(ProviderSpec::new(
        "shell",
        "1",
        &["process.shell"],
        Isolation::None,
        move |_, _, _| Box::new(Shell(Arc::clone(&queue))),
    ));
    Api::open(parts)
}

fn running(api: &Api) -> String {
    let made = data(call(
        api,
        "sandbox.create",
        json!({"type": "shell", "name": "box", "cpu": 0, "memory": 0, "settings": {}, "uses": {}}),
    ));
    let id = made["id"].as_str().unwrap().to_owned();
    data(call(
        api,
        "sandbox.transition",
        json!({"id": id, "transition": "start"}),
    ));
    id
}

fn told(api: &Api) -> Vec<Value> {
    api.history(0)
        .into_iter()
        .filter(|event| event.name() == "watcher.changed")
        .map(|event| Value::Object(event.data().clone()))
        .collect()
}

#[test]
fn a_command_is_kept_changed_and_forgotten() {
    let room = Room::new("commands");
    let api = open(&room, &Answers::default());
    let kept = data(call(
        &api,
        "commands.save",
        json!({"command": {"name": "Reach a node", "linux": "ping -c 4 {node}", "blanks": {"node": {"kind": "node"}}}}),
    ));
    let id = kept[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(kept[0]["machine"], "", "no machine is every machine");
    assert_eq!(kept[0]["timeout"], 30);
    let kept = data(call(
        &api,
        "commands.save",
        json!({"command": {"id": id, "name": "Reach a node", "linux": "ping -c 2 {node}", "machine": "sbx-1"}}),
    ));
    assert_eq!(kept.as_array().unwrap().len(), 1);
    assert_eq!(kept[0]["linux"], "ping -c 2 {node}");
    assert_eq!(kept[0]["machine"], "sbx-1");
    // Kept in a file: a daemon opened again finds it.
    drop(api);
    let api = open(&room, &Answers::default());
    assert_eq!(
        data(call(&api, "commands.list", json!({})))[0]["id"],
        id.as_str()
    );
    assert_eq!(
        data(call(&api, "commands.forget", json!({"id": id}))),
        json!([])
    );
    let refused = call(
        &api,
        "commands.save",
        json!({"command": {"name": "nothing to run"}}),
    );
    assert_eq!(code(&refused), "bad-payload");
}

#[test]
fn a_watcher_tells_when_a_text_appears_and_when_it_fails() {
    let room = Room::new("watch-once");
    let answers = Answers::default();
    answers
        .lock()
        .unwrap()
        .extend([("invoice.pdf.exe\n".to_owned(), 0)]);
    let api = open(&room, &answers);
    let id = running(&api);
    data(call(
        &api,
        "watchers.save",
        json!({"watcher": {"sandboxId": id, "line": "ls ~/Downloads", "tell": "contains", "text": ".exe"}}),
    ));
    assert!(eventually(Duration::from_secs(5), || !told(&api).is_empty()));
    let said = &told(&api)[0];
    assert_eq!(said["because"], "matched");
    assert!(said["output"].as_str().unwrap().contains("invoice"));
    let listed = data(call(&api, "watchers.list", json!({})));
    assert_eq!(listed[0]["state"], "watching");
    assert_eq!(listed[0]["runs"], 1);

    // A second one, on failure: the queue is empty, so the answer is exit 0
    // with nothing -- then a failure is queued.
    answers.lock().unwrap().push_back((String::new(), 2));
    data(call(
        &api,
        "watchers.save",
        json!({"watcher": {"sandboxId": id, "line": "systemctl is-active web", "tell": "fails"}}),
    ));
    assert!(eventually(Duration::from_secs(5), || told(&api).len() == 2));
    assert_eq!(told(&api)[1]["because"], "failed");
}

#[test]
fn a_watcher_tells_when_its_answer_changes_and_not_before() {
    let room = Room::new("watch-change");
    let answers = Answers::default();
    answers.lock().unwrap().extend([
        ("22\n631\n".to_owned(), 0),
        ("22\n631\n8080\n".to_owned(), 0),
    ]);
    let api = open(&room, &answers);
    let id = running(&api);
    let kept = data(call(
        &api,
        "watchers.save",
        json!({"watcher": {"sandboxId": id, "line": "ss -tln", "every": 1}}),
    ));
    assert_eq!(kept[0]["every"], 5, "never more often than every 5 s");
    // The first answer is only what it is; the second is news.
    assert!(eventually(Duration::from_secs(9), || !told(&api).is_empty()));
    assert_eq!(told(&api)[0]["because"], "changed");
    assert!(told(&api)[0]["output"].as_str().unwrap().contains("8080"));
}

#[test]
fn a_watcher_waits_while_its_machine_is_off_and_is_kept() {
    let room = Room::new("watch-off");
    let api = open(&room, &Answers::default());
    let made = data(call(
        &api,
        "sandbox.create",
        json!({"type": "shell", "name": "box", "cpu": 0, "memory": 0, "settings": {}, "uses": {}}),
    ));
    let id = made["id"].as_str().unwrap().to_owned();
    let kept = data(call(
        &api,
        "watchers.save",
        json!({"watcher": {"sandboxId": id, "line": "who", "paused": true}}),
    ));
    let watcher = kept[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(kept[0]["state"], "paused");
    data(call(
        &api,
        "watchers.save",
        json!({"watcher": {"id": watcher, "paused": false}}),
    ));
    assert!(eventually(Duration::from_secs(5), || {
        data(call(&api, "watchers.list", json!({})))[0]["state"] == "waiting"
    }));
    assert_eq!(data(call(&api, "watchers.list", json!({})))[0]["runs"], 0);
    drop(api);
    let api = open(&room, &Answers::default());
    assert_eq!(
        data(call(&api, "watchers.list", json!({})))[0]["line"],
        "who"
    );
    assert_eq!(
        data(call(&api, "watchers.forget", json!({"id": watcher}))),
        json!([])
    );
}
