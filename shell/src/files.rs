//! One folder on this computer, described well enough to draw it.
//!
//! The window's, not the daemon's: the person's files are where the window
//! is, which in the cloud isn't where the machines run.
//!
//! BeatBox draws its own chooser: `<input type=file>` hands over bytes, and a
//! sandbox wants a path QEMU opens itself. A native dialog came in the
//! desktop's style and language, out of place in the product.
//!
//! Nothing here writes, deletes, moves or runs anything but `xdg-user-dir`.
//! The one thing a caller can provoke is a `stat` on something it can
//! already see.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use beatbox_contract::{Crumb, FileEntry, FolderView, Place};

use crate::{Fault, host_command};

/// Past this, a folder is cut short: 40,000 build artefacts is a hang, not a
/// chooser. The page says how many it isn't showing.
pub const CAP: usize = 1500;

fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Where the disks BeatBox makes are kept: `~/.beatbox/disks`, from the
/// home directory, never `BEATBOX_HOME` (as the daemon reads it).
pub fn disks_folder() -> PathBuf {
    home().join(".beatbox").join("disks")
}

/// `Path.resolve()`: the real path of what is there, the absolute one of
/// what is not, and on Windows without the `\\?\` that canonicalising adds.
pub fn resolved(path: &Path) -> PathBuf {
    let real = fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf());
    plain(real)
}

fn plain(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

/// The two or three folders worth a single click, only ones that exist, and
/// each once: a rail with a dead entry in it is a rail nobody trusts.
pub fn places() -> Vec<Place> {
    let root = Path::new(std::path::MAIN_SEPARATOR_STR).to_path_buf();
    let root = if cfg!(windows) {
        std::env::var_os("SystemDrive")
            .map(|drive| PathBuf::from(format!("{}\\", drive.to_string_lossy())))
            .unwrap_or(root)
    } else {
        root
    };
    let candidates = [
        ("disks", "box", disks_folder()),
        ("home", "home", home()),
        ("downloads", "folder", user_dir("DOWNLOAD", "Downloads")),
        ("desktop", "folder", user_dir("DESKTOP", "Desktop")),
        ("root", "box", root),
    ];
    let mut found: Vec<Place> = Vec::new();
    for (key, icon, path) in candidates {
        let path = resolved(&path);
        let text = path.to_string_lossy().into_owned();
        if !path.is_dir() || found.iter().any(|place| place.path == text) {
            continue;
        }
        found.push(Place {
            key: key.into(),
            icon: icon.into(),
            path: text,
        });
    }
    found
}

/// Where the desktop keeps Downloads or Desktop: a Portuguese system names
/// them otherwise, so ask `xdg-user-dir` like native choosers do. Falls back
/// to the English name; one that doesn't exist is dropped.
pub fn user_dir(kind: &str, fallback: &str) -> PathBuf {
    user_dir_asking("xdg-user-dir", kind, fallback)
}

/// The same, asking `program`, which tests point at one that isn't there.
pub fn user_dir_asking(program: &str, kind: &str, fallback: &str) -> PathBuf {
    if cfg!(unix)
        && let Ok(found) = host_command(program).arg(kind).output()
        && found.status.success()
    {
        let text = String::from_utf8_lossy(&found.stdout).trim().to_owned();
        if !text.is_empty() {
            return PathBuf::from(text);
        }
    }
    home().join(fallback)
}

fn seconds(time: std::io::Result<std::time::SystemTime>) -> f64 {
    time.ok()
        .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |since| Duration::as_secs_f64(&since))
}

/// One row: a name that exists is a row, even when nothing more about it
/// can be read -- a broken symlink, a device that is not there -- and then
/// its size and time are the zeroes that say so.
fn entry(path: &Path) -> FileEntry {
    let folder = path.is_dir();
    let (size, modified) = match fs::metadata(path) {
        Ok(meta) if folder => (0, seconds(meta.modified())),
        Ok(meta) => (
            i64::try_from(meta.len()).unwrap_or(i64::MAX),
            seconds(meta.modified()),
        ),
        Err(_) => (0, 0.0),
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    FileEntry {
        hidden: name.starts_with('.'),
        name,
        path: path.to_string_lossy().into_owned(),
        folder,
        size,
        modified,
    }
}

/// The most `files.read` hands back: it is for small files someone chose
/// (a network to import), not for disks.
const READ_MAX: u64 = 4 * 1024 * 1024;

/// A small text file the person chose, as text.
pub fn read(path: &str) -> Result<String, Fault> {
    let path = match path.strip_prefix('~') {
        Some(rest) => home().join(rest.trim_start_matches(['/', '\\'])),
        None => PathBuf::from(path),
    };
    let meta = fs::metadata(&path)
        .ok()
        .filter(|meta| meta.is_file())
        .ok_or_else(|| {
            Fault::new(
                "no-such-file",
                format!("There is no file at {}.", path.display()),
            )
        })?;
    if meta.len() > READ_MAX {
        return Err(Fault::new(
            "too-big",
            format!("{} is too big to read here.", path.display()),
        ));
    }
    fs::read_to_string(&path).map_err(|error| {
        Fault::new(
            "cannot-read",
            format!("{} cannot be read: {error}", path.display()),
        )
    })
}

/// What is in one folder.
///
/// `suffixes` is space-separated (`".qcow2 .iso"`), empty for everything,
/// and filtered here so ten thousand names don't cross the bridge to be
/// dropped. Folders first, then names ignoring case; folders always stay,
/// since hiding the way to a file hides the file. A file as `path` opens
/// its folder.
pub fn listing(path: &str, suffixes: &str) -> Result<FolderView, Fault> {
    let asked = if path.is_empty() {
        home()
    } else if let Some(rest) = path.strip_prefix('~') {
        home().join(rest.trim_start_matches(['/', '\\']))
    } else {
        PathBuf::from(path)
    };
    let mut target = resolved(&asked);
    if target.is_file()
        && let Some(parent) = target.parent()
    {
        target = parent.to_path_buf();
    }
    if !target.is_dir() {
        return Err(Fault::new(
            "no-such-folder",
            format!("There is no folder at {}.", target.display()),
        ));
    }

    let wanted: Vec<String> = suffixes.split_whitespace().map(str::to_lowercase).collect();
    let read = fs::read_dir(&target).map_err(|error| {
        let (code, how) = if error.kind() == std::io::ErrorKind::PermissionDenied {
            ("cannot-read", "cannot be read")
        } else {
            ("cannot-read", "cannot be listed")
        };
        Fault::new(code, format!("{} {how}: {error}", target.display()))
    })?;
    let found: Vec<FileEntry> = read
        .filter_map(Result::ok)
        .map(|child| entry(&child.path()))
        .collect();
    let total = found.len();
    let mut rows: Vec<FileEntry> = found
        .into_iter()
        .filter(|row| {
            row.folder || wanted.is_empty() || {
                let name = row.name.to_lowercase();
                wanted.iter().any(|suffix| name.ends_with(suffix.as_str()))
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        (!a.folder, a.name.to_lowercase()).cmp(&(!b.folder, b.name.to_lowercase()))
    });
    let filtered = total - rows.len();
    let beyond = rows.len().saturating_sub(CAP);
    rows.truncate(CAP);

    let parent = target
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(FolderView {
        path: target.to_string_lossy().into_owned(),
        parent,
        crumbs: crumbs(&target),
        entries: rows,
        beyond: i64::try_from(beyond).unwrap_or(i64::MAX),
        filtered: i64::try_from(filtered).unwrap_or(i64::MAX),
    })
}

/// Every folder between the top of the disk and this one, so that any of
/// them is one click. A crumb's name is spelt the way the disk spells it; the
/// top of the disk, which has no name, is written as itself -- `/`, `C:\`.
fn crumbs(target: &Path) -> Vec<Crumb> {
    let mut trail: Vec<Crumb> = target
        .ancestors()
        .map(|part| {
            let name = match part.components().next_back() {
                Some(Component::Normal(name)) => name.to_string_lossy().into_owned(),
                _ => part.to_string_lossy().into_owned(),
            };
            Crumb {
                name,
                path: part.to_string_lossy().into_owned(),
            }
        })
        .collect();
    trail.reverse();
    trail
}
