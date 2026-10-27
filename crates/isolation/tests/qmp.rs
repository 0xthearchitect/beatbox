//! The two channels a guest is driven through, against a server that is not
//! QEMU.
//!
//! Protocol, not hypervisor: the greeting, errors as data, events mid
//! conversation, and failures that are printed instead of returned. The
//! server is a real socket with a thread behind it.

mod support;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use beatbox_isolation::endpoint::Stream;
use beatbox_isolation::qmp::{Channel, GuestAgent, Monitor};
use serde_json::{Value, json};
use support::Room;

type Answer = Arc<dyn Fn(&Value) -> Value + Send + Sync>;

/// One socket, one connection, and a script of answers. Loopback, with the
/// port recorded at the path -- how a channel looks on Windows -- so every
/// connect here goes through the record and the file runs on both systems.
struct FakeGuest {
    path: PathBuf,
    asked: Arc<Mutex<Vec<Value>>>,
    _room: Room,
}

impl FakeGuest {
    fn new(
        name: &str,
        greeting: Option<Value>,
        answers: HashMap<&'static str, Value>,
        pushes: HashMap<&'static str, Vec<Value>>,
    ) -> Self {
        let answers: HashMap<String, Answer> = answers
            .into_iter()
            .map(|(k, v)| (k.to_owned(), Arc::new(move |_: &Value| v.clone()) as Answer))
            .collect();
        Self::scripted(name, greeting, answers, pushes)
    }

    fn scripted(
        name: &str,
        greeting: Option<Value>,
        answers: HashMap<String, Answer>,
        pushes: HashMap<&'static str, Vec<Value>>,
    ) -> Self {
        let room = Room::new("qmp");
        let path = room.join(name);
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        std::fs::write(
            &path,
            format!("tcp:127.0.0.1:{}", server.local_addr().unwrap().port()),
        )
        .unwrap();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let heard = Arc::clone(&asked);
        thread::spawn(move || {
            let Ok((connection, _)) = server.accept() else {
                return;
            };
            let mut writer = connection.try_clone().unwrap();
            if let Some(greeting) = greeting {
                writeln!(writer, "{greeting}").unwrap();
            }
            for line in BufReader::new(connection).lines() {
                let Ok(line) = line else { return };
                let request: Value = serde_json::from_str(&line).unwrap();
                heard.lock().unwrap().push(request.clone());
                let name = request["execute"].as_str().unwrap_or_default().to_owned();
                for event in pushes.get(name.as_str()).into_iter().flatten() {
                    let _ = writeln!(writer, "{event}");
                }
                let answer = answers
                    .get(&name)
                    .map_or_else(|| json!({"return": {}}), |answer| answer(&request));
                if writeln!(writer, "{answer}").is_err() {
                    return;
                }
            }
        });
        Self {
            path,
            asked,
            _room: room,
        }
    }

    fn asked(&self) -> Vec<Value> {
        self.asked.lock().unwrap().clone()
    }
}

fn greeting() -> Option<Value> {
    Some(json!({"QMP": {"version": {"qemu": {"major": 9}}, "capabilities": []}}))
}

fn answers(pairs: &[(&'static str, Value)]) -> HashMap<&'static str, Value> {
    pairs.iter().cloned().collect()
}

const TWO: Duration = Duration::from_secs(2);

mod channel_tests {
    use super::*;

    #[test]
    fn a_socket_that_never_appears_says_where_it_was_looking() {
        let room = Room::new("qmp");
        let missing = room.join("nothing.sock");
        let Err(refusal) = Channel::connect(&missing, Duration::from_millis(200)) else {
            panic!("a channel to nothing opened");
        };
        assert_eq!(refusal.code(), "machine.no_channel");
        assert_eq!(refusal.reason.values["path"], missing.display().to_string());
    }

    /// The bug this prevents: `query-status` returning a SHUTDOWN event
    /// because it arrived first, and the caller reading `status` off it.
    #[test]
    fn events_pushed_mid_conversation_do_not_become_the_answer() {
        let guest = FakeGuest::new(
            "m.sock",
            greeting(),
            answers(&[("query-status", json!({"return": {"status": "running"}}))]),
            HashMap::from([(
                "query-status",
                vec![json!({"event": "RESUME"}), json!({"event": "STOP"})],
            )]),
        );
        let mut monitor = Monitor::connect(&guest.path, TWO).unwrap();
        assert_eq!(monitor.status().unwrap(), "running");
        let events: Vec<Value> = monitor
            .events(Duration::ZERO)
            .into_iter()
            .map(|e| e["event"].clone())
            .collect();
        assert_eq!(events, [json!("RESUME"), json!("STOP")]);
    }
}

mod monitor_tests {
    use super::*;

    fn guest(pairs: &[(&'static str, Value)]) -> FakeGuest {
        FakeGuest::new("monitor.sock", greeting(), answers(pairs), HashMap::new())
    }

    /// QMP refuses every command until capabilities are negotiated, so a
    /// monitor handed out by connect() is always usable.
    #[test]
    fn the_greeting_is_answered_before_a_caller_sees_anything() {
        let guest = guest(&[("query-status", json!({"return": {"status": "paused"}}))]);
        let mut monitor = Monitor::connect(&guest.path, TWO).unwrap();
        assert_eq!(monitor.status().unwrap(), "paused");
        assert_eq!(guest.asked()[0]["execute"], "qmp_capabilities");
    }

    #[test]
    fn something_that_is_not_qmp_is_refused_rather_than_talked_to() {
        let guest = FakeGuest::new(
            "monitor.sock",
            Some(json!({"hello": "i am not qemu"})),
            HashMap::new(),
            HashMap::new(),
        );
        let Err(refusal) = Monitor::connect(&guest.path, TWO) else {
            panic!("something that is not QMP was talked to");
        };
        assert_eq!(refusal.code(), "machine.channel_noise");
    }

    #[test]
    fn an_error_arrives_as_a_refusal_naming_the_command() {
        let guest = guest(&[(
            "stop",
            json!({"error": {"class": "GenericError", "desc": "not running"}}),
        )]);
        let mut monitor = Monitor::connect(&guest.path, TWO).unwrap();
        let refusal = monitor.pause().unwrap_err();
        assert_eq!(refusal.code(), "machine.refused_command");
        assert_eq!(refusal.reason.values["command"], "stop");
        assert_eq!(refusal.reason.values["detail"], "not running");
        assert_eq!(refusal.reason.values.len(), 2);
    }

    #[test]
    fn pause_and_resume_are_the_monitor_words_for_them() {
        let guest = guest(&[]);
        let mut monitor = Monitor::connect(&guest.path, TWO).unwrap();
        monitor.pause().unwrap();
        monitor.resume().unwrap();
        let asked: Vec<Value> = guest.asked()[1..]
            .iter()
            .map(|a| a["execute"].clone())
            .collect();
        assert_eq!(asked, [json!("stop"), json!("cont")]);
    }

    /// `savevm` answers `{"return": "..."}` and puts the failure in the
    /// string. A caller checking only for a QMP error records a snapshot that
    /// was never taken -- and then restores it.
    #[test]
    fn a_snapshot_that_failed_by_printing_is_still_a_failure() {
        let guest = guest(&[(
            "human-monitor-command",
            json!({"return": "Error: Device has no medium\r\n"}),
        )]);
        let mut monitor = Monitor::connect(&guest.path, TWO).unwrap();
        let refusal = monitor.snapshot_save("before").unwrap_err();
        assert_eq!(refusal.code(), "machine.snapshot_failed");
        assert!(refusal.reason.values["detail"].contains("no medium"));
    }

    #[test]
    fn a_snapshot_that_said_nothing_worked() {
        let guest = guest(&[("human-monitor-command", json!({"return": ""}))]);
        let mut monitor = Monitor::connect(&guest.path, TWO).unwrap();
        monitor.snapshot_save("before").unwrap();
        let asked = guest.asked();
        assert_eq!(
            asked.last().unwrap()["arguments"]["command-line"],
            "savevm \"before\""
        );
    }

    /// `savevm before the attack` is three arguments to the monitor. Quoted,
    /// it works, and the tag really is the whole sentence.
    #[test]
    fn a_save_point_can_be_called_what_a_person_would_call_it() {
        let guest = guest(&[("human-monitor-command", json!({"return": ""}))]);
        let mut monitor = Monitor::connect(&guest.path, TWO).unwrap();
        monitor.snapshot_save("before the attack").unwrap();
        assert_eq!(
            guest.asked().last().unwrap()["arguments"]["command-line"],
            "savevm \"before the attack\""
        );
        monitor.snapshot_load("before the attack").unwrap();
        assert_eq!(
            guest.asked().last().unwrap()["arguments"]["command-line"],
            "loadvm \"before the attack\""
        );
    }

    /// No specified escaping in a line-oriented text protocol, so guessing at
    /// one is how a tag becomes a command.
    #[test]
    fn a_name_the_monitor_cannot_carry_is_refused_rather_than_escaped() {
        let guest = guest(&[("human-monitor-command", json!({"return": ""}))]);
        let mut monitor = Monitor::connect(&guest.path, TWO).unwrap();
        for bad in ["say \"hi\"", "back\\slash", "two\nlines"] {
            let refusal = monitor.snapshot_save(bad).unwrap_err();
            assert_eq!(refusal.code(), "machine.bad_snapshot_name", "{bad}");
        }
    }

    /// It is the one command whose purpose is to end the thing that would
    /// have replied.
    #[test]
    fn quitting_a_guest_that_leaves_without_answering_is_not_a_failure() {
        let guest = guest(&[]);
        let mut monitor = Monitor::connect(&guest.path, TWO).unwrap();
        monitor.channel_mut().close();
        monitor.quit().unwrap();
    }
}

mod guest_agent_tests {
    use super::*;

    fn guest(pairs: &[(&'static str, Value)]) -> FakeGuest {
        FakeGuest::new("agent.sock", None, answers(pairs), HashMap::new())
    }

    fn b64(text: &str) -> String {
        // Written out rather than taken from the crate under test.
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bytes = text.as_bytes();
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
            for i in 0..4 {
                out.push(if i <= chunk.len() {
                    A[(n >> (18 - 6 * i) & 63) as usize] as char
                } else {
                    '='
                });
            }
        }
        out
    }

    /// `guest-file-open` answers with an integer, and reading a file through
    /// a handle of `{}` reads nothing, silently.
    #[test]
    fn a_bare_handle_is_not_flattened_into_a_dictionary() {
        let content = "127.0.0.1 localhost\n";
        let guest = guest(&[
            ("guest-file-open", json!({"return": 7})),
            (
                "guest-file-read",
                json!({"return": {"count": content.len(), "buf-b64": b64(content), "eof": true}}),
            ),
            ("guest-file-close", json!({"return": {}})),
        ]);
        let mut agent = GuestAgent::connect(&guest.path, TWO).unwrap();
        assert_eq!(agent.read_file("/etc/hosts", 1 << 20).unwrap(), content);
        assert_eq!(guest.asked()[1]["arguments"]["handle"], 7);
    }

    #[test]
    fn a_command_comes_back_with_its_code_and_its_output() {
        let guest = guest(&[
            ("guest-exec", json!({"return": {"pid": 412}})),
            (
                "guest-exec-status",
                json!({"return": {"exited": true, "exitcode": 3, "out-data": b64("hello\n")}}),
            ),
        ]);
        let mut agent = GuestAgent::connect(&guest.path, TWO).unwrap();
        let result = agent.run("/bin/echo", &[json!("hello")], None).unwrap();
        assert_eq!(
            (&result["pid"], &result["exit_code"], &result["stdout"]),
            (&json!(412), &json!(3), &json!("hello\n"))
        );
    }

    /// The agent has no way to kill it, so saying so would be a lie.
    #[test]
    fn a_command_that_outlives_the_deadline_is_not_claimed_to_have_stopped() {
        let guest = guest(&[
            ("guest-exec", json!({"return": {"pid": 9}})),
            ("guest-exec-status", json!({"return": {"exited": false}})),
        ]);
        let mut agent = GuestAgent::connect(&guest.path, TWO).unwrap();
        let refusal = agent
            .run(
                "/bin/sleep",
                &[json!("600")],
                Some(Duration::from_millis(300)),
            )
            .unwrap_err();
        assert_eq!(refusal.code(), "machine.command_outlived_us");
        assert_eq!(refusal.reason.values["pid"], "9");
    }

    #[test]
    fn a_guest_with_nothing_listening_answers_no_to_ping() {
        let guest = guest(&[("guest-ping", json!({"error": {"desc": "no such command"}}))]);
        let mut agent = GuestAgent::connect(&guest.path, TWO).unwrap();
        assert!(!agent.ping());
    }

    #[test]
    fn interfaces_come_back_as_a_list_and_not_as_a_dictionary() {
        let guest = guest(&[(
            "guest-network-get-interfaces",
            json!({"return": [{"name": "eth0", "hardware-address": "52:54:00:12:34:56",
                "ip-addresses": [{"ip-address": "10.0.2.15", "prefix": 24, "ip-address-type": "ipv4"}]}]}),
        )]);
        let mut agent = GuestAgent::connect(&guest.path, TWO).unwrap();
        let interfaces = agent.interfaces().unwrap();
        assert_eq!(interfaces.len(), 1);
        assert_eq!(interfaces[0]["name"], "eth0");
    }
}

/// A slow `savevm` and the screen's refresh on one socket: the answer to the
/// one that gave up must not be read as someone else's.
mod channel_safety_tests {
    use super::*;

    /// Two ends of a connection: ours as a channel, the other raw.
    fn pair(timeout: Duration) -> (Channel, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let ours = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (theirs, _) = listener.accept().unwrap();
        (Channel::new(Stream::Tcp(ours), timeout), theirs)
    }

    fn read_line(server: &mut TcpStream, buffer: &mut Vec<u8>) -> Value {
        while !buffer.contains(&b'\n') {
            let mut chunk = [0u8; 4096];
            let n = server.read(&mut chunk).unwrap();
            buffer.extend_from_slice(&chunk[..n]);
        }
        let end = buffer.iter().position(|b| *b == b'\n').unwrap();
        let line: Vec<u8> = buffer.drain(..=end).collect();
        serde_json::from_slice(&line[..line.len() - 1]).unwrap()
    }

    /// Every request carries an id QEMU echoes, and an answer with another
    /// request's id is dropped.
    #[test]
    fn an_answer_that_arrives_after_its_asker_gave_up_is_not_taken_by_the_next() {
        let (mut channel, mut server) = pair(Duration::from_millis(200));
        thread::spawn(move || {
            let mut buffer = Vec::new();
            let first = read_line(&mut server, &mut buffer);
            thread::sleep(Duration::from_millis(400)); // longer than the asker waits
            let second = read_line(&mut server, &mut buffer);
            for request in [first, second] {
                let mut answer = json!({"return": request["execute"]});
                if let Some(id) = request.get("id") {
                    answer["id"] = id.clone();
                }
                writeln!(server, "{answer}").unwrap();
            }
        });
        assert!(
            channel
                .exchange(&json!({"execute": "savevm"}), None)
                .is_err()
        );
        thread::sleep(Duration::from_millis(300));
        let answer = channel
            .exchange(
                &json!({"execute": "query-status"}),
                Some(Duration::from_secs(5)),
            )
            .unwrap();
        assert_eq!(Value::Object(answer), json!({"return": "query-status"}));
    }

    #[test]
    fn a_long_command_may_wait_longer_than_the_channel_does() {
        let (mut channel, mut server) = pair(Duration::from_millis(100));
        thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            let _ = server.read(&mut chunk);
            thread::sleep(Duration::from_millis(300));
            server.write_all(b"{\"return\": \"\"}\n").unwrap();
        });
        let answer = channel
            .exchange(&json!({"execute": "savevm"}), Some(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(Value::Object(answer), json!({"return": ""}));
        // and the channel's own deadline is back afterwards
        assert_eq!(
            channel.stream().read_timeout().unwrap(),
            Some(Duration::from_millis(100))
        );
    }

    /// A stale answer sits in the stream. A fresh connection must not read it
    /// as the answer to its own first question.
    #[test]
    fn resync_throws_away_what_an_earlier_client_left_behind() {
        let (mut channel, mut server) = pair(Duration::from_secs(5));
        thread::spawn(move || {
            server
                .write_all(b"{\"return\": \"stale, from somebody else\"}\n")
                .unwrap();
            let mut buffer = Vec::new();
            while !String::from_utf8_lossy(&buffer).contains("guest-sync-delimited") {
                let mut chunk = [0u8; 4096];
                let n = server.read(&mut chunk).unwrap();
                buffer.extend_from_slice(&chunk[..n]);
            }
            let after = buffer.iter().rposition(|b| *b == 0xff).unwrap() + 1;
            let line = &buffer[after..];
            let end = line.iter().position(|b| *b == b'\n').unwrap();
            let request: Value = serde_json::from_slice(&line[..end]).unwrap();
            let token = request["arguments"]["id"].clone();
            server.write_all(b"\xff").unwrap();
            writeln!(server, "{}", json!({"return": token})).unwrap();
            let mut chunk = [0u8; 4096];
            let _ = server.read(&mut chunk);
            server.write_all(b"{\"return\": \"fresh\"}\n").unwrap();
        });
        assert!(channel.resync(Duration::from_secs(5)));
        let answer = channel
            .exchange(&json!({"execute": "guest-ping"}), None)
            .unwrap();
        assert_eq!(Value::Object(answer), json!({"return": "fresh"}));
    }

    #[test]
    fn resync_with_nobody_inside_is_false_not_a_hang() {
        let (mut channel, _server) = pair(Duration::from_secs(5));
        assert!(!channel.resync(Duration::from_millis(200)));
    }
}
