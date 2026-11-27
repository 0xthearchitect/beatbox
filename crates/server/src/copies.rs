//! Copies of a service's data, kept on this computer in the Server's room,
//! so they outlive the machine the service runs in.
//!
//! A copy is `<copy>.copy`, the bytes, and `<copy>.json`, what it is. The
//! bytes arrive as `<copy>.part` and are renamed once whole and checked; the
//! `.json` is written last. A `.part` with no `.json` is a copy that did not
//! finish, said as such and never put back.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use beatbox_engine::{Action, Data, Reason, data, reason};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{ServerSandbox, base64, text};
use crate::definition::OwnData;
use crate::placeholders::fill;
use crate::process;

/// Under the Server's room: a folder per service name, so a service removed
/// and made again finds its copies.
const COPIES: &str = "copies";

/// A dump or a pack is given this long inside the guest, in seconds.
const RUN_SECONDS: u32 = 600;

/// Raw bytes sent to the guest per write: about 1 MB once in base64, which
/// the agent takes in one message.
const PUSH_BYTES: usize = 768 * 1024;

/// What a copy is, next to its bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Made {
    copy: String,
    /// Seconds since 1970.
    made: u64,
    #[serde(default)]
    note: String,
    /// The copy that was put back right after this one was made, when it
    /// was made for that.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    before: String,
    definition: String,
    definition_version: String,
    /// The machine it was copied from, as the person called it.
    machine: String,
    /// `files` (packed, the service stopped) or `dump` (by the program).
    kind: String,
    /// What the dump said it was made by: PostgreSQL's version.
    #[serde(default)]
    made_with: String,
    bytes: u64,
    sha256: String,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// `key=` from `key=value` lines a guest script printed.
fn said(data: &Data, key: &str) -> String {
    let prefix = format!("{key}=");
    data.get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn stderr_tail(data: &Data) -> String {
    let text = data
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(6)..].join("\n")
}

/// A copy's name is the second it was made: digits, nothing a path can use.
fn is_copy_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 20 && name.bytes().all(|b| b.is_ascii_digit())
}

fn stored(error: &std::io::Error) -> Reason {
    if error.kind() == std::io::ErrorKind::StorageFull {
        reason!("server.copy_no_room")
    } else {
        reason!("server.copy_not_saved", detail = error)
    }
}

fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// A database's copy is nobody else's to read on this computer.
fn private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

impl ServerSandbox {
    fn copies_of(&self, service: &str) -> PathBuf {
        self.room.join(COPIES).join(service)
    }

    /// The service's data as the catalog's definition says it: one made
    /// before its definition said so learns it too.
    fn own_data(&self, at: usize) -> Option<OwnData> {
        let id = &self.services[at].definition.id;
        self.catalog()
            .0
            .into_iter()
            .find(|(definition, _)| &definition.id == id)
            .and_then(|(definition, _)| definition.data)
            .or_else(|| self.services[at].definition.data.clone())
    }

    /// The machine up and Linux, or why not.
    fn copy_machine(&self, at: usize) -> Result<(), Reason> {
        let service = self.services[at].name.clone();
        if !self.host_up() {
            return Err(reason!("server.copy_machine_off", service = service));
        }
        if let Some(state) = self.peers.state("host")
            && state.fact("family").and_then(Value::as_str) == Some("windows")
        {
            return Err(reason!("server.copy_linux_only", service = service));
        }
        Ok(())
    }

    fn data_paths(&self, at: usize, data: &OwnData) -> Vec<String> {
        let values = self.services[at].values();
        data.paths.iter().map(|path| fill(path, &values)).collect()
    }

    fn stage_for(&self, at: usize, copy: &str) -> String {
        format!("{}/{}-{copy}", process::COPY_STAGE, self.services[at].id)
    }

    fn read_made(folder: &Path, copy: &str) -> Option<Made> {
        let bytes = std::fs::read(folder.join(format!("{copy}.json"))).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// `server.copies`: its data (where, and its size when the machine is
    /// up) and its copies, newest first, unfinished ones said apart.
    pub(super) fn copies(&mut self, action: &Action) -> Result<Data, Reason> {
        let at = self.find(&text(action, "service"))?;
        let name = self.services[at].name.clone();
        let Some(data) = self.own_data(at) else {
            return Ok(data!({"service": name, "data": null, "copies": [], "unfinished": []}));
        };
        let paths = self.data_paths(at, &data);
        let mut size = Value::Null;
        if self.copy_machine(at).is_ok() {
            let asked: Vec<&str> = paths.iter().map(String::as_str).collect();
            if let Ok(answer) = process::sh(&self.peers, process::SIZE, &asked)
                && let Ok(kb) = answer
                    .get("stdout")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .parse::<u64>()
            {
                size = json!(kb * 1024);
            }
        }
        let folder = self.copies_of(&name);
        let mut made: Vec<Made> = Vec::new();
        let mut unfinished: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if let Some(copy) = file.strip_suffix(".json") {
                made.extend(Self::read_made(&folder, copy));
            } else if let Some(copy) = file.strip_suffix(".part")
                && !folder.join(format!("{copy}.json")).exists()
            {
                unfinished.push(copy.to_owned());
            }
        }
        made.sort_by_key(|each| std::cmp::Reverse(each.made));
        unfinished.sort();
        Ok(data!({
            "service": name,
            "data": {
                "paths": paths,
                "bytes": size,
                "running": data.dump.is_some(),
            },
            "copies": made,
            "unfinished": unfinished,
        }))
    }

    /// `server.copy`: copy its data now, onto this computer.
    pub(super) fn copy(&mut self, action: &Action) -> Result<Data, Reason> {
        let at = self.find(&text(action, "service"))?;
        let note = text(action, "note");
        let made = self.make_copy(at, note.trim(), "")?;
        Ok(data!({"service": self.services[at].name, "copy": made}))
    }

    fn make_copy(&mut self, at: usize, note: &str, before: &str) -> Result<Made, Reason> {
        let name = self.services[at].name.clone();
        let data = self
            .own_data(at)
            .ok_or_else(|| reason!("server.no_data", service = name))?;
        self.copy_machine(at)?;
        let folder = self.copies_of(&name);
        private_dir(&folder).map_err(|error| stored(&error))?;
        let mut stamp = now();
        while folder.join(format!("{stamp}.part")).exists()
            || folder.join(format!("{stamp}.json")).exists()
        {
            stamp += 1;
        }
        let copy = stamp.to_string();
        let stage = self.stage_for(at, &copy);
        let result = self.copy_through(at, &data, &stage, &folder, &copy);
        let _ = process::sh(&self.peers, process::UNSTAGE, &[&stage]);
        let mut made = result?;
        made.note = note.chars().take(200).collect();
        before.clone_into(&mut made.before);
        let text = serde_json::to_string_pretty(&made).expect("a copy serialises");
        let fresh = folder.join(format!("{copy}.json.new"));
        private_file(&fresh)
            .and_then(|mut file| file.write_all(text.as_bytes()))
            .and_then(|()| std::fs::rename(&fresh, folder.join(format!("{copy}.json"))))
            .map_err(|error| stored(&error))?;
        self.emit(
            "service.copied",
            data!({"service": name, "copy": copy, "bytes": made.bytes}),
        );
        Ok(made)
    }

    /// Make the copy in the guest's stage, then bring it here.
    fn copy_through(
        &mut self,
        at: usize,
        data: &OwnData,
        stage: &str,
        folder: &Path,
        copy: &str,
    ) -> Result<Made, Reason> {
        let name = self.services[at].name.clone();
        let failed =
            |detail: String| reason!("server.copy_failed", service = name, detail = detail);
        let staged = process::sh(&self.peers, process::STAGE, &[stage])?;
        if process::exit_code(&staged) != 0 {
            return Err(failed(stderr_tail(&staged)));
        }
        let file = format!("{stage}/copy");
        let mut made_with = String::new();
        if let Some(dump) = &data.dump {
            if !self.running(at)? {
                return Err(reason!("server.copy_start_first", service = name));
            }
            let mut values = self.run_values(at);
            values.insert("_copy".into(), file.clone());
            let argv: Vec<String> = dump.iter().map(|word| fill(word, &values)).collect();
            let secrets = self.services[at].secrets(&self.own.secrets);
            let answer = process::execute_for(&self.peers, &argv, &secrets, RUN_SECONDS)?;
            if process::exit_code(&answer) != 0 {
                return Err(failed(stderr_tail(&answer)));
            }
            made_with = said(&answer, "made_with");
        } else {
            let paths = self.data_paths(at, data);
            let mut asked = vec![stage];
            asked.extend(paths.iter().map(String::as_str));
            // Stopped while it is packed, and started again whatever
            // happened: a copy that failed must not leave it off.
            let was = self.running(at)?;
            if was {
                self.halt(at)?;
            }
            let packed = process::sh_for(&self.peers, process::PACK, &asked, RUN_SECONDS);
            if was {
                self.start_service(at)?;
            }
            let packed = packed?;
            if process::exit_code(&packed) != 0 {
                return Err(failed(stderr_tail(&packed)));
            }
        }
        let sum = process::sh(&self.peers, process::SUM, &[&file])?;
        let bytes: u64 = said(&sum, "bytes").parse().unwrap_or(0);
        let sha = said(&sum, "sha");
        if sha.is_empty() {
            return Err(failed(stderr_tail(&sum)));
        }
        let part = folder.join(format!("{copy}.part"));
        let got = self.pull(&file, bytes, &part)?;
        if got != sha {
            return Err(failed("the copy changed on its way here".into()));
        }
        std::fs::rename(&part, folder.join(format!("{copy}.copy")))
            .map_err(|error| stored(&error))?;
        let instance = &self.services[at];
        Ok(Made {
            copy: copy.to_owned(),
            made: copy.parse().unwrap_or(0),
            note: String::new(),
            before: String::new(),
            definition: instance.definition.id.clone(),
            definition_version: instance.definition.version.clone(),
            machine: self.peers.name("host").unwrap_or_default(),
            kind: if data.dump.is_some() { "dump" } else { "files" }.to_owned(),
            made_with,
            bytes,
            sha256: sha,
        })
    }

    /// Bring `file` (`bytes` long) from the guest into `part`; its sha256.
    fn pull(&self, file: &str, bytes: u64, part: &Path) -> Result<String, Reason> {
        let mut out = private_file(part).map_err(|error| stored(&error))?;
        let mut hash = Sha256::new();
        let pieces = bytes.div_ceil(process::PIECE_BYTES);
        for piece in 0..pieces {
            let answer = process::sh_for(
                &self.peers,
                process::PIECE,
                &[file, &piece.to_string()],
                120,
            )?;
            let encoded = answer
                .get("stdout")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let raw = base64::decode(encoded.trim()).ok_or_else(|| {
                reason!(
                    "server.copy_not_saved",
                    detail = "a piece came back damaged"
                )
            })?;
            hash.update(&raw);
            out.write_all(&raw).map_err(|error| stored(&error))?;
        }
        out.sync_all().map_err(|error| stored(&error))?;
        Ok(format!("{:x}", hash.finalize()))
    }

    /// `server.copy_back`: put a copy back over what the service has now,
    /// after copying that first unless `first` is false.
    pub(super) fn copy_back(&mut self, action: &Action) -> Result<Data, Reason> {
        let at = self.find(&text(action, "service"))?;
        let name = self.services[at].name.clone();
        let copy = text(action, "copy");
        let folder = self.copies_of(&name);
        let made = is_copy_name(&copy)
            .then(|| Self::read_made(&folder, &copy))
            .flatten()
            .filter(|_| folder.join(format!("{copy}.copy")).is_file())
            .ok_or_else(|| reason!("server.no_such_copy", service = name))?;
        let data = self
            .own_data(at)
            .ok_or_else(|| reason!("server.no_data", service = name))?;
        let kind = if data.dump.is_some() { "dump" } else { "files" };
        if made.definition != self.services[at].definition.id || made.kind != kind {
            return Err(reason!("server.copy_other_kind", service = name));
        }
        self.copy_machine(at)?;
        if data.dump.is_some() && !self.running(at)? {
            return Err(reason!("server.copy_back_start_first", service = name));
        }
        let saved = if action.argument("first").and_then(Value::as_bool) == Some(false) {
            None
        } else {
            Some(self.make_copy(at, "", &copy)?)
        };
        let stage = self.stage_for(at, &format!("{copy}-back"));
        let result = self.put_back(at, &data, &made, &folder, &stage);
        let _ = process::sh(&self.peers, process::UNSTAGE, &[&stage]);
        result?;
        if self.running(at)? {
            self.check_health(at)?;
        }
        self.emit(
            "service.copied_back",
            data!({"service": name, "copy": copy}),
        );
        Ok(data!({"service": name, "copy": copy, "saved": saved}))
    }

    fn put_back(
        &mut self,
        at: usize,
        data: &OwnData,
        made: &Made,
        folder: &Path,
        stage: &str,
    ) -> Result<(), Reason> {
        let name = self.services[at].name.clone();
        let failed =
            |detail: String| reason!("server.copy_back_failed", service = name, detail = detail);
        let staged = process::sh(&self.peers, process::STAGE, &[stage])?;
        if process::exit_code(&staged) != 0 {
            return Err(failed(stderr_tail(&staged)));
        }
        let file = format!("{stage}/copy");
        let mut source = std::fs::File::open(folder.join(format!("{}.copy", made.copy)))
            .map_err(|error| stored(&error))?;
        let mut piece = vec![0u8; PUSH_BYTES];
        loop {
            let read = source.read(&mut piece).map_err(|error| stored(&error))?;
            if read == 0 {
                break;
            }
            process::write(
                &self.peers,
                &format!("{stage}/piece"),
                &base64::encode(&piece[..read]),
            )?;
            let appended = process::sh(&self.peers, process::APPEND, &[stage])?;
            if process::exit_code(&appended) != 0 {
                return Err(failed(stderr_tail(&appended)));
            }
        }
        let sum = process::sh(&self.peers, process::SUM, &[&file])?;
        if said(&sum, "sha") != made.sha256 {
            return Err(failed("the copy changed on its way there".into()));
        }
        if let Some(load) = &data.load {
            let mut values = self.run_values(at);
            values.insert("_copy".into(), file);
            values.insert("_made_with".into(), made.made_with.clone());
            let argv: Vec<String> = load.iter().map(|word| fill(word, &values)).collect();
            let secrets = self.services[at].secrets(&self.own.secrets);
            let answer = process::execute_for(&self.peers, &argv, &secrets, RUN_SECONDS)?;
            if process::exit_code(&answer) != 0 {
                let stdout = answer
                    .get("stdout")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(mut why) = stdout
                    .lines()
                    .find_map(|line| serde_json::from_str::<Reason>(line.trim()).ok())
                {
                    why.values.insert("service".into(), name);
                    return Err(why);
                }
                return Err(failed(stderr_tail(&answer)));
            }
            return Ok(());
        }
        let paths = self.data_paths(at, data);
        let mut asked = vec![stage];
        asked.extend(paths.iter().map(String::as_str));
        let was = self.running(at)?;
        if was {
            self.halt(at)?;
        }
        let unpacked = process::sh_for(&self.peers, process::UNPACK, &asked, RUN_SECONDS);
        if was {
            self.start_service(at)?;
        }
        let unpacked = unpacked?;
        if process::exit_code(&unpacked) != 0 {
            return Err(failed(stderr_tail(&unpacked)));
        }
        Ok(())
    }

    /// `server.copy_delete`: delete one copy, finished or not.
    pub(super) fn copy_delete(&mut self, action: &Action) -> Result<Data, Reason> {
        let at = self.find(&text(action, "service"))?;
        let name = self.services[at].name.clone();
        let copy = text(action, "copy");
        if !is_copy_name(&copy) {
            return Err(reason!("server.no_such_copy", service = name));
        }
        let folder = self.copies_of(&name);
        let mut found = false;
        for end in ["json", "copy", "part"] {
            match std::fs::remove_file(folder.join(format!("{copy}.{end}"))) {
                Ok(()) => found = true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(stored(&error)),
            }
        }
        if !found {
            return Err(reason!("server.no_such_copy", service = name));
        }
        Ok(data!({"service": name, "copy": copy}))
    }
}
