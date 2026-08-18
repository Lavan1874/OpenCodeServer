//! Small, audited wrappers around public macOS system APIs that do not have
//! safe equivalents in Rust's standard library.
//!
//! All Rust `unsafe` for POSIX and process integration in OpenCodeServer is
//! intentionally confined to this file. The Security-framework FFI boundary
//! is isolated separately in `keychain.rs` (see ADR 0016).
//!
//! The libproc premises in `process_snapshot` are the only contract in this
//! file without Apple behavioral documentation; they must be re-verified
//! whenever the minimum macOS version or the SDK changes.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::mem::{self, MaybeUninit};
use std::os::fd::{AsRawFd, RawFd};
use std::os::raw::{c_char, c_int, c_void};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlSignal {
    Terminate,
    Interrupt,
    Hangup,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub parent_pid: u32,
    pub process_group_id: u32,
    pub effective_uid: u32,
    pub start_seconds: u64,
    pub start_microseconds: u64,
    /// Resolved path of the executable file, or `None` when the process is
    /// alive but its executable file no longer exists on disk — the classic
    /// aftermath of a package manager (Homebrew) replacing the binary under
    /// a running process. proc_pidpath then fails with ENOENT while every
    /// proc_pidinfo field remains valid. Apple documents no errno semantics
    /// for proc_pidpath; this mapping is empirically verified (see the
    /// process-supervision integration tests).
    pub executable: Option<PathBuf>,
}

/// A child exit observed without consuming the child's wait status.
///
/// `waitid(WNOWAIT)` is the ownership anchor used by process supervision:
/// the direct `Child` remains waitable, so its PID cannot be recycled while
/// an authorized process group is being converged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChildExitObservation {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogLevel {
    Notice = 0,
    Info = 1,
    Error = 2,
    Fault = 3,
}

unsafe extern "C" {
    fn ocs_unified_log(level: u8, message: *const c_char);
}

pub fn log(level: LogLevel, message: &str) {
    let sanitized = message.replace('\0', "\u{fffd}");
    if let Ok(message) = CString::new(sanitized) {
        // SAFETY: `message` is a live, NUL-terminated C string for the duration
        // of the call. The bridge only passes it to os_log with a fixed format.
        unsafe { ocs_unified_log(level as u8, message.as_ptr()) };
    }
}

pub fn effective_uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions and returns a value.
    unsafe { libc::geteuid() }
}

pub fn peer_effective_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: Both output pointers are valid and writable, and the file
    // descriptor is borrowed from a live connected Unix-domain stream. man 3
    // getpeereid requires `s` to be "a UNIX-domain socket (unix(4)) of type
    // SOCK_STREAM on which either connect(2) or listen(2) have been called";
    // Rust's UnixStream is always a SOCK_STREAM socket, so the type
    // precondition is satisfied by construction.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if result == 0 {
        Ok(uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn block_control_signals() -> io::Result<()> {
    let set = control_signal_set()?;
    // SAFETY: `set` is initialized by sigemptyset/sigaddset. A null old-set
    // pointer is explicitly allowed by pthread_sigmask.
    let result = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result))
    }
}

pub fn wait_for_control_signal() -> io::Result<ControlSignal> {
    let set = control_signal_set()?;
    let mut signal = 0;
    // SAFETY: `set` and the signal output pointer are initialized and valid.
    // The signals were blocked before any worker threads were created. They
    // must also not be ignored: man 2 sigwait requires the selected signals
    // to be "blocked, but not ignored" — ignored signals are dropped by the
    // system before delivery, so waiting on one would hang. The production
    // process never installs SIG_IGN, so this precondition holds.
    let result = unsafe { libc::sigwait(&set, &mut signal) };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    match signal {
        libc::SIGTERM => Ok(ControlSignal::Terminate),
        libc::SIGINT => Ok(ControlSignal::Interrupt),
        libc::SIGHUP => Ok(ControlSignal::Hangup),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected signal {other}"),
        )),
    }
}

pub fn configure_child_signal_mask(command: &mut Command) {
    // SAFETY: `pre_exec` is required because a child inherits
    // OpenCodeServerAgent's
    // blocked signal mask across `exec`. The closure captures no Rust state and
    // calls only signal-set functions plus the async-signal-safe `sigprocmask`.
    // Without it, managed OpenCode cannot receive graceful-stop SIGTERM.
    unsafe {
        command.pre_exec(|| {
            let set = control_signal_set()?;
            let result = libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
            if result == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        });
    }
}

fn control_signal_set() -> io::Result<libc::sigset_t> {
    // SAFETY: The value is immediately initialized by sigemptyset before use.
    let mut set = unsafe { mem::zeroed::<libc::sigset_t>() };
    // SAFETY: `set` is a valid pointer and all signal numbers are valid.
    if unsafe { libc::sigemptyset(&mut set) } != 0
        || unsafe { libc::sigaddset(&mut set, libc::SIGTERM) } != 0
        || unsafe { libc::sigaddset(&mut set, libc::SIGINT) } != 0
        || unsafe { libc::sigaddset(&mut set, libc::SIGHUP) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(set)
}

pub fn process_snapshot(pid: u32) -> io::Result<ProcessSnapshot> {
    let pid = c_int::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "PID is out of range"))?;
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let info_size = c_int::try_from(mem::size_of::<libc::proc_bsdinfo>())
        .expect("proc_bsdinfo size fits in c_int");

    // SAFETY: `info` points to a correctly sized writable proc_bsdinfo buffer.
    // proc_pidinfo is a public macOS API declared in <libproc.h>. libproc has
    // no man page and its declarations carry no header comments, so the two
    // behavioral premises here — `bytes == size` implies a fully initialized
    // buffer, and proc_pidpath failing with ENOENT implies a live process
    // whose executable file was deleted — are empirical premises of this
    // project, backed by the process-supervision integration tests rather
    // than Apple behavioral documentation. errno is cleared immediately
    // before the call so the failure branch below can never classify a stale
    // errno value left behind by an unrelated earlier call.
    let bytes = unsafe {
        *libc::__error() = 0;
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            info_size,
        )
    };
    if bytes != info_size {
        return Err(libproc_failure());
    }
    // SAFETY: A full proc_bsdinfo was written because `bytes == info_size`.
    let info = unsafe { info.assume_init() };

    let mut path = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `path` is a writable buffer with the exact size supplied. errno
    // is cleared immediately before the call for the same stale-value reason
    // as above.
    let path_length = unsafe {
        *libc::__error() = 0;
        libc::proc_pidpath(
            pid,
            path.as_mut_ptr().cast(),
            u32::try_from(path.len()).expect("path buffer size fits in u32"),
        )
    };
    let executable = if path_length <= 0 {
        let error = libproc_failure();
        if error.raw_os_error() == Some(libc::ENOENT) {
            // The process is alive (proc_pidinfo succeeded above) but its
            // executable file was deleted underneath it; only the path is
            // unknowable, never the kernel identity.
            None
        } else {
            return Err(error);
        }
    } else {
        let nul = path
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(path.len());
        path.truncate(nul);
        Some(PathBuf::from(std::ffi::OsString::from(
            String::from_utf8_lossy(&path).into_owned(),
        )))
    };

    Ok(ProcessSnapshot {
        pid: info.pbi_pid,
        parent_pid: info.pbi_ppid,
        process_group_id: info.pbi_pgid,
        effective_uid: info.pbi_uid,
        start_seconds: info.pbi_start_tvsec,
        start_microseconds: info.pbi_start_tvusec,
        executable,
    })
}

/// Returns the currently observable PIDs in one process group.
///
/// `proc_listpgrppids` is a public libproc API (macOS 10.7+) and is used only
/// as a bounded group-membership observation. Callers must resolve and
/// validate the returned PIDs before treating the group as authorized.
pub fn process_group_member_ids(process_group_id: u32) -> io::Result<Vec<u32>> {
    let process_group_id = c_int::try_from(process_group_id).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "process group is out of range")
    })?;
    if process_group_id <= 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to inspect a broad process group",
        ));
    }

    list_process_group_ids(process_group_id)
}

const INITIAL_PROCESS_GROUP_CAPACITY: usize = 16;
const MAX_PROCESS_GROUP_CAPACITY: usize = 4096;

fn list_process_group_ids(process_group_id: c_int) -> io::Result<Vec<u32>> {
    // A NULL/zero-size probe is not a group count on macOS: it can report a
    // system-wide capacity. Start with a small bounded buffer instead, then
    // treat the nonzero-buffer return as the number of PID entries and grow
    // only when the result fills the current buffer. The hard cap keeps an
    // unexpected kernel result from becoming an unbounded allocation.
    let pid_size = mem::size_of::<libc::pid_t>();
    let mut pids = vec![0 as libc::pid_t; INITIAL_PROCESS_GROUP_CAPACITY];
    loop {
        let buffer_bytes = pids
            .len()
            .checked_mul(pid_size)
            .and_then(|size| c_int::try_from(size).ok())
            .ok_or_else(|| io::Error::other("process-group buffer is too large"))?;
        // SAFETY: `pids` is a writable array of pid_t values whose byte
        // length is passed exactly; the group id was range-checked by the
        // public wrapper and libproc writes at most `buffer_bytes` bytes.
        let capacity = unsafe {
            libc::proc_listpgrppids(process_group_id, pids.as_mut_ptr().cast(), buffer_bytes)
        };
        if capacity < 0 {
            return Err(io::Error::last_os_error());
        }
        let count = capacity as usize;
        // Retry when the buffer is exactly full as well: a full return is
        // ambiguous between an exact fit and truncation on APIs that report
        // copied entries. At the hard cap, fail closed rather than signal a
        // group whose complete membership was not observed.
        if count < pids.len() {
            let members = pids[..count]
                .iter()
                .map(|pid| {
                    u32::try_from(*pid).map_err(|_| {
                        io::Error::other("process-group listing returned an invalid PID")
                    })
                })
                .collect::<io::Result<Vec<_>>>()?;
            return Ok(members);
        }
        let next_capacity = next_process_group_capacity(pids.len(), count)?;
        pids.resize(next_capacity, 0);
    }
}

fn next_process_group_capacity(current: usize, reported: usize) -> io::Result<usize> {
    if reported > MAX_PROCESS_GROUP_CAPACITY || current >= MAX_PROCESS_GROUP_CAPACITY {
        return Err(io::Error::other(
            "process group membership exceeds the safe observation bound",
        ));
    }
    let doubled = current
        .checked_mul(2)
        .unwrap_or(MAX_PROCESS_GROUP_CAPACITY)
        .min(MAX_PROCESS_GROUP_CAPACITY);
    let requested = if reported == current {
        reported.saturating_add(1)
    } else {
        reported
    };
    let next = doubled.max(requested);
    if next > current && next <= MAX_PROCESS_GROUP_CAPACITY {
        Ok(next)
    } else {
        Err(io::Error::other(
            "process group membership could not be observed within the safe bound",
        ))
    }
}

/// Observes an owned child exit without reaping it.
///
/// This is the small platform boundary for the POSIX/XSI `waitid` primitive.
/// The macOS 26 SDK publicly declares `waitid` and `WNOWAIT`; the latter is
/// essential because `Child::try_wait` consumes the leader's wait status.
pub fn peek_child_exit(pid: u32) -> io::Result<Option<ChildExitObservation>> {
    let pid = libc::id_t::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "PID is out of range"))?;
    let mut info = MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: `info` points to writable storage for the public waitid output
    // type. P_PID names exactly the owned child and WNOHANG makes this call
    // non-blocking; WNOWAIT leaves an observed exit waitable for Child::wait.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: waitid completed successfully and initialized the siginfo_t
    // output. With WNOHANG, si_pid == 0 means no terminal state is ready.
    let info = unsafe { info.assume_init() };
    if info.si_pid == 0 {
        return Ok(None);
    }
    if info.si_pid != pid as libc::pid_t {
        return Err(io::Error::other(
            "waitid returned a different child identity",
        ));
    }
    match info.si_code {
        libc::CLD_EXITED => Ok(Some(ChildExitObservation {
            code: Some(info.si_status),
            signal: None,
        })),
        libc::CLD_KILLED | libc::CLD_DUMPED => Ok(Some(ChildExitObservation {
            code: None,
            signal: Some(info.si_status),
        })),
        code => Err(io::Error::other(format!(
            "waitid returned unexpected child state {code}"
        ))),
    }
}

/// Reads errno for a libproc call that just failed; callers clear errno
/// immediately before the call. A failure that still reports errno 0 carries
/// no classifiable cause, so it is mapped to a generic error: identity
/// classification stays fail-closed (an inspection error keeps the record
/// unverified) instead of being mistaken for a specific state like ESRCH.
fn libproc_failure() -> io::Error {
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(0) {
        io::Error::other("libproc call failed without reporting an errno")
    } else {
        error
    }
}

pub fn send_process_group_signal(process_group_id: u32, signal: c_int) -> io::Result<()> {
    let process_group_id = c_int::try_from(process_group_id).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "process group is out of range")
    })?;
    if process_group_id <= 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to signal a broad process group",
        ));
    }
    // SAFETY: A negative PID targets precisely the validated process group.
    // Group-level permission follows man 2 kill's EPERM clause: "When
    // signaling a process group, this error is returned if any members of
    // the group could not be signaled." The page does not state which
    // members, if any, received the signal before the error is returned,
    // so this code makes no partial-delivery claim; it only fails closed
    // by returning the error to the caller.
    let result = unsafe { libc::kill(-process_group_id, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Sends `signal` to one specific process. Production code signals whole
/// process groups only; this single-process signal exists for the test
/// fixture and integration-test teardown. PID 1 and anything below it are
/// refused so a broad or system-wide signal can never be constructed here.
#[cfg(any(test, feature = "test-fixture"))]
pub fn signal_process(pid: u32, signal: c_int) -> io::Result<()> {
    let pid = c_int::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "PID is out of range"))?;
    if pid <= 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to signal a broad or system process",
        ));
    }
    // SAFETY: A positive, validated PID targets exactly one process.
    let result = unsafe { libc::kill(pid, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Reports whether `pid` currently exists, without delivering a signal
/// (signal 0 performs error checking only). A process owned by another user
/// still counts as existing because `kill` then fails with `EPERM`, not
/// `ESRCH`. PID 0 would address the caller's own process group and is
/// rejected as out of scope for this query. Only the test fixture and the
/// integration tests query single processes this way; production identity
/// tracking goes through `process_snapshot`.
#[cfg(any(test, feature = "test-fixture"))]
pub fn process_exists(pid: u32) -> bool {
    let Ok(pid) = c_int::try_from(pid) else {
        return false;
    };
    if pid < 1 {
        return false;
    }
    // SAFETY: Signal 0 delivers nothing; the PID is validated above.
    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// The caller's own process group. Used to prove a candidate signal target
/// is not OpenCodeServerAgent's own group before a group signal is sent.
pub fn own_process_group() -> u32 {
    // SAFETY: `getpgrp` has no preconditions and returns a value.
    unsafe { libc::getpgrp() as u32 }
}

/// The parent process's PID. `getppid` cannot fail. Used only by the
/// test fixture.
#[cfg(any(test, feature = "test-fixture"))]
pub fn parent_process_id() -> u32 {
    // SAFETY: `getppid` has no preconditions and returns a value.
    unsafe { libc::getppid() as u32 }
}

/// The process group of the parent process. Only meaningful for the test
/// fixture, which demonstrates group-confirmation failure by leaving the
/// group its supervisor constructed.
#[cfg(any(test, feature = "test-fixture"))]
pub fn parent_process_group() -> io::Result<u32> {
    // SAFETY: `getpgid` with a valid PID fails only when the process is gone.
    let result = unsafe { libc::getpgid(libc::getppid()) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result as u32)
    }
}

/// Moves the calling process into `process_group_id`. Only the test fixture
/// uses this, to model a child that abandons the process group its
/// supervisor constructed at spawn.
#[cfg(any(test, feature = "test-fixture"))]
pub fn join_process_group(process_group_id: u32) -> io::Result<()> {
    let group = c_int::try_from(process_group_id).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "process group is out of range")
    })?;
    // SAFETY: `setpgid(0, group)` affects only the calling process (man 2
    // setpgid: "If pid is zero, then the call applies to the current
    // process"). Moves into a group outside the calling process's session
    // are refused: man 2 setpgid reports EPERM when the pgid "does not match
    // the process ID of the process indicated by the pid argument and there
    // is no process with a process group ID that matches the value of the
    // pgid argument in the same session as the calling process".
    let result = unsafe { libc::setpgid(0, group) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Sets `signal` to be ignored by the calling process. Only the test fixture
/// uses this (its port-holder must outlive a process-group SIGTERM). Kept at
/// the platform boundary so no other module ever changes a process-global
/// signal disposition; OpenCodeServerAgent itself consumes control signals
/// synchronously through `sigwait` and never ignores them.
#[cfg(any(test, feature = "test-fixture"))]
pub fn ignore_signal(signal: c_int) -> io::Result<()> {
    // SAFETY: A zeroed `sigaction` with the constant `SIG_IGN` disposition is
    // valid for any valid signal number; man 2 sigaction reports an invalid
    // signal number as `EINVAL`.
    let mut action = unsafe { mem::zeroed::<libc::sigaction>() };
    action.sa_sigaction = libc::SIG_IGN;
    let result = unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: F_GETFL and F_SETFL are safe for a valid open file descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Closes a file descriptor. Used only by the test fixture to model a
/// child that closes its stdout while continuing to run; kept at the
/// platform boundary so no other module performs raw descriptor surgery.
#[cfg(any(test, feature = "test-fixture"))]
pub fn close_fd(fd: RawFd) -> io::Result<()> {
    // SAFETY: `close` is the POSIX contract for a caller-owned descriptor;
    // the fixture passes its own standard-output descriptor.
    if unsafe { libc::close(fd) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn file_descriptor_identity(file: &File) -> io::Result<(u64, u64)> {
    let mut stat = MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `stat` is a correctly sized writable buffer and the descriptor is
    // borrowed from a live file.
    let result = unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstat returned success and initialized the buffer.
    let stat = unsafe { stat.assume_init() };
    Ok((stat.st_dev as u64, stat.st_ino))
}

pub fn set_no_sigpipe(stream: &UnixStream) -> io::Result<()> {
    let enabled: c_int = 1;
    // SAFETY: The descriptor is borrowed from a live socket and the option
    // value pointer is valid for the duration of the call.
    let result = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_NOSIGPIPE,
            (&enabled as *const c_int).cast::<c_void>(),
            mem::size_of::<c_int>() as libc::socklen_t,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Reduces a test client's receive window so IPC response backpressure can be
/// exercised deterministically. This helper is absent from production builds;
/// product code never tunes peer socket buffers.
#[cfg(any(test, feature = "test-fixture"))]
pub fn set_receive_buffer_size_for_tests(stream: &UnixStream, size: c_int) -> io::Result<c_int> {
    set_socket_buffer_size_for_tests(stream, libc::SO_RCVBUF, size)
}

/// Bounds the server-side send queue in incremental-write tests. Like the
/// receive-window helper, it is absent from production builds.
#[cfg(any(test, feature = "test-fixture"))]
pub fn set_send_buffer_size_for_tests(stream: &UnixStream, size: c_int) -> io::Result<c_int> {
    set_socket_buffer_size_for_tests(stream, libc::SO_SNDBUF, size)
}

#[cfg(any(test, feature = "test-fixture"))]
fn set_socket_buffer_size_for_tests(
    stream: &UnixStream,
    option: c_int,
    size: c_int,
) -> io::Result<c_int> {
    // SAFETY: the option value points to a live c_int for the call, and the fd
    // is borrowed from a live Unix-domain socket.
    let result = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            option,
            (&size as *const c_int).cast::<c_void>(),
            mem::size_of::<c_int>() as libc::socklen_t,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut actual = 0;
    let mut length = mem::size_of::<c_int>() as libc::socklen_t;
    // SAFETY: both output pointers are valid and writable for `length` bytes,
    // and the descriptor remains live for the call.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            option,
            (&mut actual as *mut c_int).cast::<c_void>(),
            &mut length,
        )
    };
    if result == 0 {
        Ok(actual)
    } else {
        Err(io::Error::last_os_error())
    }
}

const USER_SIGNAL_IDENT: usize = 1;
const MAX_KEVENTS: usize = 16;
/// Opaque kqueue tag for handshake reads. It is never dereferenced: Darwin
/// returns `udata` unchanged, letting pending handshakes stay distinct from
/// the existing subscriber `Event::Stream` namespace.
const PENDING_READ_TAG: usize = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    Listener,
    PendingReadable(RawFd),
    Stream(RawFd),
    StreamWritable(RawFd),
    ChildExit(u32),
    ConfigChanged,
    SignalWake,
}

/// A thin kqueue wrapper that drives the OpenCodeServerAgent event loop.
///
/// Registration calls are level-triggered by default; the child-exit filter
/// is one-shot because a watched PID can exit only once. All filters are
/// removed automatically when their descriptor closes, so dropping sockets
/// or files cannot leave stale registrations behind.
pub struct EventQueue {
    fd: c_int,
    listener_fd: Option<RawFd>,
}

/// A thread-safe handle that lets the signal thread wake the event loop.
/// It shares the kqueue descriptor but never closes it.
#[derive(Clone, Copy)]
pub struct EventWaker {
    queue_fd: c_int,
}

impl EventQueue {
    pub fn new() -> io::Result<Self> {
        // SAFETY: kqueue has no preconditions; the returned descriptor is
        // owned by this value.
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let queue = Self {
            fd,
            listener_fd: None,
        };
        // The user filter must exist before it can be triggered.
        queue.register(
            USER_SIGNAL_IDENT as RawFd,
            libc::EVFILT_USER,
            libc::EV_ADD,
            0,
        )?;
        Ok(queue)
    }

    pub fn waker(&self) -> EventWaker {
        EventWaker { queue_fd: self.fd }
    }

    pub fn watch_listener(&mut self, fd: RawFd) -> io::Result<()> {
        self.listener_fd = Some(fd);
        self.register(fd, libc::EVFILT_READ, libc::EV_ADD, 0)
    }

    pub fn watch_stream(&self, fd: RawFd) -> io::Result<()> {
        self.register(fd, libc::EVFILT_READ, libc::EV_ADD, 0)
    }

    pub fn watch_pending_readable(&self, fd: RawFd) -> io::Result<()> {
        self.register_with_tag(fd, libc::EVFILT_READ, libc::EV_ADD, 0, PENDING_READ_TAG)
    }

    pub fn watch_stream_writable(&self, fd: RawFd) -> io::Result<()> {
        self.register(fd, libc::EVFILT_WRITE, libc::EV_ADD, 0)
    }

    pub fn unwatch_stream_readable(&self, fd: RawFd) -> io::Result<()> {
        self.register(fd, libc::EVFILT_READ, libc::EV_DELETE, 0)
    }

    pub fn unwatch_stream_writable(&self, fd: RawFd) -> io::Result<()> {
        self.register(fd, libc::EVFILT_WRITE, libc::EV_DELETE, 0)
    }

    pub fn disable_listener(&self) -> io::Result<()> {
        self.register(
            self.registered_listener_fd()?,
            libc::EVFILT_READ,
            libc::EV_DISABLE,
            0,
        )
    }

    pub fn enable_listener(&self) -> io::Result<()> {
        self.register(
            self.registered_listener_fd()?,
            libc::EVFILT_READ,
            libc::EV_ENABLE,
            0,
        )
    }

    pub fn watch_child(&self, pid: u32) -> io::Result<()> {
        self.register(
            pid as RawFd,
            libc::EVFILT_PROC,
            libc::EV_ADD | libc::EV_ONESHOT,
            libc::NOTE_EXIT,
        )
    }

    pub fn unwatch_child(&self, pid: u32) -> io::Result<()> {
        self.register(pid as RawFd, libc::EVFILT_PROC, libc::EV_DELETE, 0)
    }

    pub fn watch_config(&self, file: &File) -> io::Result<()> {
        self.register(
            file.as_raw_fd(),
            libc::EVFILT_VNODE,
            libc::EV_ADD | libc::EV_CLEAR,
            libc::NOTE_DELETE
                | libc::NOTE_WRITE
                | libc::NOTE_RENAME
                | libc::NOTE_REVOKE
                | libc::NOTE_EXTEND,
        )
    }

    /// Waits up to `timeout` for events. An interrupted wait returns an
    /// empty batch so the caller can re-evaluate its timers.
    pub fn wait(&self, timeout: Duration) -> io::Result<Vec<Event>> {
        let timespec = libc::timespec {
            tv_sec: timeout.as_secs() as libc::time_t,
            tv_nsec: timeout.subsec_nanos() as libc::c_long,
        };
        let mut raw = [libc::kevent {
            ident: 0,
            filter: 0,
            flags: 0,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        }; MAX_KEVENTS];
        // SAFETY: `raw` is a writable event buffer of MAX_KEVENTS entries and
        // `timespec` is a valid pointer for the duration of the call.
        let count = unsafe {
            libc::kevent(
                self.fd,
                std::ptr::null(),
                0,
                raw.as_mut_ptr(),
                MAX_KEVENTS as c_int,
                &timespec,
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(Vec::new());
            }
            return Err(error);
        }
        let mut events = Vec::with_capacity(count as usize);
        for event in &raw[..count as usize] {
            match event.filter {
                libc::EVFILT_READ => {
                    if self.listener_fd == Some(event.ident as RawFd) {
                        events.push(Event::Listener);
                    } else if event.udata as usize == PENDING_READ_TAG {
                        events.push(Event::PendingReadable(event.ident as RawFd));
                    } else {
                        events.push(Event::Stream(event.ident as RawFd));
                    }
                }
                libc::EVFILT_WRITE => {
                    events.push(Event::StreamWritable(event.ident as RawFd));
                }
                libc::EVFILT_PROC => events.push(Event::ChildExit(event.ident as u32)),
                libc::EVFILT_VNODE => events.push(Event::ConfigChanged),
                libc::EVFILT_USER if event.ident == USER_SIGNAL_IDENT => {
                    events.push(Event::SignalWake);
                }
                _ => {}
            }
        }
        Ok(events)
    }

    fn register(&self, ident: RawFd, filter: i16, flags: u16, fflags: u32) -> io::Result<()> {
        self.register_with_tag(ident, filter, flags, fflags, 0)
    }

    fn register_with_tag(
        &self,
        ident: RawFd,
        filter: i16,
        flags: u16,
        fflags: u32,
        tag: usize,
    ) -> io::Result<()> {
        let change = libc::kevent {
            ident: ident as usize,
            filter,
            flags,
            fflags,
            data: 0,
            udata: tag as *mut c_void,
        };
        let timespec = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `change` is a valid one-element changelist and the timeout
        // pointer is valid; the event list is empty.
        let result =
            unsafe { libc::kevent(self.fd, &change, 1, std::ptr::null_mut(), 0, &timespec) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn registered_listener_fd(&self) -> io::Result<RawFd> {
        self.listener_fd.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "listener is not registered with the event queue",
            )
        })
    }
}

impl EventWaker {
    pub fn trigger_signal(&self) {
        let change = libc::kevent {
            ident: USER_SIGNAL_IDENT,
            filter: libc::EVFILT_USER,
            flags: 0,
            fflags: libc::NOTE_TRIGGER,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        let timespec = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `change` is a valid one-element changelist. Triggering a
        // user event is side-effect free beyond waking the queue, so a
        // failure here only delays shutdown until the next timer expiry.
        let _ = unsafe {
            libc::kevent(
                self.queue_fd,
                &change,
                1,
                std::ptr::null_mut(),
                0,
                &timespec,
            )
        };
    }
}

impl Drop for EventQueue {
    fn drop(&mut self) {
        // SAFETY: The descriptor is owned by this value and closed exactly
        // once here.
        unsafe { libc::close(self.fd) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_current_process() {
        let snapshot = process_snapshot(std::process::id()).expect("snapshot");
        assert_eq!(snapshot.pid, std::process::id());
        assert_eq!(snapshot.effective_uid, effective_uid());
        assert!(
            snapshot
                .executable
                .as_ref()
                .expect("current process has an executable path")
                .is_absolute()
        );
    }

    #[test]
    fn peer_credentials_match_for_socket_pair() {
        let (left, right) = UnixStream::pair().expect("socket pair");
        assert_eq!(
            peer_effective_uid(&left).expect("left uid"),
            effective_uid()
        );
        assert_eq!(
            peer_effective_uid(&right).expect("right uid"),
            effective_uid()
        );
    }

    #[test]
    fn no_sigpipe_option_applies_to_stream() {
        // SO_NOSIGPIPE fails with EINVAL on a stream socket whose peer has
        // already closed, so the peer end must stay alive here — and the
        // agent must apply the option right after accept, while the peer is
        // still connected.
        let (stream, _peer) = UnixStream::pair().expect("socket pair");
        set_no_sigpipe(&stream).expect("set SO_NOSIGPIPE");
    }

    #[test]
    fn config_watch_reports_file_writes() {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("ocs-kqueue-vnode-{}", std::process::id()));
        {
            let mut file = File::create(&path).expect("create watched file");
            file.write_all(b"one").expect("seed write");
            file.sync_all().expect("sync");
        }
        let file = File::open(&path).expect("open watched file");
        let queue = EventQueue::new().expect("event queue");
        queue.watch_config(&file).expect("watch config");
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open writer");
        writer.write_all(b"two").expect("triggering write");
        writer.sync_all().expect("sync");
        let events = queue
            .wait(Duration::from_secs(2))
            .expect("wait for vnode event");
        assert!(events.contains(&Event::ConfigChanged));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn child_exit_event_reports_pid() {
        let queue = EventQueue::new().expect("event queue");
        let mut child = Command::new("/usr/bin/true").spawn().expect("spawn true");
        queue.watch_child(child.id()).expect("watch child");
        let events = queue
            .wait(Duration::from_secs(5))
            .expect("wait for exit event");
        assert!(events.contains(&Event::ChildExit(child.id())));
        child.wait().expect("reap child");
    }

    #[test]
    fn watching_an_already_reaped_process_fails() {
        // Regression premise for the agent-loop race fix: when a watched
        // process exits and is reaped between the supervisor's last poll and
        // the EVFILT_PROC registration, EV_ADD fails (measured as ESRCH on
        // macOS 26). The agent loop treats any registration failure as a
        // routine race (log + immediate poll), not as a fatal error, so this
        // test locks the failure itself rather than a specific errno.
        let queue = EventQueue::new().expect("event queue");
        let mut child = Command::new("/usr/bin/true").spawn().expect("spawn true");
        child.wait().expect("reap child before watching");
        queue
            .watch_child(child.id())
            .expect_err("watching a reaped process must fail");
        // The queue itself stays usable for the next, live watch target.
        let mut next = Command::new("/usr/bin/true").spawn().expect("spawn true");
        queue.watch_child(next.id()).expect("watch next child");
        let events = queue
            .wait(Duration::from_secs(5))
            .expect("wait for exit event");
        assert!(events.contains(&Event::ChildExit(next.id())));
        next.wait().expect("reap next child");
    }

    #[test]
    fn waker_produces_signal_wake_event() {
        let queue = EventQueue::new().expect("event queue");
        let waker = queue.waker();
        waker.trigger_signal();
        let events = queue
            .wait(Duration::from_secs(2))
            .expect("wait for user event");
        assert_eq!(events, vec![Event::SignalWake]);
    }

    #[test]
    fn pending_and_subscriber_read_events_use_distinct_namespaces() {
        use std::io::Write;

        let queue = EventQueue::new().expect("event queue");
        let (pending, mut pending_peer) = UnixStream::pair().expect("pending pair");
        queue
            .watch_pending_readable(pending.as_raw_fd())
            .expect("watch pending read");
        pending_peer.write_all(b"p").expect("trigger pending read");
        let events = queue
            .wait(Duration::from_secs(2))
            .expect("wait for pending read");
        assert_eq!(events, vec![Event::PendingReadable(pending.as_raw_fd())]);

        let (subscriber, mut subscriber_peer) = UnixStream::pair().expect("subscriber pair");
        queue
            .watch_stream(subscriber.as_raw_fd())
            .expect("watch subscriber read");
        subscriber_peer
            .write_all(b"s")
            .expect("trigger subscriber read");
        let events = queue
            .wait(Duration::from_secs(2))
            .expect("wait for subscriber read");
        assert!(events.contains(&Event::Stream(subscriber.as_raw_fd())));
        assert!(!events.contains(&Event::PendingReadable(subscriber.as_raw_fd())));
    }

    #[test]
    fn writable_stream_events_are_reported_separately() {
        let queue = EventQueue::new().expect("event queue");
        let (stream, _peer) = UnixStream::pair().expect("socket pair");
        queue
            .watch_stream_writable(stream.as_raw_fd())
            .expect("watch writable stream");
        let events = queue
            .wait(Duration::from_secs(2))
            .expect("wait for writable stream");
        assert_eq!(events, vec![Event::StreamWritable(stream.as_raw_fd())]);
        queue
            .unwatch_stream_writable(stream.as_raw_fd())
            .expect("unwatch writable stream");
    }

    #[test]
    fn signal_process_refuses_broad_or_system_targets() {
        assert_eq!(
            signal_process(0, 0)
                .expect_err("PID 0 must be refused")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            signal_process(1, libc::SIGTERM)
                .expect_err("PID 1 must be refused")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            signal_process(u32::MAX, 0)
                .expect_err("out-of-range PID must be refused")
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn process_existence_queries_are_signal_free() {
        assert!(process_exists(std::process::id()));
        assert!(process_exists(1), "PID 1 always exists on macOS");
        assert!(!process_exists(0), "PID 0 is not a queryable process");
        assert!(!process_exists(u32::MAX));
    }

    #[test]
    fn process_group_queries_report_the_current_process() {
        assert!(own_process_group() >= 1);
        assert!(parent_process_id() >= 1);
        let parent_group = parent_process_group().expect("parent process group");
        assert!(parent_group >= 1);
    }

    #[test]
    fn process_group_capacity_growth_is_bounded() {
        assert_eq!(
            next_process_group_capacity(INITIAL_PROCESS_GROUP_CAPACITY, 16).expect("grow"),
            32
        );
        assert_eq!(
            next_process_group_capacity(32, 100).expect("grow past reported count"),
            100
        );
        assert!(
            next_process_group_capacity(MAX_PROCESS_GROUP_CAPACITY, MAX_PROCESS_GROUP_CAPACITY)
                .is_err(),
            "an exactly full hard-cap result is ambiguous and must fail closed"
        );
        assert!(
            next_process_group_capacity(16, MAX_PROCESS_GROUP_CAPACITY + 1).is_err(),
            "the process-group observation must not allocate from an unbounded count"
        );
    }

    #[test]
    fn ignore_signal_rejects_an_invalid_signal_number() {
        assert_eq!(
            ignore_signal(-1).expect_err("signal -1 must fail").kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn set_nonblocking_succeeds_on_a_valid_fd() {
        use std::os::unix::io::AsRawFd;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let fd = listener.as_raw_fd();
        set_nonblocking(fd).expect("set_nonblocking on a valid listener fd");
        // Verify O_NONBLOCK is set by checking F_GETFL.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags & libc::O_NONBLOCK != 0, "O_NONBLOCK is set");
    }

    #[test]
    fn set_nonblocking_fails_on_an_invalid_fd() {
        let error = set_nonblocking(-1).expect_err("set_nonblocking on fd -1");
        assert!(
            error.raw_os_error() == Some(libc::EBADF),
            "expected EBADF for invalid fd, got: {error}"
        );
    }
}
