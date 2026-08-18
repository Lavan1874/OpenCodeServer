use crate::config::ValidatedConfig;
use crate::config_fingerprint::ConfigFingerprint;
use crate::platform::{
    LogLevel, ProcessSnapshot, configure_child_signal_mask, log, process_snapshot,
};
use crate::process_cleanup::{
    PendingGroupCleanup, UnregisteredChildShutdown, poll_owned_child, shutdown_unregistered_child,
};
use crate::process_group::{observe_attached, signal_owned};
use crate::runtime_state::ProcessRecord;
use std::io;
use std::net::{SocketAddr, TcpListener, ToSocketAddrs};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExitReason {
    Exited(i32),
    Signaled(i32),
    Disappeared,
    IdentityChanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordIdentity {
    Current,
    ExecutableVanished,
    ExecutableMismatch,
    GroupEscaped,
    Missing,
    Mismatched,
}

impl std::fmt::Display for ExitReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exited(code) => write!(formatter, "exited with status {code}"),
            Self::Signaled(signal) => write!(formatter, "terminated by signal {signal}"),
            Self::Disappeared => formatter.write_str("process disappeared"),
            Self::IdentityChanged => formatter.write_str("process identity no longer matches"),
        }
    }
}

pub enum ManagedProcess {
    /// The direct OpenCode child, optionally in the leader-exited cleanup
    /// phase. The Child is retained until the authorized group is empty,
    /// preventing PID/PGID reuse while descendants are converged.
    Child {
        child: Child,
        record: ProcessRecord,
        cleanup: Option<PendingGroupCleanup>,
        /// The child was observed outside its recorded dedicated group. Keep
        /// the owned handle for direct reaping, but never use the record as a
        /// signal authority again.
        identity_failed: bool,
    },
    Attached {
        record: ProcessRecord,
    },
}

/// A spawn failure. Preflight and exec failures carry no process. When the
/// child was created but its identity could not be confirmed, `survivor`
/// holds the still-owned child after a graceful termination attempt: it did
/// not exit within the bounded grace window, so ownership is handed back to
/// the caller, which must keep supervising it. The error path never drops a
/// live child and never sends SIGKILL.
pub struct SpawnError {
    error: io::Error,
    // Boxed so the common preflight/exec failure stays a small `Err`.
    survivor: Option<Box<ManagedProcess>>,
}

impl SpawnError {
    fn without_process(error: io::Error) -> Self {
        Self {
            error,
            survivor: None,
        }
    }

    pub fn kind(&self) -> io::ErrorKind {
        self.error.kind()
    }

    pub fn into_parts(self) -> (io::Error, Option<ManagedProcess>) {
        (self.error, self.survivor.map(|boxed| *boxed))
    }
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.error)
    }
}

impl std::fmt::Debug for SpawnError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SpawnError")
            .field("error", &self.error)
            .field(
                "survivor",
                &self.survivor.as_ref().map(|boxed| boxed.record()),
            )
            .finish()
    }
}

impl std::error::Error for SpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Failure of the post-spawn identity confirmation: the last probe error,
/// plus the last observed snapshot when one was available. The snapshot
/// feeds the survivor's identity record and its signal-target decision.
#[derive(Debug)]
struct UnconfirmedIdentity {
    error: io::Error,
    last_snapshot: Option<ProcessSnapshot>,
}

/// How the termination attempt on an unregistered child ended.
impl ManagedProcess {
    pub fn spawn(
        config: &ValidatedConfig,
        installed_version: Option<String>,
        config_fingerprint: ConfigFingerprint,
    ) -> Result<Self, SpawnError> {
        Self::spawn_impl(
            config,
            installed_version,
            config_fingerprint,
            &process_snapshot,
        )
    }

    /// Spawns OpenCode with an injectable process-snapshot probe. Production
    /// uses `spawn`; the process-supervision integration tests inject probe
    /// failures here to prove the post-spawn ownership rules. The probe is
    /// the only injection point: the spawned child's behavior is expressed
    /// by the test fixture itself (marker files next to the fixture copy),
    /// never by a production-path behavior switch.
    #[cfg(any(test, feature = "test-fixture"))]
    pub fn spawn_with_snapshot(
        config: &ValidatedConfig,
        installed_version: Option<String>,
        config_fingerprint: ConfigFingerprint,
        snapshot: &dyn Fn(u32) -> io::Result<ProcessSnapshot>,
    ) -> Result<Self, SpawnError> {
        Self::spawn_impl(config, installed_version, config_fingerprint, snapshot)
    }

    fn spawn_impl(
        config: &ValidatedConfig,
        installed_version: Option<String>,
        config_fingerprint: ConfigFingerprint,
        snapshot: &dyn Fn(u32) -> io::Result<ProcessSnapshot>,
    ) -> Result<Self, SpawnError> {
        ensure_endpoint_available(config).map_err(SpawnError::without_process)?;
        let mut command = Command::new(&config.configured_executable);
        command
            .arg("serve")
            .arg("--hostname")
            .arg(&config.source.hostname)
            .arg("--port")
            .arg(config.source.port.to_string())
            .env_remove("OPENCODE_SERVER_USERNAME")
            .env_remove("OPENCODE_SERVER_PASSWORD")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        if config.source.mdns {
            command.arg("--mdns");
        }
        if !config.source.password.is_empty() {
            command
                .env("OPENCODE_SERVER_USERNAME", &config.effective_username)
                .env("OPENCODE_SERVER_PASSWORD", &config.source.password);
        }
        configure_child_signal_mask(&mut command);
        let child = command.spawn().map_err(SpawnError::without_process)?;
        let pid = child.id();
        let snapshot = match wait_for_snapshot_with(pid, &config.canonical_executable, snapshot) {
            Ok(snapshot) => snapshot,
            Err(unconfirmed) => {
                return Err(Self::terminate_unconfirmed_child(
                    child,
                    unconfirmed.error,
                    unconfirmed.last_snapshot,
                    config,
                    config_fingerprint,
                ));
            }
        };
        if snapshot.process_group_id != pid {
            return Err(Self::terminate_unconfirmed_child(
                child,
                io::Error::other("OpenCode did not enter its dedicated process group"),
                Some(snapshot),
                config,
                config_fingerprint,
            ));
        }
        let record = record_from_snapshot(snapshot, config, installed_version, config_fingerprint);
        Ok(Self::Child {
            child,
            record,
            cleanup: None,
            identity_failed: false,
        })
    }

    /// Handles the "spawned but not yet registered" ownership window. While
    /// the `Child` is held and un-reaped, its PID cannot be recycled (only
    /// this process can reap it), so a graceful SIGTERM to the group
    /// constructed at spawn — or to the group last observed for it — cannot
    /// hit a reused PID. A group observed as OpenCodeServerAgent's own is
    /// never signaled. If the child still does not exit within the bounded
    /// grace window it is NOT killed: ownership continues as a supervised
    /// `ManagedProcess::Child` inside the returned error, so no live process
    /// is ever abandoned unmanaged and no second OpenCode can be started by
    /// mistake.
    fn terminate_unconfirmed_child(
        mut child: Child,
        error: io::Error,
        last_snapshot: Option<ProcessSnapshot>,
        config: &ValidatedConfig,
        config_fingerprint: ConfigFingerprint,
    ) -> SpawnError {
        let pid = child.id();
        log(
            LogLevel::Error,
            &format!(
                "OpenCode spawned (PID {pid}) but its identity could not be confirmed; requesting a graceful stop before failing the start: {error}"
            ),
        );
        let record = survivor_record(pid, last_snapshot, config, config_fingerprint);
        match shutdown_unregistered_child(&mut child, &record) {
            UnregisteredChildShutdown::Reaped(status) => {
                log(
                    LogLevel::Notice,
                    &format!(
                        "Unconfirmed OpenCode child (PID {pid}) {status} and was reaped before the start failure returned"
                    ),
                );
                SpawnError {
                    error,
                    survivor: None,
                }
            }
            UnregisteredChildShutdown::Survived { cleanup } => {
                log(
                    LogLevel::Fault,
                    &format!(
                        "Unconfirmed OpenCode child (PID {pid}) survived the graceful interval; it remains owned and supervised and no SIGKILL was sent"
                    ),
                );
                SpawnError {
                    error,
                    survivor: Some(Box::new(Self::Child {
                        child,
                        record,
                        cleanup,
                        identity_failed: false,
                    })),
                }
            }
        }
    }

    pub fn attach(record: ProcessRecord) -> Self {
        Self::Attached { record }
    }

    pub fn record(&self) -> &ProcessRecord {
        match self {
            Self::Child { record, .. } | Self::Attached { record } => record,
        }
    }

    pub fn record_mut(&mut self) -> &mut ProcessRecord {
        match self {
            Self::Child { record, .. } | Self::Attached { record } => record,
        }
    }

    /// Returns the direct-leader exit while group descendants are still
    /// pending convergence. It is intentionally an in-memory fact; the
    /// persisted `ProcessRecord` remains the only cross-agent schema.
    pub fn pending_group_exit(&self) -> Option<&ExitReason> {
        match self {
            Self::Child {
                cleanup: Some(cleanup),
                ..
            } => Some(cleanup.leader_exit()),
            Self::Child { cleanup: None, .. } | Self::Attached { .. } => None,
        }
    }

    pub fn pending_group_authorized(&self) -> bool {
        match self {
            Self::Child {
                cleanup: Some(cleanup),
                identity_failed,
                ..
            } => !*identity_failed && cleanup.signal_allowed(),
            Self::Child { cleanup: None, .. } | Self::Attached { .. } => true,
        }
    }

    pub fn is_owned_child(&self) -> bool {
        matches!(self, Self::Child { .. })
    }

    /// Reports whether the in-memory ownership evidence currently permits a
    /// lifecycle signal.  This is the status-time capability snapshot; the
    /// actual signal path still revalidates the complete process identity to
    /// close the command-time race.
    pub(crate) fn signal_authorized(&self) -> bool {
        match self {
            Self::Child {
                cleanup,
                identity_failed,
                ..
            } => {
                !*identity_failed
                    && cleanup
                        .as_ref()
                        .is_none_or(|pending| pending.signal_allowed())
            }
            Self::Attached { record } => !record.identity_unconfirmed,
        }
    }

    pub fn mark_identity_failed(&mut self) {
        if let Self::Child {
            identity_failed, ..
        } = self
        {
            *identity_failed = true;
        }
    }

    pub fn poll_exit(&mut self) -> io::Result<Option<ExitReason>> {
        match self {
            Self::Child {
                child,
                record,
                cleanup,
                identity_failed,
            } => poll_owned_child(child, record, cleanup, *identity_failed),
            Self::Attached { record } => match process_snapshot(record.pid) {
                Ok(snapshot) => match identity_probe(&snapshot, record) {
                    IdentityProbe::Mismatch | IdentityProbe::GroupEscaped => {
                        Ok(Some(ExitReason::IdentityChanged))
                    }
                    IdentityProbe::Match
                    | IdentityProbe::ExecutableVanished
                    | IdentityProbe::ExecutableMismatch => Ok(None),
                },
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
                    Ok(Some(ExitReason::Disappeared))
                }
                Err(error) => Err(error),
            },
        }
    }

    pub fn send_terminate(&self) -> io::Result<()> {
        self.signal(libc::SIGTERM)
    }

    pub fn send_kill(&self) -> io::Result<()> {
        self.signal(libc::SIGKILL)
    }

    fn signal(&self, signal: i32) -> io::Result<()> {
        match self {
            Self::Child {
                child,
                record,
                cleanup,
                identity_failed,
            } => {
                if *identity_failed {
                    return Err(io::Error::other(
                        "refusing to signal an OpenCode child whose process-group identity changed",
                    ));
                }
                if cleanup
                    .as_ref()
                    .is_some_and(|pending| !pending.signal_allowed())
                {
                    return Err(io::Error::other(
                        "refusing to signal an OpenCode child while its process-group identity is unverified",
                    ));
                }
                signal_owned(record, child.id(), cleanup.is_some(), signal)
            }
            // Attached process (post-restart): the persisted record is the
            // only identity basis, so the full identity must revalidate
            // before any signal.
            Self::Attached { record } => {
                if !identity_matches(record)? {
                    return Err(io::Error::other(
                        "refusing to signal a process whose identity changed",
                    ));
                }
                if record.process_group_id == crate::platform::own_process_group() {
                    return Err(io::Error::other(
                        "refusing to signal OpenCodeServerAgent's own process group",
                    ));
                }
                crate::platform::send_process_group_signal(record.process_group_id, signal)
            }
        }
    }

    /// Attempts one identity confirmation for an owned child whose kernel
    /// identity was never observed at spawn (the zero-start survivor).
    /// Returns true when the record was upgraded to a confirmed identity
    /// and can be persisted for a later OpenCodeServerAgent restart.
    ///
    /// Only valid while the `Child` handle is held, so the PID cannot have
    /// been recycled; the snapshot must additionally prove the child still
    /// leads its constructed group and runs the configured executable. A
    /// child that escaped its group stays unconfirmed (its record never
    /// authorizes signals and restart keeps it unverified).
    pub fn confirm_unconfirmed_identity(&mut self) -> bool {
        let Self::Child { child, record, .. } = self else {
            return false;
        };
        if !record.identity_unconfirmed {
            return false;
        }
        let Ok(snapshot) = process_snapshot(child.id()) else {
            return false;
        };
        let executable_matches = snapshot
            .executable
            .as_deref()
            .is_some_and(|path| paths_equal(path, Path::new(&record.executable)));
        if snapshot.pid != record.pid
            || snapshot.process_group_id != snapshot.pid
            || !executable_matches
        {
            return false;
        }
        record.start_seconds = snapshot.start_seconds;
        record.start_microseconds = snapshot.start_microseconds;
        record.identity_unconfirmed = false;
        true
    }
}

pub fn inspect_record_identity(record: &ProcessRecord) -> io::Result<RecordIdentity> {
    let snapshot = match process_snapshot(record.pid) {
        Ok(snapshot) => snapshot,
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            return Ok(RecordIdentity::Missing);
        }
        Err(error) => return Err(error),
    };
    Ok(match identity_probe(&snapshot, record) {
        IdentityProbe::Match => RecordIdentity::Current,
        IdentityProbe::ExecutableVanished => RecordIdentity::ExecutableVanished,
        IdentityProbe::ExecutableMismatch => RecordIdentity::ExecutableMismatch,
        IdentityProbe::GroupEscaped => RecordIdentity::GroupEscaped,
        IdentityProbe::Mismatch => RecordIdentity::Mismatched,
    })
}

fn record_from_snapshot(
    snapshot: ProcessSnapshot,
    config: &ValidatedConfig,
    running_version: Option<String>,
    config_fingerprint: ConfigFingerprint,
) -> ProcessRecord {
    ProcessRecord {
        pid: snapshot.pid,
        process_group_id: snapshot.process_group_id,
        start_seconds: snapshot.start_seconds,
        start_microseconds: snapshot.start_microseconds,
        executable: snapshot
            .executable
            .as_deref()
            // wait_for_snapshot only accepts a snapshot whose executable path
            // is known and matches, so this fallback is unreachable; it keeps
            // the record total without a panic path.
            .unwrap_or(&config.canonical_executable)
            .to_string_lossy()
            .into_owned(),
        started_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        running_version,
        config_fingerprint,
        identity_unconfirmed: false,
    }
}

fn wait_for_snapshot_with(
    pid: u32,
    expected_executable: &Path,
    snapshot: &dyn Fn(u32) -> io::Result<ProcessSnapshot>,
) -> Result<ProcessSnapshot, UnconfirmedIdentity> {
    // A freshly execed fixture can change process-group membership from its
    // first user-space instruction. Require one short, second observation so
    // the spawn path does not accept a transient dedicated-group snapshot
    // immediately before an escape. This is a bounded startup confirmation,
    // not a permanent process-group lease; later escapes remain observable
    // identity failures during normal supervision.
    const IDENTITY_SETTLE: Duration = Duration::from_millis(25);
    let mut last_error = None;
    let mut last_snapshot = None;
    for _ in 0..20 {
        match snapshot(pid) {
            Ok(observed) => {
                // A freshly spawned process must still have its executable
                // file on disk; a vanished path here means something replaced
                // the target mid-spawn, which must not be accepted.
                if observed
                    .executable
                    .as_deref()
                    .is_some_and(|path| paths_equal(path, expected_executable))
                {
                    thread::sleep(IDENTITY_SETTLE);
                    match snapshot(pid) {
                        Ok(settled)
                            if settled
                                .executable
                                .as_deref()
                                .is_some_and(|path| paths_equal(path, expected_executable)) =>
                        {
                            return Ok(settled);
                        }
                        Ok(settled) => {
                            last_error = Some(io::Error::other(
                                "spawned process executable does not match the configured target after identity settle",
                            ));
                            last_snapshot = Some(settled);
                        }
                        Err(error) => last_error = Some(error),
                    }
                    continue;
                }
                last_error = Some(io::Error::other(
                    "spawned process executable does not match the configured target",
                ));
                last_snapshot = Some(observed);
            }
            Err(error) => last_error = Some(error),
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(UnconfirmedIdentity {
        error: last_error.unwrap_or_else(|| io::Error::other("unable to inspect spawned process")),
        last_snapshot,
    })
}

/// Identity record for a child whose identity confirmation failed after
/// spawn. `identity_unconfirmed` is true when either no snapshot was ever
/// obtained (the kernel identity was never observed) or the snapshot proved
/// the child escaped its constructed process group. In both cases the
/// record authorizes nothing: no signal, no takeover, no second OpenCode.
/// Zeroed kernel start fields ensure `identity_probe` returns `Mismatch` on
/// restart, but `try_reattach` for `identity_unconfirmed` records only
/// checks `Missing`, so a live PID stays unverified rather than being
/// discarded as stale.
///
/// When `last_snapshot` exists and the child stayed in its constructed
/// group (executable-mismatch path), the real kernel identity is safe to
/// persist: the child is still reachable via the constructed group, so
/// Stop/Force Stop work through the `Child` handle, and on restart the
/// record enters the `ExecutableMismatch` attach path.
fn survivor_record(
    pid: u32,
    last_snapshot: Option<ProcessSnapshot>,
    config: &ValidatedConfig,
    config_fingerprint: ConfigFingerprint,
) -> ProcessRecord {
    let identity_unconfirmed = last_snapshot
        .as_ref()
        .is_none_or(|snapshot| snapshot.process_group_id != pid);
    let (start_seconds, start_microseconds) = if identity_unconfirmed {
        (0, 0)
    } else {
        let snapshot = last_snapshot.as_ref().expect("confirmed snapshot");
        (snapshot.start_seconds, snapshot.start_microseconds)
    };
    ProcessRecord {
        pid,
        process_group_id: pid,
        start_seconds,
        start_microseconds,
        executable: config.canonical_executable.to_string_lossy().into_owned(),
        started_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        running_version: None,
        config_fingerprint,
        identity_unconfirmed,
    }
}

pub(crate) fn identity_matches(record: &ProcessRecord) -> io::Result<bool> {
    let snapshot = match process_snapshot(record.pid) {
        Ok(snapshot) => snapshot,
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => return Ok(false),
        Err(error) => return Err(error),
    };
    // ExecutableVanished counts as matching: pid + start time + uid +
    // process-group leadership still pin the recorded process, so signalling
    // it stays safe even though its executable file was replaced on disk.
    Ok(!matches!(
        identity_probe(&snapshot, record),
        IdentityProbe::Mismatch | IdentityProbe::GroupEscaped
    ))
}

/// Owned-child polling needs to distinguish a transiently unreportable
/// terminating task from a live process whose verified identity changed.
/// `None` is deliberately not an identity mismatch: waitid remains the
/// authority for the Child's eventual terminal state.
pub(crate) fn owned_identity_matches(record: &ProcessRecord) -> io::Result<Option<bool>> {
    let snapshot = match process_snapshot(record.pid) {
        Ok(snapshot) => snapshot,
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(!matches!(
        identity_probe(&snapshot, record),
        IdentityProbe::Mismatch | IdentityProbe::GroupEscaped
    )))
}

/// Reports whether a recorded group still has an authorized member. This is
/// intentionally separate from `identity_matches`: a missing leader PID is
/// expected while a cooperative descendant keeps the group alive.
pub fn authorized_process_group_has_members(record: &ProcessRecord) -> io::Result<bool> {
    observe_attached(record)
}

/// How a live process snapshot compares against the persisted record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IdentityProbe {
    /// Every recorded kernel identity field and the executable path match.
    Match,
    ExecutableVanished,
    ExecutableMismatch,
    /// PID, UID, start identity, and executable match, but the process no
    /// longer leads the recorded dedicated group. The PID is still the same
    /// process, but the recorded group is no longer a safe signal target.
    GroupEscaped,
    Mismatch,
}

fn identity_probe(snapshot: &ProcessSnapshot, record: &ProcessRecord) -> IdentityProbe {
    let process_identity_matches = snapshot.pid == record.pid
        && snapshot.effective_uid == crate::platform::effective_uid()
        && snapshot.start_seconds == record.start_seconds
        && snapshot.start_microseconds == record.start_microseconds;
    if !process_identity_matches {
        return IdentityProbe::Mismatch;
    }
    if snapshot.process_group_id != record.process_group_id
        || snapshot.process_group_id != snapshot.pid
    {
        return IdentityProbe::GroupEscaped;
    }
    match &snapshot.executable {
        Some(executable) if paths_equal(executable, Path::new(&record.executable)) => {
            IdentityProbe::Match
        }
        Some(_) => IdentityProbe::ExecutableMismatch,
        None => IdentityProbe::ExecutableVanished,
    }
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn ensure_endpoint_available(config: &ValidatedConfig) -> io::Result<()> {
    // The integration fixture can hold an OS listener through the complete
    // spawn-to-bind handoff. This marker is compiled into test-fixture builds
    // only; normal OpenCodeServerAgent builds always execute the real
    // endpoint preflight below.
    #[cfg(any(test, feature = "test-fixture"))]
    if config
        .configured_executable
        .with_file_name("port-reservation-held")
        .is_file()
    {
        return Ok(());
    }
    let hostname = config
        .source
        .hostname
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(&config.source.hostname);
    let addresses: Vec<_> = (hostname, config.source.port)
        .to_socket_addrs()?
        .take(8)
        .collect();
    if addresses.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "configured hostname resolved to no local address",
        ));
    }
    ensure_addresses_available(&addresses).map_err(|error| {
        if error.kind() == io::ErrorKind::AddrInUse {
            io::Error::new(
                io::ErrorKind::AddrInUse,
                format!(
                    "configured endpoint {} is already in use",
                    config.endpoint()
                ),
            )
        } else {
            error
        }
    })
}

/// Binds every candidate address to prove the endpoint is free. Rust's std
/// listener sets SO_REUSEADDR on macOS, so leftover TIME_WAIT sockets from
/// a predecessor that already exited do not fail this probe; only a socket
/// that is still bound — a live listener, or one whose owner is still mid
/// teardown — reports AddrInUse (see ADR 0011).
fn ensure_addresses_available(addresses: &[SocketAddr]) -> io::Result<()> {
    let mut listeners = Vec::new();
    for address in addresses {
        listeners.push(TcpListener::bind(address)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;
    use std::path::PathBuf;

    fn record_fixture() -> ProcessRecord {
        ProcessRecord {
            pid: 4242,
            process_group_id: 4242,
            start_seconds: 1_700_000_000,
            start_microseconds: 123_456,
            executable: "/opt/fixture/bin/opencode".to_owned(),
            started_at_unix_seconds: 1_700_000_000,
            running_version: None,
            config_fingerprint: ConfigFingerprint {
                version: 1,
                hmac_sha256: "00".repeat(32),
            },
            identity_unconfirmed: false,
        }
    }

    fn snapshot_fixture(record: &ProcessRecord) -> ProcessSnapshot {
        ProcessSnapshot {
            pid: record.pid,
            parent_pid: 1,
            process_group_id: record.process_group_id,
            effective_uid: crate::platform::effective_uid(),
            start_seconds: record.start_seconds,
            start_microseconds: record.start_microseconds,
            // Paths that never exist on disk keep paths_equal on its literal
            // comparison fallback, so the probes below stay hermetic.
            executable: Some(PathBuf::from(&record.executable)),
        }
    }

    #[test]
    fn identical_snapshot_is_a_full_match() {
        let record = record_fixture();
        let snapshot = snapshot_fixture(&record);
        assert_eq!(identity_probe(&snapshot, &record), IdentityProbe::Match);
    }

    #[test]
    fn matching_process_identity_with_a_new_group_is_group_escape() {
        let record = record_fixture();
        let mut snapshot = snapshot_fixture(&record);
        snapshot.process_group_id += 1;
        assert_eq!(
            identity_probe(&snapshot, &record),
            IdentityProbe::GroupEscaped
        );
    }

    #[test]
    fn vanished_executable_still_identifies_the_process() {
        let record = record_fixture();
        let mut snapshot = snapshot_fixture(&record);
        snapshot.executable = None;
        assert_eq!(
            identity_probe(&snapshot, &record),
            IdentityProbe::ExecutableVanished
        );
    }

    #[test]
    fn a_different_executable_path_is_an_executable_mismatch() {
        let record = record_fixture();
        let mut snapshot = snapshot_fixture(&record);
        snapshot.executable = Some(PathBuf::from("/opt/other/bin/opencode"));
        assert_eq!(
            identity_probe(&snapshot, &record),
            IdentityProbe::ExecutableMismatch
        );
    }

    #[test]
    fn a_vanished_executable_does_not_rescue_a_reused_pid() {
        // PID reuse necessarily changes the kernel start timestamp; the
        // missing file must never weaken the remaining identity fields.
        let record = record_fixture();
        let mut snapshot = snapshot_fixture(&record);
        snapshot.executable = None;
        snapshot.start_seconds += 1;
        assert_eq!(identity_probe(&snapshot, &record), IdentityProbe::Mismatch);
        let mut snapshot = snapshot_fixture(&record);
        snapshot.executable = None;
        snapshot.start_microseconds += 1;
        assert_eq!(identity_probe(&snapshot, &record), IdentityProbe::Mismatch);
    }

    #[test]
    fn classifies_normal_exit() {
        let status = Command::new("/usr/bin/true").status().expect("true status");
        assert_eq!(
            crate::process_cleanup::classify_exit(status),
            ExitReason::Exited(0)
        );
    }

    /// Binds a throwaway listener on 127.0.0.1, accepts one connection, and
    /// closes the server side first so the accepted socket lingers in
    /// TIME_WAIT. Returns the address plus the client, which stays alive so
    /// the TIME_WAIT entry persists for the duration of the test.
    fn address_with_stale_time_wait() -> (SocketAddr, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind throwaway listener");
        let address = listener.local_addr().expect("listener address");
        let client = TcpStream::connect(address).expect("connect throwaway client");
        let (accepted, _) = listener.accept().expect("accept throwaway connection");
        drop(accepted);
        drop(listener);
        (address, client)
    }

    /// Locks in the endpoint-probe contract that a predecessor which has
    /// already fully exited must not be reported as a port conflict, even
    /// while its accepted connections still linger in TIME_WAIT.
    #[test]
    fn stale_time_wait_does_not_fail_the_endpoint_probe() {
        let (address, _client) = address_with_stale_time_wait();
        // Premise, empirically verified on macOS 15: std sets SO_REUSEADDR
        // on unix listeners, so a direct bind already succeeds over stale
        // TIME_WAIT sockets. The probe inherits that behavior.
        assert!(
            TcpListener::bind(address).is_ok(),
            "std listeners must set SO_REUSEADDR and bind over stale TIME_WAIT"
        );
        ensure_addresses_available(&[address]).expect("TIME_WAIT must not count as a conflict");
    }

    #[test]
    fn live_listener_fails_the_endpoint_probe() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind live listener");
        let address = listener.local_addr().expect("listener address");
        let error = ensure_addresses_available(&[address])
            .expect_err("a live listener must keep failing the probe");
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
    }
}
