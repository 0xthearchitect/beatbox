//! The socket and who may talk to it: `0700` folder, `0600` socket, and the
//! peer uid checked via `SO_PEERCRED` as a second lock in case the folder's wrong.
//!
//! `beatboxd.sock.lock` is held for the daemon's life. Without it, two
//! daemons starting together would each delete the other's "stale" socket.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// A UNIX socket's path has to fit in `sun_path`, 108 bytes with its NUL.
pub const PATH_LIMIT: usize = 107;

#[allow(unsafe_code)] // getuid(2): no arguments, cannot fail.
fn our_uid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    // Safe: getuid takes nothing and always succeeds.
    unsafe { getuid() }
}

/// Create `socket`'s folder as `0700`, or check an existing one is ours and closed.
pub fn prepare(socket: &Path) -> io::Result<()> {
    if socket.as_os_str().len() > PATH_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is {} bytes; a UNIX socket's path can have {PATH_LIMIT}",
                socket.display(),
                socket.as_os_str().len()
            ),
        ));
    }
    let folder = socket.parent().unwrap_or(Path::new("."));
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)?;
    let meta = fs::metadata(folder)?;
    if meta.uid() != our_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} belongs to somebody else", folder.display()),
        ));
    }
    // Refuse, don't chmod: BEATBOX_SOCKET could point into someone's home.
    if meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} can be read by others (mode {:o}); the socket's folder has to be 0700",
                folder.display(),
                meta.mode() & 0o777
            ),
        ));
    }
    Ok(())
}

fn lock_path(socket: &Path) -> PathBuf {
    let mut name = socket.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Be the one daemon on this socket, or `None` if another already is.
pub fn claim(socket: &Path) -> io::Result<Option<File>> {
    let lock = OpenOptions::new()
        .create(true)
        .append(true)
        .open(lock_path(socket))?;
    Ok(lock.try_lock().is_ok().then_some(lock))
}

/// Listen on `socket`, removing a dead daemon's leftover. Only call with
/// [`claim`]'s lock held, so nothing alive is there.
pub fn listen(socket: &Path) -> io::Result<UnixListener> {
    match fs::remove_file(socket) {
        Ok(()) => log::info!("removed the socket a daemon before this one left"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Whether the process at the other end is this user's.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)] // getsockopt(2) with SO_PEERCRED: std's peer_cred is unstable.
pub fn same_user(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;

    #[repr(C)]
    struct Credentials {
        pid: i32,
        uid: u32,
        gid: u32,
    }
    unsafe extern "C" {
        fn getsockopt(
            socket: i32,
            level: i32,
            name: i32,
            value: *mut std::ffi::c_void,
            length: *mut u32,
        ) -> i32;
    }
    // Same on x86_64 and aarch64, the two we ship.
    const SOL_SOCKET: i32 = 1;
    const SO_PEERCRED: i32 = 17;
    let mut credentials = Credentials {
        pid: 0,
        uid: u32::MAX,
        gid: u32::MAX,
    };
    let size = std::mem::size_of::<Credentials>() as u32;
    let mut length = size;
    // Safe: the descriptor is this stream's and open for the call; the
    // buffer is a correctly sized `struct ucred` that outlives it, and its
    // size is passed beside it.
    let read = unsafe {
        getsockopt(
            stream.as_raw_fd(),
            SOL_SOCKET,
            SO_PEERCRED,
            (&raw mut credentials).cast(),
            &mut length,
        )
    };
    read == 0 && length == size && credentials.uid == our_uid()
}

/// Elsewhere the folder is the only lock.
#[cfg(not(target_os = "linux"))]
pub fn same_user(_stream: &UnixStream) -> bool {
    true
}
