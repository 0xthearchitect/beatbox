//! Where Linux and Windows differ, in one place, and nowhere else.
//!
//! File locks, KVM, free memory, `SIGKILL`, finding QEMU: each answered here
//! (control channels in [`crate::endpoint`]) so callers never grow a `cfg`.
//!
//! The Windows answers need the Windows API, which has no safe way in. The
//! calls below are the only `unsafe` in this crate, each argued where made.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Whether this build runs on Windows.
pub const WINDOWS: bool = cfg!(windows);
/// Whether this build runs on Linux.
pub const LINUX: bool = cfg!(target_os = "linux");

// --- starting a program ------------------------------------------------------------

/// A program for the daemon to start.
///
/// The daemon has no console to share, so on Windows each child would open
/// an empty black window of its own, and closing it kills the child.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

// --- where QEMU is -----------------------------------------------------------------

/// The full path to one of QEMU's programs, or `None`.
///
/// `BEATBOX_QEMU_DIR` first, for a QEMU somewhere else entirely. Then `PATH`,
/// everywhere. On Windows, then the places QEMU's installers put it without
/// adding it to `PATH` -- which the official one does not: `C:\Program
/// Files\qemu`, MSYS2's two toolchains and Scoop.
pub fn qemu_program(name: &str) -> Option<PathBuf> {
    qemu_program_with(name, &|key| env::var_os(key), WINDOWS)
}

/// [`qemu_program`], with the environment and the system handed in, so both
/// systems' answers can be tested on either.
pub fn qemu_program_with(
    name: &str,
    variable: &dyn Fn(&str) -> Option<OsString>,
    windows: bool,
) -> Option<PathBuf> {
    let extensions = extensions(variable, windows);
    if let Some(folders) = variable("BEATBOX_QEMU_DIR").filter(|value| !value.is_empty())
        && let Some(found) = which_in(name, env::split_paths(&folders), &extensions)
    {
        return Some(found);
    }
    let path = variable("PATH").unwrap_or_default();
    if let Some(found) = which_in(name, env::split_paths(&path), &extensions) {
        return Some(found);
    }
    if !windows {
        return None;
    }
    windows_qemu_folders(variable)
        .into_iter()
        .map(|folder| folder.join(format!("{name}.exe")))
        .find(|candidate| candidate.is_file())
}

/// What a bare name may be completed with: nothing on Linux, `PATHEXT` on
/// Windows.
fn extensions(variable: &dyn Fn(&str) -> Option<OsString>, windows: bool) -> Vec<String> {
    if !windows {
        return vec![String::new()];
    }
    let listed = variable("PATHEXT")
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_owned());
    let mut found = vec![String::new()];
    found.extend(
        listed
            .split(';')
            .filter(|e| !e.is_empty())
            .map(str::to_owned),
    );
    found
}

/// Whether a program is on `PATH`, completed with `PATHEXT` on Windows.
pub fn on_path(name: &str) -> bool {
    let variable = |key: &str| env::var_os(key);
    let path = env::var_os("PATH").unwrap_or_default();
    which_in(
        name,
        env::split_paths(&path),
        &extensions(&variable, WINDOWS),
    )
    .is_some()
}

fn which_in(
    name: &str,
    folders: impl Iterator<Item = PathBuf>,
    extensions: &[String],
) -> Option<PathBuf> {
    for folder in folders {
        for extension in extensions {
            let candidate = folder.join(format!("{name}{extension}"));
            if executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn windows_qemu_folders(variable: &dyn Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let mut folders = Vec::new();
    for key in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = variable(key).filter(|value| !value.is_empty()) {
            folders.push(PathBuf::from(root).join("qemu"));
        }
    }
    folders.push(PathBuf::from("C:/msys64/ucrt64/bin"));
    folders.push(PathBuf::from("C:/msys64/mingw64/bin"));
    if let Some(profile) = variable("USERPROFILE").filter(|value| !value.is_empty()) {
        folders.push(
            PathBuf::from(profile)
                .join("scoop")
                .join("apps")
                .join("qemu")
                .join("current"),
        );
    }
    folders
}

// --- paths -------------------------------------------------------------------------

/// `path` absolute with links followed, and on Windows without the `\\?\`
/// prefix `canonicalize` adds: QEMU reads a backing file with it as a path
/// that doesn't exist.
pub fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    Ok(plain(fs::canonicalize(path)?))
}

/// `\\?\C:\x` as `C:\x`, and `\\?\UNC\host\x` as `\\host\x`; anything
/// else as it is.
pub fn plain(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\")
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return PathBuf::from(rest);
    }
    path
}

// --- memory ------------------------------------------------------------------------

/// `(available, total)` in MiB, or `None` for whichever cannot be read.
///
/// Available as in *can be handed out without swapping*: `MemAvailable` on
/// Linux, `ullAvailPhys` on Windows. Neither counts swap or the page file --
/// a guest living in either is one nobody can use.
pub fn memory_mb() -> (Option<u64>, Option<u64>) {
    #[cfg(windows)]
    {
        windows::memory_mb()
    }
    #[cfg(not(windows))]
    {
        match fs::read_to_string("/proc/meminfo") {
            Ok(text) => parse_meminfo(&text),
            Err(_) => (None, None),
        }
    }
}

/// `/proc/meminfo`'s two lines that matter, in MiB.
pub fn parse_meminfo(text: &str) -> (Option<u64>, Option<u64>) {
    let read = |field: &str| {
        text.lines().find_map(|line| {
            let (name, rest) = line.split_once(':')?;
            (name == field).then(|| rest.split_whitespace().next()?.parse::<u64>().ok())?
        })
    };
    (
        read("MemAvailable").map(|kib| kib / 1024),
        read("MemTotal").map(|kib| kib / 1024),
    )
}

// --- acceleration ------------------------------------------------------------------

/// Whether Windows will run a guest on its own hypervisor: WHPX.
///
/// The Windows Hypervisor Platform is an optional Windows feature, and QEMU's
/// `-accel whpx` needs it on. Asked of Windows itself, the way QEMU does:
/// `WHvGetCapability(WHvCapabilityCodeHypervisorPresent)`. The DLL only
/// exists with the feature installed, so its absence is an answer too.
pub fn whpx_available() -> bool {
    #[cfg(windows)]
    {
        windows::whpx_available()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

// --- what this is ---------------------------------------------------------------

/// `Linux` or `Windows`, which is all BeatBox runs on.
pub fn system_name() -> &'static str {
    if WINDOWS {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "Darwin"
    } else if LINUX {
        "Linux"
    } else {
        std::env::consts::OS
    }
}

/// The kernel release on Linux (`7.1.5-zen1-2-zen`); on Windows `10`, or `11`
/// from build 22000 (servers read as their desktop twin). Empty if unknown.
pub fn release() -> String {
    #[cfg(windows)]
    {
        windows::release()
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|text| text.trim().to_owned())
            .unwrap_or_default()
    }
}

// --- one owner at a time -----------------------------------------------------------

/// Take an exclusive, non-blocking lock on an open file, or say no.
///
/// `flock` on Linux, `LockFileEx` on Windows. The OS lets go when the process
/// ends, crash or not, which is what the single-owner rule rests on. Covers
/// the Python build's lock on the same file too.
pub fn lock_exclusively(file: &File) -> bool {
    file.try_lock().is_ok()
}

#[cfg(windows)]
#[allow(unsafe_code)] // The Windows API, which has no safe way in. Each call says why it is sound.
mod windows {
    use std::ffi::c_void;

    #[repr(C)]
    struct MemoryStatus {
        length: u32,
        load: u32,
        total_physical: u64,
        available_physical: u64,
        total_page_file: u64,
        available_page_file: u64,
        total_virtual: u64,
        available_virtual: u64,
        available_extended_virtual: u64,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> i32;
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }

    type GetCapability = unsafe extern "system" fn(u32, *mut c_void, u32, *mut u32) -> i32;

    #[repr(C)]
    struct VersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        service_pack: [u16; 128],
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut VersionInfo) -> i32;
    }

    pub(super) fn release() -> String {
        let mut info = VersionInfo {
            size: std::mem::size_of::<VersionInfo>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform: 0,
            service_pack: [0; 128],
        };
        // Safe: `info` is a correctly sized, initialised RTL_OSVERSIONINFOW
        // that outlives the call. RtlGetVersion, unlike GetVersionEx, does
        // not lie to a program without a manifest.
        if unsafe { RtlGetVersion(&mut info) } != 0 {
            return String::new();
        }
        match (info.major, info.minor) {
            (10, _) if info.build >= 22000 => "11".into(),
            (10, _) => "10".into(),
            (6, 3) => "8.1".into(),
            (6, 2) => "8".into(),
            (6, 1) => "7".into(),
            (major, minor) => format!("{major}.{minor}"),
        }
    }

    pub(super) fn memory_mb() -> (Option<u64>, Option<u64>) {
        let mut status = MemoryStatus {
            length: std::mem::size_of::<MemoryStatus>() as u32,
            load: 0,
            total_physical: 0,
            available_physical: 0,
            total_page_file: 0,
            available_page_file: 0,
            total_virtual: 0,
            available_virtual: 0,
            available_extended_virtual: 0,
        };
        // Safe: `status` is a correctly sized, initialised MEMORYSTATUSEX that
        // outlives the call, which is all GlobalMemoryStatusEx asks.
        let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
        if ok == 0 {
            return (None, None);
        }
        (
            Some(status.available_physical >> 20),
            Some(status.total_physical >> 20),
        )
    }

    pub(super) fn whpx_available() -> bool {
        let name: Vec<u16> = "WinHvPlatform.dll\0".encode_utf16().collect();
        // Safe: both names are NUL-terminated and live across the calls; a
        // null module or procedure is checked before anything is called, and
        // WHvGetCapability's signature is the documented one, handed a
        // 64-byte buffer and its true size.
        unsafe {
            let module = LoadLibraryW(name.as_ptr());
            if module.is_null() {
                return false;
            }
            let procedure = GetProcAddress(module, c"WHvGetCapability".as_ptr().cast());
            if procedure.is_null() {
                return false;
            }
            let get: GetCapability = std::mem::transmute(procedure);
            let mut answer = [0u8; 64];
            let mut written = 0u32;
            let result = get(0, answer.as_mut_ptr().cast(), 64, &mut written);
            result == 0 && u32::from_le_bytes([answer[0], answer[1], answer[2], answer[3]]) != 0
        }
    }
}
