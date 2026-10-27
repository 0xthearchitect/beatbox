//! Disks another program made: VirtualBox's `.vdi`, VMware's `.vmdk`,
//! Hyper-V's `.vhd`/`.vhdx`, and the `.ova` a machine is exported as.
//!
//! **Used where they are, never copied or written.** QEMU reads all four,
//! and a machine's own disk is a qcow2 layer on top. Only an `.ova` (a tar)
//! has its disk taken out once, into BeatBox's disks folder.
//!
//! Problems are said before a machine is made. VirtualBox snapshots live in
//! difference files QEMU can't follow, so the main `.vdi` is the machine as
//! it was before them, and a difference file alone isn't a disk. The
//! firmware is read off the disk, so a UEFI install boots under UEFI.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use beatbox_contract::DiskInspectView;
use beatbox_engine::reason;
use beatbox_isolation::firmware::disk_start;
use beatbox_isolation::machine::disk_format;
use serde_json::{Map, Value, json};

use crate::Api;
use crate::disks::{SUFFIXES, a_name, bad_name};
use crate::fault::ApiFault;
use crate::guest_tools::run;
use crate::jobs::Said;
use crate::py;
use crate::sandbox::reason_view;

/// The job an unpacking is.
pub const UNPACK_JOB: &str = "unpack";
/// What a disk taken out of an `.ova` is saved as.
pub const UNPACKED_SUFFIX: &str = ".vmdk";

/// One file in a tar archive: its name, and where its bytes are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TarEntry {
    /// The name it was stored under.
    pub name: String,
    /// Where its bytes start.
    pub offset: u64,
    /// How many there are.
    pub size: u64,
}

fn text_field(header: &[u8]) -> String {
    let end = header.iter().position(|&b| b == 0).unwrap_or(header.len());
    String::from_utf8_lossy(&header[..end]).trim().to_owned()
}

/// A tar size: octal, or -- past 8 GB, as GNU tar writes it -- base-256
/// with the top bit of the first byte set.
fn tar_size(field: &[u8]) -> Option<u64> {
    if field[0] & 0x80 != 0 {
        return Some(
            field[1..]
                .iter()
                .fold(0u64, |value, byte| value << 8 | u64::from(*byte)),
        );
    }
    let text = text_field(field);
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(&text, 8).ok()
}

/// The regular files in a tar archive, with where each one's bytes are.
/// ustar names with their prefix, and GNU's long names; nothing is read
/// but the headers.
pub fn tar_entries(archive: &mut (impl Read + Seek)) -> io::Result<Vec<TarEntry>> {
    let mut entries = Vec::new();
    let mut header = [0u8; 512];
    let mut long_name: Option<String> = None;
    let mut at = 0u64;
    loop {
        archive.seek(SeekFrom::Start(at))?;
        if archive.read_exact(&mut header).is_err() || header.iter().all(|&b| b == 0) {
            break;
        }
        let size = tar_size(&header[124..136])
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a tar archive"))?;
        let data = at + 512;
        match header[156] {
            b'L' => {
                let mut name = vec![0u8; usize::try_from(size.min(4096)).unwrap_or(4096)];
                archive.read_exact(&mut name)?;
                long_name = Some(text_field(&name));
            }
            b'0' | 0 => {
                let mut name = text_field(&header[0..100]);
                if &header[257..262] == b"ustar" {
                    let prefix = text_field(&header[345..500]);
                    if !prefix.is_empty() {
                        name = format!("{prefix}/{name}");
                    }
                }
                entries.push(TarEntry {
                    name: long_name.take().unwrap_or(name),
                    offset: data,
                    size,
                });
            }
            _ => long_name = None,
        }
        at = data + size.div_ceil(512) * 512;
    }
    Ok(entries)
}

/// The first disk in an `.ova`, or why there is none that can be used.
fn ova_disk(path: &Path) -> Result<TarEntry, beatbox_engine::Reason> {
    let mut archive =
        File::open(path).map_err(|error| reason!("disks.unreadable", detail = error))?;
    let entries =
        tar_entries(&mut archive).map_err(|error| reason!("disks.unreadable", detail = error))?;
    let lower = |entry: &&TarEntry| entry.name.to_lowercase();
    if let Some(disk) = entries.iter().find(|entry| lower(entry).ends_with(".vmdk")) {
        return Ok(disk.clone());
    }
    if entries
        .iter()
        .any(|entry| lower(&entry).ends_with(".vmdk.gz"))
    {
        return Err(reason!("disks.ova_compressed"));
    }
    Err(reason!("disks.ova_without_disk"))
}

/// Whether VirtualBox keeps snapshots of the machine this disk belongs to:
/// a `Snapshots` folder beside it, with disks in it.
pub fn virtualbox_snapshots_beside(disk: &Path) -> bool {
    let Some(folder) = disk.parent() else {
        return false;
    };
    fs::read_dir(folder.join("Snapshots")).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            [".vdi", ".vmdk", ".vhd"]
                .iter()
                .any(|suffix| name.ends_with(suffix))
        })
    })
}

/// What `qemu-img` said about a disk it could not open, said for a person:
/// VirtualBox's difference files are the one worth naming.
fn unreadable(detail: &str) -> beatbox_engine::Reason {
    if detail.contains("link UUID") || detail.contains("parent UUID") {
        reason!("disks.virtualbox_difference")
    } else {
        reason!("disks.unreadable", detail = detail)
    }
}

impl Api {
    /// What a disk is and whether it will do, before a machine is made of it.
    pub(crate) fn disks_inspect(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let path = py::text_or_empty(&payload["path"]);
        let disk = Path::new(&path);
        let mut view = DiskInspectView {
            format: String::new(),
            bytes: 0,
            firmware: String::new(),
            bootable: true,
            virtualbox_snapshots: false,
            inside: String::new(),
            installer: String::new(),
            problem: None,
        };
        if !disk.is_file() {
            view.problem = Some(reason_view(&reason!("disks.not_there", path = path)));
            return Ok(serde_json::to_value(view).expect("a view is JSON"));
        }
        // A disc is something to install from: "windows" installs unattended
        // (disks.build), "disc" is clicked through. It boots via El Torito,
        // not an MBR, so the checks below would call it unbootable.
        if beatbox_isolation::machine::is_optical(&path) {
            let windows = beatbox_isolation::machine::windows_installer_language(&path).is_some();
            view.installer = if windows { "windows" } else { "disc" }.into();
            view.format = "iso".into();
            view.bytes =
                fs::metadata(disk).map_or(0, |meta| i64::try_from(meta.len()).unwrap_or(i64::MAX));
            return Ok(serde_json::to_value(view).expect("a view is JSON"));
        }
        if path.to_lowercase().ends_with(".ova") {
            view.format = "ova".into();
            match ova_disk(disk) {
                Ok(entry) => {
                    view.inside = entry.name;
                    view.bytes = i64::try_from(entry.size).unwrap_or(i64::MAX);
                }
                Err(why) => view.problem = Some(reason_view(&why)),
            }
            return Ok(serde_json::to_value(view).expect("a view is JSON"));
        }
        let qemu = (self.installer.qemu)();
        let runner = self.installer.runner.as_ref();
        let argv = vec![
            qemu.image.clone(),
            "info".into(),
            "--output=json".into(),
            path.clone(),
        ];
        match runner.run(&argv, None) {
            Ok(output) if output.code == 0 => {
                let info: Value = serde_json::from_str(&output.stdout).unwrap_or_default();
                view.format = info["format"].as_str().unwrap_or_default().to_owned();
                view.bytes = info["virtual-size"].as_i64().unwrap_or(0);
            }
            Ok(output) => {
                view.problem = Some(reason_view(&unreadable(output.stderr.trim())));
                return Ok(serde_json::to_value(view).expect("a view is JSON"));
            }
            Err(error) => {
                view.problem = Some(reason_view(&unreadable(&error.to_string())));
                return Ok(serde_json::to_value(view).expect("a view is JSON"));
            }
        }
        let _ = fs::create_dir_all(&self.paths.temp);
        let scratch = self
            .paths
            .temp
            .join(format!(".head-{}", std::process::id()));
        let format = disk_format(&path, &qemu, runner).unwrap_or_else(|_| view.format.clone());
        let (firmware, bootable) = disk_start(&path, &format, &qemu, runner, &scratch);
        view.firmware = firmware.to_owned();
        view.bootable = bootable;
        view.virtualbox_snapshots = virtualbox_snapshots_beside(disk);
        Ok(serde_json::to_value(view).expect("a view is JSON"))
    }

    /// Take the disk out of an `.ova` into the disks folder, as a job.
    pub(crate) fn disks_unpack(&self, payload: &Map<String, Value>) -> Result<Value, ApiFault> {
        let name = &payload["name"];
        let clean = py::text_or_empty(name).trim().to_owned();
        if !a_name(&clean) {
            return Err(bad_name(name));
        }
        let source = PathBuf::from(py::text_or_empty(&payload["source"]));
        let entry = ova_disk(&source).map_err(|why| {
            ApiFault::because(
                "bad-archive",
                "There is no disk to take out of this.",
                Some(why),
            )
        })?;
        let target = self.paths.disks.join(format!("{clean}{UNPACKED_SUFFIX}"));
        if SUFFIXES
            .iter()
            .any(|suffix| self.paths.disks.join(format!("{clean}{suffix}")).exists())
        {
            return Err(ApiFault::new(
                "disk-exists",
                format!("There is already a disk called {clean}."),
            ));
        }
        let _ = fs::create_dir_all(&self.paths.disks);
        let job = run(&self.jobs, UNPACK_JOB, move |say| {
            let partial = target.with_extension("vmdk.part");
            let copied = (|| -> io::Result<()> {
                let mut archive = File::open(&source)?;
                archive.seek(SeekFrom::Start(entry.offset))?;
                let mut from = archive.take(entry.size);
                let mut out = File::create(&partial)?;
                let mut chunk = vec![0u8; 1 << 20];
                let (mut done, mut said) = (0u64, 0u64);
                loop {
                    let read = from.read(&mut chunk)?;
                    if read == 0 {
                        break;
                    }
                    out.write_all(&chunk[..read])?;
                    done += read as u64;
                    let percent = done * 100 / entry.size.max(1);
                    if percent >= said + 5 {
                        said = percent;
                        say(&format!("{percent}%"));
                    }
                }
                if done != entry.size {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the archive ends early",
                    ));
                }
                out.sync_all()?;
                fs::rename(&partial, &target)
            })();
            match copied {
                Ok(()) => (true, Said::Text(target.to_string_lossy().into_owned())),
                Err(error) => {
                    let _ = fs::remove_file(&partial);
                    (
                        false,
                        Said::Reason(reason!("disks.unpack_failed", detail = error)),
                    )
                }
            }
        })?;
        Ok(json!(crate::jobs::wire(&job)))
    }
}
