//! BeatDrop on Linux: capture a physical disk as a raw base image.
//!
//! An unmounted disk is copied byte for byte. The disk the running system is
//! on cannot be: its filesystems change while they are read, and plain
//! partitions offer no snapshot. So a disk in use is *rebuilt* on another
//! disk -- the partition table and every unmounted range copied as bytes,
//! each mounted filesystem made again at the same offset and filled file by
//! file, then given back its UUID and label so fstab and the bootloader find
//! it. Files that change during the copy are caught by a second pass, not
//! frozen: this is a copy of a live system, not a snapshot of one.

use std::cell::Cell;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use beatbox_contract::{BeatDropInspectView, BeatDropSourceView, BeatDropTargetView};
use beatbox_engine::reason;
use serde_json::{Map, Value};

use crate::disks::{SUFFIXES, a_name, bad_name};
use crate::fault::ApiFault;
use crate::guest_tools::run;
use crate::jobs::{Said, Say};
use crate::{Api, py};

/// Job kind for a physical disk capture.
pub const JOB: &str = "beatdrop";
/// A captured physical disk is a raw base image.
pub const SUFFIX: &str = ".raw";
const CHUNK: usize = 4 << 20;

/// The active capture's cancellation switch. Jobs are already exclusive.
#[derive(Default)]
pub struct Capture {
    cancel: std::sync::Mutex<Option<Arc<AtomicBool>>>,
}

impl Capture {
    fn begin(&self, switch: Arc<AtomicBool>) -> bool {
        let mut active = self.cancel.lock().unwrap();
        if active.is_some() {
            return false;
        }
        *active = Some(switch);
        true
    }

    fn end(&self) {
        *self.cancel.lock().unwrap() = None;
    }

    fn cancel(&self) -> bool {
        if let Some(switch) = self.cancel.lock().unwrap().as_ref() {
            switch.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
}

fn mounted(node: &Value, mounts: &mut Vec<String>) {
    if let Some(points) = node["mountpoints"].as_array() {
        for point in points.iter().filter_map(Value::as_str) {
            if !point.is_empty() {
                mounts.push(point.to_owned());
            }
        }
    }
    if let Some(children) = node["children"].as_array() {
        for child in children {
            mounted(child, mounts);
        }
    }
}

fn descendants(node: &Value, paths: &mut Vec<String>, stacked: &mut bool) {
    if let Some(children) = node["children"].as_array() {
        for child in children {
            if let Some(path) = child["path"].as_str() {
                paths.push(path.to_owned());
            }
            let kind = child["type"].as_str().unwrap_or_default();
            *stacked |= kind.starts_with("raid") || kind == "md";
            descendants(child, paths, stacked);
        }
    }
}

/// `lsblk` reports the actual block tree, including a partition mounted at
/// `/boot` while the parent disk itself has no mountpoint.
pub fn sources(lsblk: &str) -> Vec<BeatDropSourceView> {
    let parsed: Value = serde_json::from_str(lsblk).unwrap_or_default();
    let devices = parsed["blockdevices"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut uses: HashMap<String, usize> = HashMap::new();
    for disk in devices.iter().filter(|node| node["type"] == "disk") {
        let mut paths = Vec::new();
        let mut stacked = false;
        descendants(disk, &mut paths, &mut stacked);
        paths.sort();
        paths.dedup();
        for path in paths {
            *uses.entry(path).or_default() += 1;
        }
    }
    devices
        .iter()
        .filter(|node| node["type"] == "disk")
        .filter_map(|node| {
            let path = node["path"].as_str()?;
            let name = node["name"].as_str()?;
            // Only physical disks: not the virtual ones a capture itself
            // attaches (nbd), nor loop, RAM or compressed-swap devices, nor
            // an empty card reader.
            let virtual_disk = ["zram", "nbd", "loop", "ram"]
                .iter()
                .any(|prefix| name.starts_with(prefix));
            if virtual_disk || path.is_empty() || node["size"].as_i64().unwrap_or(0) <= 0 {
                return None;
            }
            let mut mountpoints = Vec::new();
            mounted(node, &mut mountpoints);
            let mut paths = Vec::new();
            let mut stacked = false;
            descendants(node, &mut paths, &mut stacked);
            let dependent = stacked
                || paths
                    .iter()
                    .any(|path| uses.get(path).copied().unwrap_or(0) > 1);
            mountpoints.sort();
            mountpoints.dedup();
            let bytes = node["size"].as_i64().unwrap_or(0);
            // A disk in use is copied live, or says why it cannot be.
            let (needed, problem) = match parts(lsblk, path) {
                _ if mountpoints.is_empty() => (bytes, String::new()),
                Ok(parts) => (
                    i64::try_from(needed(&parts, bytes as u64)).unwrap_or(i64::MAX),
                    String::new(),
                ),
                Err(problem) => (bytes, problem),
            };
            Some(BeatDropSourceView {
                path: path.to_owned(),
                name: name.to_owned(),
                bytes,
                mountpoints,
                dependent,
                needed,
                problem,
            })
        })
        .collect()
}

fn number(value: &Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        .unwrap_or(0)
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

/// Mounted filesystems a live capture can rebuild. Anything else in use is
/// refused by name rather than copied as bytes that are changing.
const REBUILT: [&str; 5] = ["ext2", "ext3", "ext4", "vfat", "swap"];
/// Filesystems a live copy can be kept on. It is a qcow2, which grows only
/// with what is written, so exFAT and NTFS USB disks work; FAT cannot hold a
/// file over 4 GB.
const WRITABLE: [&str; 9] = [
    "ext2", "ext3", "ext4", "xfs", "btrfs", "f2fs", "exfat", "ntfs", "ntfs3",
];
/// A live copy is a qcow2 image.
pub const LIVE_SUFFIX: &str = crate::disks::SUFFIX;

/// One partition of a disk in use, as the live capture sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// Its device, `/dev/nvme0n1p2`.
    pub path: String,
    /// Where it starts on the disk, in bytes.
    pub start: u64,
    /// How long it is, in bytes.
    pub bytes: u64,
    /// Its filesystem, as `lsblk` names it.
    pub fstype: String,
    /// The filesystem's version: `FAT32` is what decides the FAT layout.
    pub fsver: String,
    /// What fstab and the bootloader find it by.
    pub uuid: String,
    /// Its label, often empty.
    pub label: String,
    /// Bytes in use, when mounted.
    pub used: u64,
    /// Whether it is in use and must be rebuilt rather than copied.
    pub mounted: bool,
}

/// The partitions of `disk`, or why a live capture cannot rebuild them.
pub fn parts(lsblk: &str, disk: &str) -> Result<Vec<Part>, String> {
    let parsed: Value = serde_json::from_str(lsblk).unwrap_or_default();
    let node = parsed["blockdevices"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|node| node["path"] == disk && node["type"] == "disk")
        .ok_or_else(|| format!("{disk} is not a physical disk"))?;
    let mut found = Vec::new();
    for child in node["children"].as_array().into_iter().flatten() {
        let path = text(&child["path"]);
        if child["type"] != "part"
            || child["children"]
                .as_array()
                .is_some_and(|children| !children.is_empty())
        {
            return Err(format!(
                "{path} holds encryption, LVM or RAID, which a live capture cannot rebuild yet"
            ));
        }
        let mut mounts = Vec::new();
        mounted(child, &mut mounts);
        let fstype = text(&child["fstype"]);
        let uuid = text(&child["uuid"]);
        let mounted = !mounts.is_empty();
        if mounted && !REBUILT.contains(&fstype.as_str()) {
            return Err(format!(
                "{path} is {fstype}, which a live capture cannot rebuild yet"
            ));
        }
        if mounted && uuid.is_empty() {
            return Err(format!("{path} has no filesystem UUID"));
        }
        found.push(Part {
            path,
            start: number(&child["start"]) * 512,
            bytes: number(&child["size"]),
            fstype,
            fsver: text(&child["fsver"]),
            uuid,
            label: text(&child["label"]),
            used: number(&child["fsused"]),
            mounted,
        });
    }
    Ok(found)
}

/// Everything on the disk that is not a mounted partition: the partition
/// tables at both ends, gaps, and partitions nobody is writing to.
pub fn raw_ranges(parts: &[Part], bytes: u64) -> Vec<(u64, u64)> {
    let mut rebuilt: Vec<(u64, u64)> = parts
        .iter()
        .filter(|part| part.mounted)
        .map(|part| (part.start, part.start + part.bytes))
        .collect();
    rebuilt.sort_unstable();
    let mut ranges = Vec::new();
    let mut at = 0;
    for (start, end) in rebuilt {
        if start > at {
            ranges.push((at, start));
        }
        at = at.max(end);
    }
    if bytes > at {
        ranges.push((at, bytes));
    }
    ranges
}

/// Room a live capture takes on its target, with a little for metadata.
pub fn needed(parts: &[Part], bytes: u64) -> u64 {
    let raw: u64 = raw_ranges(parts, bytes)
        .iter()
        .map(|(start, end)| end - start)
        .sum();
    let files: u64 = parts
        .iter()
        .filter(|part| part.mounted)
        .map(|part| part.used)
        .sum();
    (raw + files) / 100 * 101
}

fn nodes<'a>(node: &'a Value, found: &mut Vec<&'a Value>) {
    found.push(node);
    for child in node["children"].as_array().into_iter().flatten() {
        nodes(child, found);
    }
}

/// Mounted filesystems a live capture can write its image to, with the
/// physical disk each is on so the page can leave out the source's own.
pub fn targets(lsblk: &str) -> Vec<BeatDropTargetView> {
    let parsed: Value = serde_json::from_str(lsblk).unwrap_or_default();
    let mut found = Vec::new();
    for disk in parsed["blockdevices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|node| node["type"] == "disk")
    {
        let mut below = Vec::new();
        nodes(disk, &mut below);
        for node in below {
            if !WRITABLE.contains(&node["fstype"].as_str().unwrap_or_default()) {
                continue;
            }
            let Some(path) = node["mountpoints"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|point| !point.is_empty())
                .min_by_key(|point| point.len())
            else {
                continue;
            };
            found.push(BeatDropTargetView {
                path: path.to_owned(),
                disk: text(&disk["path"]),
                free: i64::try_from(number(&node["fsavail"])).unwrap_or(i64::MAX),
            });
        }
    }
    found
}

/// Where `path` lives: the partition of `parts` holding it, and the path from
/// that filesystem's own root, which is how the rebuild's rsync sees it. The
/// longest mountpoint wins, so a path on a USB disk under `/run/media` is not
/// mistaken for part of `/`; bind mounts carry their `fsroot`.
pub fn locate(findmnt: &str, parts: &[Part], path: &str) -> Option<(String, String)> {
    let parsed: Value = serde_json::from_str(findmnt).unwrap_or_default();
    let (target, source, fsroot) = parsed["filesystems"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|mount| {
            let target = mount["target"].as_str()?;
            let rest = path.strip_prefix(target)?;
            (target == "/" || rest.is_empty() || rest.starts_with('/')).then_some((
                target,
                mount["source"].as_str()?,
                mount["fsroot"].as_str().unwrap_or("/"),
            ))
        })
        .max_by_key(|(target, _, _)| target.len())?;
    let device = source.split('[').next()?;
    parts
        .iter()
        .find(|part| part.mounted && part.path == device)?;
    let rest = path[target.len()..].trim_matches('/');
    let root = fsroot.trim_end_matches('/');
    let inside = if rest.is_empty() {
        format!("{root}/")
    } else {
        format!("{root}/{rest}")
    };
    Some((device.to_owned(), inside))
}

/// An rsync exclude anchored at the filesystem's root. A folder stays, empty,
/// so a mountpoint or a home directory is still there in the copy.
pub fn leave_out_rule(inside: &str, folder: bool) -> String {
    let wild = folder || inside.contains(['*', '?', '[']);
    let mut rule: String = if wild {
        inside
            .chars()
            .flat_map(|c| {
                matches!(c, '*' | '?' | '[' | '\\')
                    .then_some('\\')
                    .into_iter()
                    .chain([c])
            })
            .collect()
    } else {
        inside.to_owned()
    };
    if folder {
        rule = format!("{}/**", rule.trim_end_matches('/'));
    }
    rule
}

/// `du -d1` read as the folders inside `folder`, largest first.
pub fn folders(du: &str, folder: &str) -> Vec<beatbox_contract::DiskView> {
    let mut found: Vec<beatbox_contract::DiskView> = du
        .lines()
        .filter_map(|line| {
            let (bytes, path) = line.split_once('\t')?;
            (path.trim_end_matches('/') != folder.trim_end_matches('/')).then(|| {
                beatbox_contract::DiskView {
                    name: Path::new(path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    path: path.to_owned(),
                    bytes: bytes.parse().unwrap_or(0),
                }
            })
        })
        .collect();
    found.sort_by_key(|entry| std::cmp::Reverse(entry.bytes));
    found
}

/// What rsync said went wrong, without its summary line or the files that
/// merely vanished: the last few, for a message that fits on one line.
pub fn rsync_errors(stderr: impl BufRead) -> Vec<String> {
    let mut found: Vec<String> = stderr
        .lines()
        .map_while(Result::ok)
        .map(|line| line.trim().to_owned())
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("file has vanished")
                && !line.starts_with("rsync error: some files")
                && !line.starts_with("rsync error: some files/attrs")
        })
        .collect();
    let keep = found.len().saturating_sub(3);
    found.drain(..keep);
    found
}

/// Sectors per cluster for a rebuilt FAT: the original's, but never so
/// large that a FAT32 falls under the 65525 clusters the specification (and
/// UEFI's FAT driver) require. Some PCs boot an undersized one; OVMF does not.
pub fn fat_cluster(original: u8, bytes: u64, fsver: &str) -> u8 {
    let original = original.max(1);
    if fsver != "FAT32" {
        return original;
    }
    // A margin over 65525 for the FATs and reserved sectors.
    let most = (bytes / 512 / 70_000).max(1);
    let most = 1u64 << (63 - most.leading_zeros());
    original.min(u8::try_from(most.min(128)).unwrap_or(128))
}

/// rsync's `--info=progress2` line, `  1,234  37%  1.00MB/s  0:00:01`.
pub fn rsync_percent(line: &str) -> Option<u64> {
    line.split_whitespace()
        .rev()
        .find_map(|word| word.strip_suffix('%')?.parse().ok())
}

fn scan() -> Result<String, ApiFault> {
    if !cfg!(target_os = "linux") {
        return Err(ApiFault::new(
            "beatdrop-platform",
            "BeatDrop currently captures disks on Linux hosts.",
        ));
    }
    let output = Command::new("lsblk")
        .args([
            "-J",
            "-b",
            "-o",
            "NAME,PATH,TYPE,SIZE,START,FSTYPE,FSVER,UUID,LABEL,FSUSED,FSAVAIL,MOUNTPOINTS",
        ])
        .output()
        .map_err(|error| ApiFault::new("beatdrop-inspect", error.to_string()))?;
    if !output.status.success() {
        return Err(ApiFault::new(
            "beatdrop-inspect",
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn mounts() -> Result<String, ApiFault> {
    let output = Command::new("findmnt")
        .args(["-J", "-l", "-o", "TARGET,SOURCE,FSROOT"])
        .output()
        .map_err(|error| ApiFault::new("beatdrop-inspect", error.to_string()))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Bytes a path holds on its own filesystem, as far as this user can read.
fn size_of(path: &Path) -> u64 {
    Command::new("du")
        .args(["-s", "-x", "-B1"])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|output| {
            String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

fn inventory() -> Result<Vec<BeatDropSourceView>, ApiFault> {
    Ok(sources(&scan()?))
}

fn selected(source: &str) -> Result<BeatDropSourceView, ApiFault> {
    let found = inventory()?
        .into_iter()
        .find(|candidate| candidate.path == source)
        .ok_or_else(|| ApiFault::new("beatdrop-source", "Choose a physical disk from the list."))?;
    if !found.mountpoints.is_empty() {
        return Err(ApiFault::new(
            "beatdrop-mounted",
            "This disk has mounted volumes. Boot another system to capture it.",
        ));
    }
    if found.dependent {
        return Err(ApiFault::new(
            "beatdrop-dependent",
            "This disk belongs to a storage set that needs other disks.",
        ));
    }
    if found.bytes <= 0 {
        return Err(ApiFault::new(
            "beatdrop-size",
            "The disk size could not be read.",
        ));
    }
    Ok(found)
}

/// Copy bytes into a sparse raw image from where `target` is positioned. A
/// zero chunk occupies no blocks on the destination filesystem; logical
/// bytes remain exactly those read once the caller sets the file's length.
pub fn copy_sparse(
    reader: &mut dyn Read,
    target: &mut File,
    bytes: u64,
    cancel: &AtomicBool,
    progress: &dyn Fn(u64),
) -> io::Result<()> {
    let mut chunk = vec![0u8; CHUNK];
    let mut done = 0u64;
    while done < bytes {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "capture cancelled",
            ));
        }
        let wanted = usize::try_from((bytes - done).min(CHUNK as u64)).unwrap_or(CHUNK);
        reader.read_exact(&mut chunk[..wanted])?;
        if chunk[..wanted].iter().all(|byte| *byte == 0) {
            target.seek(SeekFrom::Current(wanted as i64))?;
        } else {
            target.write_all(&chunk[..wanted])?;
        }
        done += wanted as u64;
        progress(done);
    }
    Ok(())
}

/// Says a whole percentage when it moves, never 100 before the end.
struct Meter<'a> {
    total: u64,
    base: Cell<u64>,
    said: Cell<u64>,
    say: Say<'a>,
}

impl<'a> Meter<'a> {
    fn new(total: u64, say: Say<'a>) -> Self {
        Self {
            total: total.max(1),
            base: Cell::new(0),
            said: Cell::new(0),
            say,
        }
    }

    fn at(&self, extra: u64) {
        let percent = ((self.base.get() + extra).saturating_mul(100) / self.total).min(99);
        if percent > self.said.get() {
            self.said.set(percent);
            (self.say)(&format!("{percent}%"));
        }
    }

    #[cfg(unix)]
    fn add(&self, bytes: u64) {
        self.base.set(self.base.get() + bytes);
        self.at(0);
    }
}

enum Source {
    Direct(File),
    /// The pipe is an `Option` so it can be closed before the reader is
    /// waited for: see `Drop`.
    Elevated(Option<ChildStdout>, Child),
}

impl Read for Source {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Direct(file) => file.read(bytes),
            Self::Elevated(stdout, child) => {
                let read = stdout.as_mut().map_or(Ok(0), |stdout| stdout.read(bytes))?;
                if read > 0 {
                    return Ok(read);
                }
                let status = child.wait()?;
                if status.success() {
                    Ok(0)
                } else {
                    let mut detail = String::new();
                    if let Some(mut stderr) = child.stderr.take() {
                        let _ = stderr.read_to_string(&mut detail);
                    }
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("disk access was not authorized: {}", detail.trim()),
                    ))
                }
            }
        }
    }
}

impl Source {
    fn open(path: &str) -> io::Result<Self> {
        match File::open(path) {
            Ok(file) => {
                check_block(&file)?;
                Ok(Self::Direct(file))
            }
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                let executable = std::env::current_exe()?;
                let mut child = Command::new("pkexec")
                    .arg(executable)
                    .arg("--beatdrop-read")
                    .arg(path)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()?;
                let stdout = child.stdout.take().expect("piped");
                Ok(Self::Elevated(Some(stdout), child))
            }
            Err(error) => Err(error),
        }
    }

    fn finish(&mut self) -> io::Result<()> {
        let Self::Elevated(_, child) = self else {
            return Ok(());
        };
        let status = child.wait()?;
        if status.success() {
            return Ok(());
        }
        let mut detail = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut detail);
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("disk access was not authorized: {}", detail.trim()),
        ))
    }
}

#[cfg(unix)]
fn check_block(file: &File) -> io::Result<()> {
    use std::os::unix::fs::FileTypeExt;
    if file.metadata()?.file_type().is_block_device() {
        Ok(())
    } else {
        Err(io::Error::other("the source is no longer a block device"))
    }
}

#[cfg(not(unix))]
fn check_block(_file: &File) -> io::Result<()> {
    Err(io::Error::other("BeatDrop's disk reader requires Linux"))
}

/// The daemon's short, elevated mode. It is a read-only source stream and
/// never accepts an output path, so it cannot overwrite the host's files.
pub fn read_device(path: &str) -> io::Result<()> {
    let source_view = selected(path).map_err(|error| io::Error::other(error.message))?;
    let source = File::open(path)?;
    check_block(&source)?;
    let mut source = source.take(source_view.bytes as u64);
    let mut output = io::stdout().lock();
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
    }
    Ok(())
}

/// Once the daemon that made a partial is gone, it cannot be resumed. Only
/// our own suffix is swept, and only by the daemon holding the owner's lock.
pub fn sweep_partials(folder: &Path) {
    if let Ok(entries) = fs::read_dir(folder) {
        for path in entries.filter_map(Result::ok).map(|entry| entry.path()) {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".raw.beatdrop-partial"))
                && path.is_file()
            {
                let _ = fs::remove_file(path);
            }
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        if let Self::Elevated(stdout, child) = self {
            // The reader runs as root, so `kill` is refused. Closing its pipe
            // stops it: its next write fails. Wait with the pipe open and
            // both block on each other, and Cancel never returns.
            drop(stdout.take());
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn capture(
    source: BeatDropSourceView,
    target: PathBuf,
    switch: &AtomicBool,
    say: Say<'_>,
) -> io::Result<()> {
    fs::create_dir_all(target.parent().unwrap_or(Path::new(".")))?;
    let partial = target.with_extension("raw.beatdrop-partial");
    let result = (|| {
        // A stale partial after a crash must be dealt with explicitly; it is
        // never published, and a new attempt never truncates it.
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut out = options.open(&partial)?;
        let mut input = Source::open(&source.path)?;
        let mut sector = [0u8; 512];
        input.read_exact(&mut sector)?;
        if sector[510..] != [0x55, 0xaa] {
            return Err(io::Error::other("the source disk has no boot signature"));
        }
        let mut complete = io::Cursor::new(sector).chain(&mut input);
        let meter = Meter::new(source.bytes as u64, say);
        copy_sparse(
            &mut complete,
            &mut out,
            source.bytes as u64,
            switch,
            &|done| meter.at(done),
        )?;
        out.set_len(source.bytes as u64)?;
        out.sync_all()?;
        input.finish()?;
        if switch.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "capture cancelled",
            ));
        }
        let mut sector = [0u8; 512];
        out.seek(SeekFrom::Start(0))?;
        out.read_exact(&mut sector)?;
        if sector[510..] != [0x55, 0xaa] {
            return Err(io::Error::other("the captured disk has no boot signature"));
        }
        let fresh = selected(&source.path).map_err(|error| io::Error::other(error.message))?;
        if fresh.bytes != source.bytes {
            return Err(io::Error::other("the source disk changed during capture"));
        }
        fs::hard_link(&partial, &target)?;
        let _ = fs::remove_file(&partial);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result
}

/// The disk in use, rebuilt. `pkexec` starts `--beatdrop-live`, which moves
/// into a private mount namespace so every mount it makes goes with it
/// however it ends; the inner `--beatdrop-live-ns` does the work as root.
///
/// It takes a disk and a target *from the inventory*, never a path to write:
/// the image is a new file under `BeatDrop/` on a filesystem of another disk.
/// Its only output is progress on stdout; closing its stdin cancels it,
/// because the daemon cannot signal a process running as root.
#[cfg(unix)]
pub mod live {
    use std::collections::HashMap;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
    use std::os::unix::fs::{FileExt, OpenOptionsExt};
    use std::os::unix::io::AsRawFd;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitStatus, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::thread;
    use std::time::Duration;

    use super::{
        LIVE_SUFFIX, Meter, Part, a_name, check_block, copy_sparse, fat_cluster, leave_out_rule,
        locate, mounts, parts, raw_ranges, rsync_errors, rsync_percent, scan, sources, targets,
    };

    fn fail(message: impl Into<String>) -> io::Error {
        io::Error::other(message.into())
    }

    fn cancelled() -> io::Error {
        io::Error::new(io::ErrorKind::Interrupted, "capture cancelled")
    }

    fn tool(program: &str, args: &[&str]) -> io::Result<String> {
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| fail(format!("{program}: {error}")))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(fail(format!(
                "{program} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }

    /// `--beatdrop-live`: the same arguments, inside a mount namespace of
    /// its own.
    ///
    /// The machine must not sleep meanwhile: suspending cuts the nbd socket
    /// and the copy's filesystem fails under rsync. `systemd-inhibit` holds
    /// sleep and the lid switch off for as long as this runs, where there is
    /// one.
    pub fn isolated(args: &[String]) -> io::Result<ExitStatus> {
        let isolate = |program: &str, first: &[&str]| -> io::Result<ExitStatus> {
            Command::new(program)
                .args(first)
                .args(["unshare", "--mount", "--propagation", "private"])
                .arg(std::env::current_exe()?)
                .arg("--beatdrop-live-ns")
                .args(args)
                .status()
        };
        match isolate(
            "systemd-inhibit",
            &[
                "--what=sleep:idle:handle-lid-switch",
                "--who=BeatBox",
                "--why=Cloning this computer's disk",
                "--mode=block",
            ],
        ) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => isolate("env", &[]),
            other => other,
        }
    }

    /// Unmounted when dropped: by then nothing else holds it.
    struct Mount(PathBuf);

    impl Mount {
        fn new(what: &str, at: &Path, options: &[&str]) -> io::Result<Self> {
            let at_text = at.to_string_lossy();
            let mut args = options.to_vec();
            args.extend([what, &at_text]);
            tool("mount", &args)?;
            Ok(Self(at.to_owned()))
        }
    }

    impl Drop for Mount {
        fn drop(&mut self) {
            let _ = Command::new("umount").arg(&self.0).status();
        }
    }

    /// The filesystem made again, empty, at the partition's offset in the
    /// image. ext and FAT get a fresh random identity here: two block
    /// devices with one UUID on a running system can make systemd unmount
    /// the real one when the copy goes away. [`identity`] gives it back once
    /// the image is no longer attached.
    fn make(part: &Part, image: &str) -> io::Result<()> {
        let kib = (part.bytes / 1024).to_string();
        match part.fstype.as_str() {
            "vfat" => {
                let bits = part.fsver.trim_start_matches("FAT");
                let sector = (part.start / 512).to_string();
                // The original's cluster size, not mkfs.fat's pick: on an nbd
                // device it chose 32 KiB, which leaves a 1 GiB ESP under the
                // 65525 clusters FAT32 needs, and UEFI's FAT driver then
                // reads it as FAT16 and finds nothing to boot.
                let mut boot = [0u8; 512];
                File::open(&part.path)?.read_exact(&mut boot)?;
                let cluster = fat_cluster(boot[13], part.bytes, &part.fsver).to_string();
                let mut args = vec!["-F", bits, "-s", &cluster, "--offset", &sector];
                if !part.label.is_empty() {
                    args.extend(["-n", &part.label]);
                }
                args.extend([image, &kib]);
                tool("mkfs.fat", &args).map(drop)
            }
            // Never attached, so it may carry its identity from the start.
            "swap" => {
                let offset = part.start.to_string();
                let mut args = vec!["-U", &part.uuid, "--offset", &offset];
                if !part.label.is_empty() {
                    args.extend(["-L", &part.label]);
                }
                args.extend([image, &kib]);
                tool("mkswap", &args).map(drop)
            }
            ext => {
                let shape = tool("dumpe2fs", &["-h", &part.path])?;
                let field = |name: &str| {
                    shape
                        .lines()
                        .find_map(|line| line.strip_prefix(name))
                        .map(|value| value.trim().to_owned())
                        .ok_or_else(|| fail(format!("dumpe2fs did not say {name}")))
                };
                let (inodes, block, inode) = (
                    field("Inode count:")?,
                    field("Block size:")?,
                    field("Inode size:")?,
                );
                let offset = format!("offset={}", part.start);
                let size = format!("{kib}k");
                tool(
                    "mke2fs",
                    &[
                        "-t", ext, "-F", "-q", "-b", &block, "-I", &inode, "-N", &inodes, "-E",
                        &offset, image, &size,
                    ],
                )
                .map(drop)
            }
        }
    }

    /// The source's UUID and label, written into the unattached image.
    fn identity(part: &Part, image: &str, out: &File) -> io::Result<()> {
        match part.fstype.as_str() {
            "swap" => Ok(()),
            "vfat" => {
                let serial = u32::from_str_radix(&part.uuid.replace('-', ""), 16)
                    .map_err(|_| fail(format!("{} has an odd FAT serial", part.path)))?;
                if part.fsver == "FAT32" {
                    // The boot sector and its backup, which FAT32 keeps too.
                    let mut backup = [0u8; 2];
                    out.read_exact_at(&mut backup, part.start + 0x32)?;
                    let backup = u64::from(u16::from_le_bytes(backup));
                    out.write_all_at(&serial.to_le_bytes(), part.start + 0x43)?;
                    if backup != 0 {
                        out.write_all_at(&serial.to_le_bytes(), part.start + backup * 512 + 0x43)?;
                    }
                } else {
                    out.write_all_at(&serial.to_le_bytes(), part.start + 0x27)?;
                }
                Ok(())
            }
            _ => {
                // e2fsprogs read `file?offset=N` as the filesystem at N. The
                // check is also the copy's validation; tune2fs needs it
                // freshly done before it changes a checksummed UUID.
                let spec = format!("{image}?offset={}", part.start);
                let status = Command::new("e2fsck")
                    .args(["-f", "-y", &spec])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .status()?;
                if status.code().is_none_or(|code| code > 1) {
                    return Err(fail(format!(
                        "the copy of {} did not pass e2fsck ({status})",
                        part.path
                    )));
                }
                let mut args = vec!["-U", &part.uuid];
                if !part.label.is_empty() {
                    args.extend(["-L", &part.label]);
                }
                args.push(&spec);
                tool("tune2fs", &args).map(drop)
            }
        }
    }

    /// Two passes: the second, with `--delete`, catches what changed while
    /// the first ran.
    fn fill(
        part: &Part,
        from: &Path,
        into: &Path,
        skip: &[String],
        cancel: &AtomicBool,
        meter: &Meter<'_>,
    ) -> io::Result<()> {
        let keep: &[&str] = if part.fstype == "vfat" {
            &["-rltS", "--modify-window=1"]
        } else {
            &["-aHAXS", "--numeric-ids"]
        };
        for pass in 0..2 {
            let mut child = Command::new("rsync")
                .args(keep)
                .args([
                    "--one-file-system",
                    "--info=progress2",
                    "--no-inc-recursive",
                ])
                .args(if pass == 1 { &["--delete"][..] } else { &[] })
                .args(skip.iter().map(|rule| format!("--exclude={rule}")))
                .arg(format!("{}/", from.display()))
                .arg(format!("{}/", into.display()))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| fail(format!("rsync: {error}")))?;
            let stderr = child.stderr.take().expect("piped");
            let complaints = thread::spawn(move || rsync_errors(BufReader::new(stderr)));
            let percent = Arc::new(AtomicU64::new(0));
            let stdout = child.stdout.take().expect("piped");
            let heard = Arc::clone(&percent);
            let reader = thread::spawn(move || {
                for chunk in BufReader::new(stdout).split(b'\r').map_while(Result::ok) {
                    if let Some(value) = rsync_percent(&String::from_utf8_lossy(&chunk)) {
                        heard.store(value, Ordering::Relaxed);
                    }
                }
            });
            let status = loop {
                if cancel.load(Ordering::Relaxed) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(cancelled());
                }
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if pass == 0 {
                    meter.at(part.used / 100 * percent.load(Ordering::Relaxed));
                }
                thread::sleep(Duration::from_millis(250));
            };
            let _ = reader.join();
            let complaints = complaints.join().unwrap_or_default();
            // 24 is files that vanished meanwhile. 23 in the first pass is,
            // on a running system, mostly files that changed while read; the
            // second pass copies them again, and it is the one that must be
            // clean.
            let fine = match status.code() {
                Some(0 | 24) => true,
                Some(23) => pass == 0,
                _ => false,
            };
            if !fine {
                return Err(fail(format!(
                    "rsync stopped copying {} ({status}): {}",
                    part.path,
                    if complaints.is_empty() {
                        "no detail".to_owned()
                    } else {
                        complaints.join(" | ")
                    }
                )));
            }
        }
        Ok(())
    }

    /// The qcow2, attached as a block device. The module is loaded with
    /// `max_part=0` so the kernel never reads the copy's partition table and
    /// its UUIDs never appear beside the real ones (ignored if it is already
    /// loaded; see `capture` for the second guard).
    struct Nbd(String);

    impl Nbd {
        fn connect(image: &str) -> io::Result<Self> {
            tool("modprobe", &["nbd", "max_part=0"])?;
            let size = |index: usize| {
                fs::read_to_string(format!("/sys/block/nbd{index}/size"))
                    .ok()
                    .and_then(|size| size.trim().parse::<u64>().ok())
            };
            for index in 0..16 {
                if size(index) != Some(0) {
                    continue;
                }
                let device = format!("/dev/nbd{index}");
                let connected = tool(
                    "qemu-nbd",
                    &[
                        "--connect",
                        &device,
                        "--format",
                        "qcow2",
                        "--discard",
                        "unmap",
                        image,
                    ],
                );
                if connected.is_err() {
                    continue;
                }
                for _ in 0..100 {
                    if size(index).is_some_and(|size| size > 0) {
                        return Ok(Self(device));
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                let _ = tool("qemu-nbd", &["--disconnect", &device]);
            }
            Err(fail("no free nbd device to attach the copy to"))
        }
    }

    impl Drop for Nbd {
        fn drop(&mut self) {
            let _ = tool("qemu-nbd", &["--disconnect", &self.0]);
        }
    }

    /// Every filesystem in use rebuilt first, then the bytes around them --
    /// the partition tables last, so nothing reads one while the copy's
    /// UUIDs are being written.
    #[allow(clippy::too_many_arguments)]
    fn rebuild(
        disk: &str,
        bytes: u64,
        parts: &[Part],
        skip: &HashMap<String, Vec<String>>,
        image: &str,
        out: &mut File,
        cancel: &AtomicBool,
        meter: &Meter<'_>,
    ) -> io::Result<()> {
        let room = PathBuf::from(format!("/run/beatdrop-{}", std::process::id()));
        let (from, into) = (room.join("source"), room.join("copy"));
        fs::create_dir_all(&from)?;
        fs::create_dir_all(&into)?;
        let result = (|| {
            for part in parts.iter().filter(|part| part.mounted) {
                if cancel.load(Ordering::Relaxed) {
                    return Err(cancelled());
                }
                make(part, image)?;
                if part.fstype == "swap" {
                    continue;
                }
                {
                    // The device itself, not its mountpoint: its whole tree,
                    // without what other filesystems cover or bind over.
                    let source = Mount::new(&part.path, &from, &[])?;
                    tool(
                        "mount",
                        &["-o", "remount,bind,ro", &source.0.to_string_lossy()],
                    )?;
                    let loop_options =
                        format!("loop,offset={},sizelimit={}", part.start, part.bytes);
                    let _copy = Mount::new(image, &into, &["-o", &loop_options])?;
                    let rules = skip.get(&part.path).map_or(&[][..], Vec::as_slice);
                    fill(part, &from, &into, rules, cancel, meter)?;
                }
                meter.add(part.used);
                identity(part, image, out)?;
            }
            Ok(())
        })();
        let _ = fs::remove_dir(&from);
        let _ = fs::remove_dir(&into);
        let _ = fs::remove_dir(&room);
        result?;
        let mut device = File::open(disk)?;
        check_block(&device)?;
        for (start, end) in raw_ranges(parts, bytes) {
            device.seek(SeekFrom::Start(start))?;
            out.seek(SeekFrom::Start(start))?;
            copy_sparse(
                &mut (&mut device).take(end - start),
                out,
                end - start,
                cancel,
                &|done| meter.at(done),
            )?;
            meter.add(end - start);
        }
        out.sync_all()?;
        let mut sector = [0u8; 512];
        out.read_exact_at(&mut sector, 0)?;
        if sector[510..] != [0x55, 0xaa] {
            return Err(fail("the captured disk has no boot signature"));
        }
        Ok(())
    }

    /// `--beatdrop-live-ns DISK TARGET NAME [LEAVE-OUT...]`, as root.
    pub fn capture(disk: &str, target: &str, name: &str, leave_out: &[String]) -> io::Result<()> {
        let owner: u32 = std::env::var("PKEXEC_UID")
            .ok()
            .and_then(|uid| uid.parse().ok())
            .ok_or_else(|| fail("BeatBox starts this through pkexec"))?;
        if !a_name(name) {
            return Err(fail("not a disk name"));
        }
        let lsblk = scan().map_err(|error| fail(error.message))?;
        let source = sources(&lsblk)
            .into_iter()
            .find(|candidate| candidate.path == disk)
            .ok_or_else(|| fail("choose a physical disk from the list"))?;
        if source.dependent {
            return Err(fail(
                "this disk belongs to a storage set that needs other disks",
            ));
        }
        let parts = parts(&lsblk, disk).map_err(fail)?;
        if !targets(&lsblk).iter().any(|spot| spot.path == target) {
            return Err(fail("the target must be a mounted Linux filesystem"));
        }
        // What the person left out, and the copy itself when it is being
        // written onto the disk it copies.
        let table = mounts().map_err(|error| fail(error.message))?;
        let mut skip: HashMap<String, Vec<String>> = HashMap::new();
        let own = Path::new(target).join("BeatDrop");
        for path in leave_out.iter().map(PathBuf::from).chain([own.clone()]) {
            let text = path.to_string_lossy();
            if path != own && (text == "/" || path.canonicalize().ok().as_ref() != Some(&path)) {
                return Err(fail(format!("{text} cannot be left out")));
            }
            let Some((device, inside)) = locate(&table, &parts, &text) else {
                if path == own {
                    continue;
                }
                return Err(fail(format!("{text} is not on {disk}")));
            };
            let folder = path == own || fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir());
            skip.entry(device)
                .or_default()
                .push(leave_out_rule(&inside, folder));
        }
        // exFAT and NTFS have no owners, so the folder cannot be required to
        // be root's. It must not be a symlink, and the image is created new,
        // so root never writes into a file that was already there.
        let folder = Path::new(target).join("BeatDrop");
        match fs::symlink_metadata(&folder) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&folder)?,
            Ok(meta) if meta.is_dir() => {}
            _ => {
                return Err(fail(format!(
                    "{} is not a folder BeatDrop can use",
                    folder.display()
                )));
            }
        }
        let image = folder.join(format!("{name}{LIVE_SUFFIX}"));
        let partial = folder.join(format!("{name}{LIVE_SUFFIX}.beatdrop-partial"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)?;
        // Tools reach the image through this process's descriptor, so a path
        // swapped underneath cannot point them at another file.
        let held = format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd());
        let cancel = Arc::new(AtomicBool::new(false));
        let heard = Arc::clone(&cancel);
        thread::spawn(move || {
            let _ = io::copy(&mut io::stdin(), &mut io::sink());
            heard.store(true, Ordering::Relaxed);
        });
        let total = super::needed(&parts, source.bytes as u64);
        let say = |line: &str| {
            let mut stdout = io::stdout().lock();
            let _ = writeln!(stdout, "{line}");
        };
        let meter = Meter::new(total, &say);
        let result = (|| {
            let bytes = source.bytes as u64;
            tool(
                "qemu-img",
                &["create", "-q", "-f", "qcow2", &held, &bytes.to_string()],
            )?;
            // Declared before the device so it is dropped after it: the
            // device is disconnected while still open, and closing it later
            // -- which is when udev re-reads a partition table -- finds
            // nothing there.
            let mut out: Option<File> = None;
            let nbd = Nbd::connect(&held)?;
            let device = out.insert(OpenOptions::new().read(true).write(true).open(&nbd.0)?);
            rebuild(disk, bytes, &parts, &skip, &nbd.0, device, &cancel, &meter)?;
            drop(nbd);
            drop(out);
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&partial);
            return Err(error);
        }
        // The copy is complete. Nothing from here on may throw it away.
        // Rename, not hard link: exFAT and NTFS through FUSE have no links.
        // The name was checked free when this started; it is checked again
        // because rename would replace a file that appeared meanwhile.
        if image.exists() {
            return Err(fail(format!(
                "the copy is complete but {} appeared meanwhile; it is kept as {}",
                image.display(),
                partial.display()
            )));
        }
        fs::rename(&partial, &image).map_err(|error| {
            fail(format!(
                "the copy is complete and kept as {}, but could not be renamed: {error}",
                partial.display()
            ))
        })?;
        // Best effort: exFAT and NTFS have no owners to give.
        let _ = std::os::unix::fs::chown(&image, Some(owner), None);
        let _ = std::os::unix::fs::chown(&folder, Some(owner), None);
        Ok(())
    }

    /// The daemon's side: run the helper and relay what it says.
    pub fn run(
        disk: &str,
        target: &str,
        name: &str,
        leave_out: &[String],
        switch: &AtomicBool,
        say: super::Say<'_>,
    ) -> io::Result<PathBuf> {
        let mut child = Command::new("pkexec")
            .arg(std::env::current_exe()?)
            .arg("--beatdrop-live")
            .args([disk, target, name])
            .args(leave_out)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let done = AtomicBool::new(false);
        let status = thread::scope(|scope| {
            let last = scope.spawn(|| {
                BufReader::new(stderr)
                    .lines()
                    .map_while(Result::ok)
                    .filter(|line| !line.trim().is_empty())
                    .last()
                    .unwrap_or_default()
            });
            scope.spawn(|| {
                let _stdin = stdin;
                while !done.load(Ordering::Relaxed) && !switch.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(200));
                }
            });
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                say(&line);
            }
            let status = child.wait();
            done.store(true, Ordering::Relaxed);
            status.map(|status| (status, last.join().unwrap_or_default()))
        });
        let (status, last) = status?;
        if switch.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        if !status.success() {
            return Err(fail(last.trim_start_matches("beatdrop: ").to_owned()));
        }
        Ok(Path::new(target)
            .join("BeatDrop")
            .join(format!("{name}{LIVE_SUFFIX}")))
    }
}

impl Api {
    pub(crate) fn beatdrop_inspect(&self) -> Result<Value, ApiFault> {
        let lsblk = scan()?;
        Ok(serde_json::to_value(BeatDropInspectView {
            sources: sources(&lsblk),
            targets: targets(&lsblk),
        })
        .expect("JSON"))
    }

    pub(crate) fn beatdrop_start(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let chosen = py::text_or_empty(&payload["source"]);
        let lsblk = scan()?;
        let in_use = sources(&lsblk)
            .into_iter()
            .find(|candidate| candidate.path == chosen)
            .filter(|candidate| !candidate.mountpoints.is_empty());
        let source = match in_use {
            Some(source) => source,
            None => selected(&chosen)?,
        };
        let name = &payload["name"];
        let clean = py::text_or_empty(name).trim().to_owned();
        if !a_name(&clean) {
            return Err(bad_name(name));
        }
        if SUFFIXES
            .iter()
            .any(|suffix| self.paths.disks.join(format!("{clean}{suffix}")).exists())
        {
            return Err(ApiFault::new(
                "disk-exists",
                format!("There is already a disk called {clean}."),
            ));
        }
        let raw = self.paths.disks.join(format!("{clean}{SUFFIX}"));
        if raw.with_extension("raw.beatdrop-partial").exists() {
            return Err(ApiFault::new(
                "beatdrop-partial",
                "A previous capture with this name needs attention.",
            ));
        }
        let live = if source.mountpoints.is_empty() {
            None
        } else {
            Some(self.live_target(
                &lsblk,
                &source,
                &clean,
                &payload["target"],
                &payload["leave_out"],
            )?)
        };
        // A live copy is a qcow2 elsewhere, linked into the library by name.
        let target = match live {
            None => raw,
            Some(_) => self.paths.disks.join(format!("{clean}{LIVE_SUFFIX}")),
        };
        let switch = Arc::new(AtomicBool::new(false));
        let active = Arc::clone(&self.capture);
        if !active.begin(Arc::clone(&switch)) {
            return Err(ApiFault::new("busy", "A disk capture is already running."));
        }
        let worker = Arc::clone(&switch);
        let job = run(&self.jobs, JOB, move |say| {
            let result = match &live {
                None => capture(source, target.clone(), &worker, say),
                #[cfg(unix)]
                Some((spot, leave_out)) => {
                    live::run(&source.path, spot, &clean, leave_out, &worker, say)
                        .and_then(|image| std::os::unix::fs::symlink(image, &target))
                }
                #[cfg(not(unix))]
                Some(_) => Err(io::Error::other("BeatDrop's live capture requires Linux")),
            };
            active.end();
            match result {
                Ok(()) => (true, Said::Text(target.to_string_lossy().into_owned())),
                Err(error) => (
                    false,
                    Said::Reason(reason!("beatdrop.copy_failed", detail = error)),
                ),
            }
        });
        if job.is_err() {
            self.capture.end();
        }
        let job = job?;
        Ok(crate::jobs::wire(&job))
    }

    /// Where a disk in use is rebuilt, and what is left out of it: a
    /// filesystem from the inventory with room for what is kept. It may be on
    /// the disk being copied; the copy then leaves itself out. The image lives
    /// there and the library links to it.
    fn live_target(
        &self,
        lsblk: &str,
        source: &BeatDropSourceView,
        name: &str,
        target: &Value,
        leave_out: &Value,
    ) -> Result<(String, Vec<String>), ApiFault> {
        if source.dependent {
            return Err(ApiFault::new(
                "beatdrop-dependent",
                "This disk belongs to a storage set that needs other disks.",
            ));
        }
        let parts = parts(lsblk, &source.path)
            .map_err(|detail| ApiFault::new("beatdrop-layout", detail))?;
        let table = mounts()?;
        let mut kept = Vec::new();
        let mut saved = 0i64;
        for entry in leave_out.as_array().into_iter().flatten() {
            let asked = py::text_or_empty(entry);
            let found = Path::new(&asked)
                .canonicalize()
                .ok()
                .filter(|path| path != Path::new("/"))
                .filter(|path| locate(&table, &parts, &path.to_string_lossy()).is_some())
                .ok_or_else(|| {
                    ApiFault::new(
                        "beatdrop-leave-out",
                        format!("{asked} is not something on {} to leave out.", source.path),
                    )
                })?;
            saved += i64::try_from(size_of(&found)).unwrap_or(0);
            kept.push(found.to_string_lossy().into_owned());
        }
        let wanted = py::text_or_empty(target);
        let spot = targets(lsblk)
            .into_iter()
            .find(|spot| spot.path == wanted)
            .ok_or_else(|| ApiFault::new("beatdrop-target", "Choose where to keep the copy."))?;
        if spot.free < source.needed - saved {
            return Err(ApiFault::new(
                "beatdrop-space",
                format!(
                    "{} needs {} GB free.",
                    spot.path,
                    (source.needed - saved) / 1_000_000_000 + 1
                ),
            ));
        }
        let image = Path::new(&spot.path)
            .join("BeatDrop")
            .join(format!("{name}{LIVE_SUFFIX}"));
        if image.exists() || image.with_extension("qcow2.beatdrop-partial").exists() {
            return Err(ApiFault::new(
                "beatdrop-partial",
                format!("{} already holds a copy with this name.", spot.path),
            ));
        }
        Ok((spot.path, kept))
    }

    pub(crate) fn beatdrop_folders(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let asked = py::text_or_empty(&payload["path"]);
        let folder = if asked.is_empty() {
            std::env::var("HOME").unwrap_or_default()
        } else {
            asked
        };
        let output = Command::new("du")
            .args(["-x", "-B1", "-d1"])
            .arg(&folder)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|error| ApiFault::new("beatdrop-leave-out", error.to_string()))?;
        Ok(
            serde_json::to_value(folders(&String::from_utf8_lossy(&output.stdout), &folder))
                .expect("JSON"),
        )
    }

    pub(crate) fn beatdrop_measure(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let path = Path::new(&py::text_or_empty(&payload["path"]))
            .canonicalize()
            .map_err(|error| ApiFault::new("beatdrop-leave-out", error.to_string()))?;
        Ok(Value::from(size_of(&path)))
    }

    pub(crate) fn beatdrop_cancel(&self) -> Result<Value, ApiFault> {
        Ok(Value::Bool(self.capture.cancel()))
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Seek, SeekFrom};

    use super::*;

    #[test]
    fn inventory_checks_children_and_ignores_distribution_names() {
        let inventory = r#"{"blockdevices":[
          {"name":"zram0","path":"/dev/zram0","type":"disk","size":100,"mountpoints":[]},
          {"name":"nbd0","path":"/dev/nbd0","type":"disk","size":0,"mountpoints":[]},
          {"name":"sda","path":"/dev/sda","type":"disk","size":8192,"mountpoints":[],
           "children":[{"name":"sda1","type":"part","mountpoints":["/boot"]},
                       {"name":"sda2","type":"part","mountpoints":["/"]}]},
          {"name":"sdb","path":"/dev/sdb","type":"disk","size":4096,"mountpoints":[]}
        ]}"#;
        let found = sources(inventory);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, "/dev/sda");
        assert_eq!(found[0].mountpoints, ["/", "/boot"]);
        assert!(found[1].mountpoints.is_empty());
        assert!(!found[1].dependent);
    }

    #[test]
    fn a_volume_shared_across_physical_disks_is_not_a_complete_single_disk() {
        let inventory = r#"{"blockdevices":[
          {"name":"sda","path":"/dev/sda","type":"disk","size":8192,"mountpoints":[],
           "children":[{"path":"/dev/mapper/system","type":"lvm","mountpoints":[]}]},
          {"name":"sdb","path":"/dev/sdb","type":"disk","size":8192,"mountpoints":[],
           "children":[{"path":"/dev/mapper/system","type":"lvm","mountpoints":[]}]}
        ]}"#;
        let found = sources(inventory);
        assert!(found.iter().all(|disk| disk.dependent));
    }

    const IN_USE: &str = r#"{"blockdevices":[
      {"name":"nvme0n1","path":"/dev/nvme0n1","type":"disk","size":10000000,"mountpoints":[],
       "children":[
        {"name":"nvme0n1p1","path":"/dev/nvme0n1p1","type":"part","size":1048576,"start":2048,
         "fstype":"vfat","fsver":"FAT32","uuid":"0CC5-7E46","label":null,"fsused":1000,"mountpoints":["/boot"]},
        {"name":"nvme0n1p2","path":"/dev/nvme0n1p2","type":"part","size":4194304,"start":4096,
         "fstype":"ext4","fsver":"1.0","uuid":"d0c1","label":"root","fsused":3000,"mountpoints":["/"]},
        {"name":"nvme0n1p3","path":"/dev/nvme0n1p3","type":"part","size":1048576,"start":12288,
         "fstype":"ntfs","uuid":"x","mountpoints":[]}]},
      {"name":"sda","path":"/dev/sda","type":"disk","size":8000000,"mountpoints":[],
       "children":[{"name":"sda1","path":"/dev/sda1","type":"part","fstype":"ext4",
                    "fsavail":7000000,"mountpoints":["/run/media/me/big"]}]},
      {"name":"sdb","path":"/dev/sdb","type":"disk","size":8000000,"mountpoints":[],
       "children":[{"name":"sdb1","path":"/dev/sdb1","type":"part","fstype":"exfat",
                    "fsavail":7000000,"mountpoints":["/run/media/me/stick"]}]}
    ]}"#;

    #[test]
    fn a_disk_in_use_is_planned_as_rebuilt_filesystems_and_raw_gaps() {
        let parts = parts(IN_USE, "/dev/nvme0n1").unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].start, 2048 * 512);
        assert!(parts[0].mounted && parts[1].mounted && !parts[2].mounted);
        // The tables before p1, the gap and unmounted p3, and the tail.
        assert_eq!(
            raw_ranges(&parts, 10_000_000),
            [(0, 1_048_576), (6_291_456, 10_000_000)]
        );
        assert_eq!(
            needed(&parts, 10_000_000),
            (1_048_576 + 3_708_544 + 4000) / 100 * 101
        );
        let source = sources(IN_USE).remove(0);
        assert_eq!(source.needed, needed(&parts, 10_000_000) as i64);
    }

    #[test]
    fn a_live_capture_refuses_layers_it_cannot_rebuild() {
        let crypt = IN_USE.replace(
            r#""mountpoints":["/"]}"#,
            r#""mountpoints":[],"children":[{"path":"/dev/mapper/root","type":"crypt","mountpoints":["/"]}]}"#,
        );
        assert!(
            parts(&crypt, "/dev/nvme0n1")
                .unwrap_err()
                .contains("encryption")
        );
        let xfs = IN_USE.replace(r#""fstype":"ext4","fsver":"1.0""#, r#""fstype":"xfs""#);
        assert!(parts(&xfs, "/dev/nvme0n1").unwrap_err().contains("xfs"));
    }

    #[test]
    fn targets_are_writable_filesystems_named_with_their_disk() {
        // The source's own root is listed too; start and the page leave out
        // every target on the disk being copied.
        let found: Vec<_> = targets(IN_USE)
            .into_iter()
            .filter(|spot| spot.disk != "/dev/nvme0n1")
            .collect();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, "/run/media/me/big");
        // exFAT holds a qcow2 as well as anything does.
        assert_eq!(found[1].path, "/run/media/me/stick");
        assert_eq!(found[0].disk, "/dev/sda");
        assert_eq!(found[0].free, 7_000_000);
    }

    const MOUNTS: &str = r#"{"filesystems":[
      {"target":"/","source":"/dev/nvme0n1p2","fsroot":"/"},
      {"target":"/boot","source":"/dev/nvme0n1p1","fsroot":"/"},
      {"target":"/run/media/me/big","source":"/dev/sda1","fsroot":"/"},
      {"target":"/var/lib/docker/volumes/kali/_data",
       "source":"/dev/nvme0n1p3[/alice/.labs/kali]","fsroot":"/alice/.labs/kali"}
    ]}"#;

    #[test]
    fn a_path_is_located_on_its_own_filesystem_and_never_on_another_disk() {
        let mut parts = parts(IN_USE, "/dev/nvme0n1").unwrap();
        parts[2].path = "/dev/nvme0n1p3".into();
        parts[2].mounted = true;
        let found = |path: &str| locate(MOUNTS, &parts, path);
        assert_eq!(
            found("/home/alice/Downloads"),
            Some(("/dev/nvme0n1p2".into(), "/home/alice/Downloads".into()))
        );
        assert_eq!(
            found("/boot/EFI"),
            Some(("/dev/nvme0n1p1".into(), "/EFI".into()))
        );
        // A bind mount of a folder of another partition.
        assert_eq!(
            found("/var/lib/docker/volumes/kali/_data/x"),
            Some(("/dev/nvme0n1p3".into(), "/alice/.labs/kali/x".into()))
        );
        assert_eq!(found("/run/media/me/big/BeatDrop"), None);
        // /bootstrap is on /, not on /boot.
        assert_eq!(found("/bootstrap").unwrap().0, "/dev/nvme0n1p2");
    }

    #[test]
    fn a_left_out_folder_stays_as_an_empty_folder() {
        assert_eq!(
            leave_out_rule("/alice/Downloads", true),
            "/alice/Downloads/**"
        );
        assert_eq!(leave_out_rule("/", true), "/**");
        assert_eq!(leave_out_rule("/alice/big.iso", false), "/alice/big.iso");
        assert_eq!(leave_out_rule("/alice/a[1]*", false), "/alice/a\\[1]\\*");
    }

    #[test]
    fn home_folders_are_listed_largest_first_without_the_home_itself() {
        let du = "1100000\t/home/alice/.minecraft\n116000000\t/home/alice/VirtualBox VMs\n200000000\t/home/alice\n";
        let found = folders(du, "/home/alice/");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "VirtualBox VMs");
        assert_eq!(found[0].bytes, 116_000_000);
        assert_eq!(found[1].path, "/home/alice/.minecraft");
    }

    #[test]
    fn rsync_errors_keep_the_last_real_complaints() {
        let stderr = "file has vanished: \"/a\"\nrsync: [sender] read errors mapping \"/alice/x\": No data available (61)\nrsync error: some files/attrs were not transferred (see previous errors) (code 23)\n";
        assert_eq!(
            rsync_errors(stderr.as_bytes()),
            ["rsync: [sender] read errors mapping \"/alice/x\": No data available (61)"]
        );
    }

    #[test]
    fn a_rebuilt_fat32_never_falls_under_its_minimum_cluster_count() {
        let gib = 1 << 30;
        // The 1 GiB ESP that did not boot: 64 sectors a cluster, 32758 clusters.
        assert_eq!(fat_cluster(64, gib, "FAT32"), 16);
        assert!(gib / 512 / u64::from(fat_cluster(64, gib, "FAT32")) > 65_525);
        assert_eq!(fat_cluster(8, gib, "FAT32"), 8);
        assert_eq!(fat_cluster(64, gib, "FAT16"), 64);
    }

    #[test]
    fn rsync_progress_is_read_from_its_status_line() {
        assert_eq!(
            rsync_percent("  1,234,567  37%  1.00MB/s    0:00:01 (xfr#3, to-chk=0/9)"),
            Some(37)
        );
        assert_eq!(rsync_percent("sending incremental file list"), None);
    }

    #[test]
    fn sparse_copy_preserves_logical_bytes_and_can_be_cancelled() {
        let path = std::env::temp_dir().join(format!(
            "beatdrop-copy-test-{}-{}",
            std::process::id(),
            beatbox_isolation::uuid4()
        ));
        let mut bytes = vec![0u8; CHUNK + 17];
        bytes[CHUNK] = 0x55;
        bytes[CHUNK + 1] = 0xaa;
        let mut target = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let cancel = AtomicBool::new(false);
        copy_sparse(
            &mut Cursor::new(&bytes),
            &mut target,
            bytes.len() as u64,
            &cancel,
            &|_| {},
        )
        .unwrap();
        target.set_len(bytes.len() as u64).unwrap();
        assert_eq!(target.metadata().unwrap().len(), bytes.len() as u64);
        target.seek(SeekFrom::Start(0)).unwrap();
        let mut actual = Vec::new();
        target.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, bytes);
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            copy_sparse(
                &mut Cursor::new(&bytes),
                &mut target,
                bytes.len() as u64,
                &cancel,
                &|_| {}
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn only_unfinished_beatdrop_images_are_swept() {
        let folder = std::env::temp_dir().join(format!(
            "beatdrop-sweep-test-{}-{}",
            std::process::id(),
            beatbox_isolation::uuid4()
        ));
        fs::create_dir(&folder).unwrap();
        let partial = folder.join("machine.raw.beatdrop-partial");
        let complete = folder.join("machine.raw");
        let other = folder.join("other.part");
        for path in [&partial, &complete, &other] {
            fs::write(path, b"keep").unwrap();
        }
        sweep_partials(&folder);
        assert!(!partial.exists());
        assert!(complete.exists());
        assert!(other.exists());
        fs::remove_dir_all(folder).unwrap();
    }
}
