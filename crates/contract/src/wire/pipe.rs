//! Windows named pipes, our stand-in for the UNIX socket. [`connect`] for
//! clients, [`Listener`] for the daemon.
//!
//! Overlapped I/O on purpose: a blocking pipe handle allows one operation at
//! a time, so a pending read would block every answer and event. Overlapped
//! waits also give us read timeouts (pipes have no `SO_RCVTIMEO`).
//!
//! Security, both ways: the daemon's pipe has a protected DACL for this
//! user's SID only (not even Administrators/SYSTEM) and rejects remote
//! clients. Pipe names are first-come, so someone could squat ours:
//! [`connect`] checks the server process is ours and hangs up otherwise, and
//! the daemon checks every client (like `SO_PEERCRED`). We connect at
//! identification level, so a squatter can't impersonate us meanwhile.

#![allow(unsafe_code)] // The Windows API has no safe way in. Each call says why it is sound.

use std::ffi::{OsStr, c_void};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

type Handle = *mut c_void;

const INVALID: Handle = -1isize as Handle;

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;
const PIPE_UNLIMITED_INSTANCES: u32 = 255;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_USER: u32 = 1;
const SDDL_REVISION_1: u32 = 1;
const WAIT_TIMEOUT: u32 = 258;

const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_BROKEN_PIPE: i32 = 109;
const ERROR_PIPE_BUSY: i32 = 231;
const ERROR_NO_DATA: i32 = 232;
const ERROR_PIPE_NOT_CONNECTED: i32 = 233;
const ERROR_PIPE_CONNECTED: i32 = 535;
const ERROR_OPERATION_ABORTED: i32 = 995;
const ERROR_IO_PENDING: i32 = 997;

#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: Handle,
}

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut c_void,
    inherit: i32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut SecurityAttributes,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        instances: u32,
        out_size: u32,
        in_size: u32,
        timeout: u32,
        security: *mut SecurityAttributes,
    ) -> Handle;
    fn ConnectNamedPipe(pipe: Handle, overlapped: *mut Overlapped) -> i32;
    fn DisconnectNamedPipe(pipe: Handle) -> i32;
    fn WaitNamedPipeW(name: *const u16, timeout: u32) -> i32;
    fn GetNamedPipeServerProcessId(pipe: Handle, pid: *mut u32) -> i32;
    fn GetNamedPipeClientProcessId(pipe: Handle, pid: *mut u32) -> i32;
    fn ReadFile(
        file: Handle,
        buffer: *mut u8,
        length: u32,
        read: *mut u32,
        overlapped: *mut Overlapped,
    ) -> i32;
    fn WriteFile(
        file: Handle,
        buffer: *const u8,
        length: u32,
        written: *mut u32,
        overlapped: *mut Overlapped,
    ) -> i32;
    fn GetOverlappedResult(
        file: Handle,
        overlapped: *mut Overlapped,
        transferred: *mut u32,
        wait: i32,
    ) -> i32;
    fn CancelIoEx(file: Handle, overlapped: *mut Overlapped) -> i32;
    fn CreateEventW(security: *mut c_void, manual: i32, initial: i32, name: *const u16) -> Handle;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn CloseHandle(handle: Handle) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
    fn GetCurrentProcess() -> Handle;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(
        token: Handle,
        class: u32,
        information: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
    fn EqualSid(a: *mut c_void, b: *mut c_void) -> i32;
    fn ConvertSidToStringSidW(sid: *mut c_void, text: *mut *mut u16) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

/// A handle, closed once when the last owner lets go.
struct Owned(Handle);

// Safe: a Windows handle is a process-wide value, usable from any thread;
// what is done with it is synchronised by the kernel, and overlapped I/O is
// made for exactly this.
unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}

impl Owned {
    /// Hand the handle on, without closing it.
    fn into_raw(self) -> Handle {
        let handle = self.0;
        std::mem::forget(self);
        handle
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID {
            // Safe: this is the only owner of a handle this module opened.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// The `TOKEN_USER` of a process, in a buffer that keeps its SID alive.
struct UserOf {
    buffer: Vec<u64>,
}

impl UserOf {
    /// Safe to call with any process handle: every failure is `None`.
    fn process(process: Handle) -> Option<Self> {
        let mut token = null_mut();
        // Safe: `token` is written by the call and closed below.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return None;
        }
        let token = Owned(token);
        let mut needed = 0u32;
        // Safe: asking for the size with no buffer is the documented way.
        unsafe {
            GetTokenInformation(token.0, TOKEN_USER, null_mut(), 0, &mut needed);
        }
        if needed == 0 {
            return None;
        }
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        // Safe: the buffer is at least `needed` bytes, 8-aligned, and lives
        // in `UserOf` for as long as its SID pointer is used.
        let ok = unsafe {
            GetTokenInformation(
                token.0,
                TOKEN_USER,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        };
        (ok != 0).then_some(Self { buffer })
    }

    fn ours() -> Option<Self> {
        // Safe: GetCurrentProcess returns a pseudo-handle that needs no closing.
        Self::process(unsafe { GetCurrentProcess() })
    }

    /// The SID: the first field of `TOKEN_USER` is `SID_AND_ATTRIBUTES`,
    /// whose first field is the SID's pointer, into this same buffer.
    fn sid(&self) -> *mut c_void {
        self.buffer[0] as usize as *mut c_void
    }

    fn same_as(&self, other: &Self) -> bool {
        // Safe: both SIDs point into buffers alive for this call.
        unsafe { EqualSid(self.sid(), other.sid()) != 0 }
    }

    fn text(&self) -> Option<String> {
        let mut text = null_mut();
        // Safe: the SID is valid; the string the call allocates is read up
        // to its NUL and freed with LocalFree, as documented.
        unsafe {
            if ConvertSidToStringSidW(self.sid(), &mut text) == 0 {
                return None;
            }
            let mut length = 0;
            while *text.add(length) != 0 {
                length += 1;
            }
            let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
            LocalFree(text.cast());
            Some(value)
        }
    }
}

/// Does `pid` run as this user? Can't ask (usually another user's) = no.
pub fn process_is_ours(pid: u32) -> bool {
    // Safe: OpenProcess returns null on failure, checked; the handle is
    // closed by `Owned`.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return false;
    }
    let process = Owned(process);
    match (UserOf::process(process.0), UserOf::ours()) {
        (Some(theirs), Some(ours)) => theirs.same_as(&ours),
        _ => false,
    }
}

/// `D:P(A;;GA;;;<this user's SID>)`: a protected DACL, one entry, this user.
pub fn only_this_user() -> io::Result<String> {
    let sid = UserOf::ours()
        .and_then(|user| user.text())
        .ok_or_else(|| io::Error::other("this process's user cannot be read"))?;
    Ok(format!("D:P(A;;GA;;;{sid})"))
}

struct Inner {
    handle: Owned,
    server: bool,
    closed: AtomicBool,
    timeout: Mutex<Option<Duration>>,
}

/// One connected pipe. Clones are the same connection, and each may read or
/// write while another does the other.
#[derive(Clone)]
pub struct PipeStream {
    inner: Arc<Inner>,
}

fn failed(code: i32) -> io::Error {
    match code {
        ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED => {
            io::Error::new(io::ErrorKind::BrokenPipe, "the other end of the pipe went")
        }
        ERROR_OPERATION_ABORTED => {
            io::Error::new(io::ErrorKind::ConnectionAborted, "the pipe was hung up on")
        }
        _ => io::Error::from_raw_os_error(code),
    }
}

fn last() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

impl PipeStream {
    fn new(handle: Handle, server: bool) -> Self {
        Self {
            inner: Arc::new(Inner {
                handle: Owned(handle),
                server,
                closed: AtomicBool::new(false),
                timeout: Mutex::new(None),
            }),
        }
    }

    fn handle(&self) -> Handle {
        self.inner.handle.0
    }

    /// The same connection, as a second handle-holder.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(self.clone())
    }

    /// How long a read waits, as `UnixStream::set_read_timeout`.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        *self
            .inner
            .timeout
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = timeout;
        Ok(())
    }

    /// Hang up: every read and write, now and later, fails, and the other
    /// end finds the pipe gone.
    pub fn shutdown(&self, _how: Shutdown) -> io::Result<()> {
        self.inner.closed.store(true, Ordering::SeqCst);
        // Safe: cancelling this handle's own I/O, from any thread, is what
        // CancelIoEx is for; disconnecting is the server end's to do.
        unsafe {
            CancelIoEx(self.handle(), null_mut());
            if self.inner.server {
                DisconnectNamedPipe(self.handle());
            }
        }
        Ok(())
    }

    /// Whether the process at the other end is this user's.
    pub fn peer_is_ours(&self) -> bool {
        let mut pid = 0u32;
        // Safe: `pid` is written by the call; the handle is this pipe's.
        let asked = unsafe {
            if self.inner.server {
                GetNamedPipeClientProcessId(self.handle(), &mut pid)
            } else {
                GetNamedPipeServerProcessId(self.handle(), &mut pid)
            }
        };
        asked != 0 && process_is_ours(pid)
    }

    /// One overlapped read or write, waited for to its end.
    fn transfer(&self, reading: bool, buffer: *mut u8, length: usize) -> io::Result<usize> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(failed(ERROR_OPERATION_ABORTED));
        }
        let length = u32::try_from(length).unwrap_or(u32::MAX);
        let timeout = if reading {
            *self
                .inner
                .timeout
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
        } else {
            None
        };
        // Safe: a manual-reset event, unnamed, closed by `Owned`.
        let event = Owned(unsafe { CreateEventW(null_mut(), 1, 0, null()) });
        if event.0.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut overlapped = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event: event.0,
        };
        let mut done = 0u32;
        // Safe: the buffer and `overlapped` outlive the operation, because
        // this function does not return until it has completed or been
        // cancelled and reaped, whichever way it goes.
        unsafe {
            let started = if reading {
                ReadFile(self.handle(), buffer, length, null_mut(), &mut overlapped)
            } else {
                WriteFile(self.handle(), buffer, length, null_mut(), &mut overlapped)
            };
            if started == 0 {
                let code = last();
                if code != ERROR_IO_PENDING {
                    return Err(failed(code));
                }
                if let Some(timeout) = timeout {
                    let milliseconds = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
                    if WaitForSingleObject(event.0, milliseconds) == WAIT_TIMEOUT {
                        CancelIoEx(self.handle(), &mut overlapped);
                        GetOverlappedResult(self.handle(), &mut overlapped, &mut done, 1);
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "nothing arrived on the pipe in time",
                        ));
                    }
                }
            }
            if GetOverlappedResult(self.handle(), &mut overlapped, &mut done, 1) == 0 {
                return Err(failed(last()));
            }
        }
        Ok(done as usize)
    }
}

impl Read for PipeStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.transfer(true, buffer.as_mut_ptr(), buffer.len()) {
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(0),
            other => other,
        }
    }
}

impl Write for PipeStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        // WriteFile does not write through its pointer.
        self.transfer(false, buffer.as_ptr().cast_mut(), buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Does the pipe exist? Checked without opening it, since opening = connecting.
pub fn exists(name: &Path) -> bool {
    const ERROR_SEM_TIMEOUT: i32 = 121;
    let wide_name = wide(name.as_os_str());
    // Safe: the name is NUL-terminated and outlives the call.
    if unsafe { WaitNamedPipeW(wide_name.as_ptr(), 1) } != 0 {
        return true;
    }
    matches!(last(), ERROR_SEM_TIMEOUT | ERROR_PIPE_BUSY)
}

/// Open the pipe called `name`, waiting up to `within` while every instance
/// is busy, and make sure it is this user's daemon at the other end.
pub fn connect(name: &Path, within: Duration) -> io::Result<PipeStream> {
    let wide_name = wide(name.as_os_str());
    let deadline = Instant::now() + within;
    loop {
        // Safe: the name is NUL-terminated and outlives the call; the handle
        // returned is owned by the stream, or checked for failure.
        let handle = unsafe {
            CreateFileW(
                wide_name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            )
        };
        if handle != INVALID {
            let stream = PipeStream::new(handle, false);
            if !stream.peer_is_ours() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{} is held by a process of another user", name.display()),
                ));
            }
            return Ok(stream);
        }
        let code = last();
        if code == ERROR_PIPE_BUSY && Instant::now() < deadline {
            // Safe: the name is NUL-terminated.
            unsafe {
                WaitNamedPipeW(wide_name.as_ptr(), 500);
            }
            continue;
        }
        return Err(io::Error::from_raw_os_error(code));
    }
}

/// The server end: instances made one ahead, so the name never disappears
/// while the daemon lives.
pub struct Listener {
    name: Vec<u16>,
    descriptor: *mut c_void,
    next: Mutex<Option<Owned>>,
}

// Safe: the descriptor is only read, by CreateNamedPipeW, and freed once.
unsafe impl Send for Listener {}
unsafe impl Sync for Listener {}

impl Drop for Listener {
    fn drop(&mut self) {
        // Safe: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW
        // for this listener alone.
        unsafe {
            LocalFree(self.descriptor);
        }
    }
}

impl Listener {
    /// Create the pipe, or `AddrInUse` if the name exists (another daemon or
    /// a squatter). First-instance-only is what stops us sharing a squatted pipe.
    pub fn bind(name: &Path) -> io::Result<Self> {
        let sddl = wide(OsStr::new(&only_this_user()?));
        let mut descriptor = null_mut();
        // Safe: the SDDL string is NUL-terminated; the descriptor is freed in
        // Drop.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let listener = Self {
            name: wide(name.as_os_str()),
            descriptor,
            next: Mutex::new(None),
        };
        let first = listener.instance(true).map_err(|error| {
            if error.raw_os_error() == Some(ERROR_ACCESS_DENIED) {
                io::Error::new(io::ErrorKind::AddrInUse, "the pipe is already there")
            } else {
                error
            }
        })?;
        *listener.next.lock().unwrap_or_else(PoisonError::into_inner) = Some(first);
        Ok(listener)
    }

    fn instance(&self, first: bool) -> io::Result<Owned> {
        let mut security = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            descriptor: self.descriptor,
            inherit: 0,
        };
        let open = PIPE_ACCESS_DUPLEX
            | FILE_FLAG_OVERLAPPED
            | if first {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                0
            };
        // Safe: the name and the attributes outlive the call; the handle is
        // checked and owned.
        let handle = unsafe {
            CreateNamedPipeW(
                self.name.as_ptr(),
                open,
                PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                1 << 16,
                1 << 16,
                0,
                &mut security,
            )
        };
        if handle == INVALID {
            return Err(io::Error::last_os_error());
        }
        Ok(Owned(handle))
    }

    /// Wait for the next connection.
    pub fn accept(&self) -> io::Result<PipeStream> {
        let pipe = match self
            .next
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            Some(pipe) => pipe,
            None => self.instance(false)?,
        };
        // Safe: as in `transfer` -- the OVERLAPPED and its event outlive the
        // wait, which runs to completion.
        let connected = unsafe {
            let event = Owned(CreateEventW(null_mut(), 1, 0, null()));
            if event.0.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut overlapped = Overlapped {
                internal: 0,
                internal_high: 0,
                offset: 0,
                offset_high: 0,
                event: event.0,
            };
            if ConnectNamedPipe(pipe.0, &mut overlapped) == 0 {
                match last() {
                    ERROR_PIPE_CONNECTED => Ok(()),
                    ERROR_IO_PENDING => {
                        let mut done = 0u32;
                        if GetOverlappedResult(pipe.0, &mut overlapped, &mut done, 1) != 0 {
                            Ok(())
                        } else {
                            Err(last())
                        }
                    }
                    code => Err(code),
                }
            } else {
                Ok(())
            }
        };
        // Make the next instance before handing this one out, so the name never disappears.
        if let Ok(next) = self.instance(false) {
            *self.next.lock().unwrap_or_else(PoisonError::into_inner) = Some(next);
        }
        connected.map_err(io::Error::from_raw_os_error)?;
        Ok(PipeStream::new(pipe.into_raw(), true))
    }
}
