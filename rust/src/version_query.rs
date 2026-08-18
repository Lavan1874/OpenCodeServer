use crate::platform::{
    Event, EventQueue, LogLevel, effective_uid, log, own_process_group, process_snapshot,
    set_nonblocking,
};
use crate::version_cleanup::converge_query_group;
use std::io;
use std::io::Read as _;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_OUTPUT_BYTES: usize = 4096;
const MAX_VERSION_BYTES: usize = 128;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum VersionQueryResult {
    Available(String),
    Unavailable,
    /// The query leader was observed outside its dedicated process group, or
    /// its identity could no longer be inspected while it was expected to be
    /// live. The caller must stop automatic queries for this executable until
    /// configuration changes or OpenCodeServerAgent restarts.
    Quarantined,
}

struct QueryCleanupState {
    group_authorized: bool,
    quarantined: bool,
    leader_snapshot_missing: bool,
    version: Option<String>,
}

impl QueryCleanupState {
    fn new(
        group_authorized: bool,
        quarantined: bool,
        leader_snapshot_missing: bool,
        version: Option<String>,
    ) -> Self {
        Self {
            group_authorized,
            quarantined,
            leader_snapshot_missing,
            version,
        }
    }
}

pub(crate) fn query_installed_version(
    executable: &Path,
    timeout: Duration,
    generation: String,
) -> VersionQueryResult {
    query_installed_version_impl(
        executable,
        timeout,
        generation,
        &set_nonblocking,
        &process_snapshot,
    )
}

fn query_installed_version_impl(
    executable: &Path,
    timeout: Duration,
    generation: String,
    set_nonblocking: &dyn Fn(std::os::unix::io::RawFd) -> io::Result<()>,
    snapshot: &dyn Fn(u32) -> io::Result<crate::platform::ProcessSnapshot>,
) -> VersionQueryResult {
    let _completion = QueryCompletion {
        executable,
        generation: &generation,
    };
    query_event(executable, "spawn-requested", &generation);

    // Create the event queue before the child. The direct child remains
    // waitable until the only cleanup function below has performed any
    // authorized signal and then reaped it.
    let event_queue = match EventQueue::new() {
        Ok(queue) => queue,
        Err(error) => {
            log(
                LogLevel::Error,
                &format!("Installed-version query could not create kqueue: {error}"),
            );
            return VersionQueryResult::Unavailable;
        }
    };

    let mut child = match Command::new(executable)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    {
        Ok(child) => {
            query_event(executable, "spawn-returned", &generation);
            child
        }
        Err(error) => {
            query_event(executable, "spawn-failed", &generation);
            log(
                LogLevel::Error,
                &format!("Installed-version query could not spawn the child: {error}"),
            );
            return VersionQueryResult::Unavailable;
        }
    };

    let pid = child.id();
    let expected_group = pid;
    query_event(
        executable,
        "pid-published",
        &format!("generation={generation};pid={pid}"),
    );

    let mut group_authorized = false;
    let mut quarantined = false;
    let mut leader_snapshot_missing = false;
    let mut leader_start = None;

    // Install the non-reaping exit watcher before the first identity
    // snapshot. Watching the child does not authorize any signal; it only
    // preserves an exit observation while the unreaped Child remains the
    // eventual ownership anchor. This ordering lets a first-snapshot ESRCH
    // be distinguished from a live identity-inspection failure without
    // racing watcher registration against a fast normal exit.
    if let Err(error) = event_queue.watch_child(pid) {
        log(
            LogLevel::Error,
            &format!(
                "Installed-version query could not watch child PID {pid}; failing closed: {error}"
            ),
        );
        return cleanup_query_child(
            child,
            None,
            executable,
            &generation,
            QueryCleanupState::new(group_authorized, quarantined, leader_snapshot_missing, None),
        );
    }

    match snapshot(pid) {
        Ok(snapshot)
            if snapshot.pid == pid
                && snapshot.process_group_id == expected_group
                && snapshot.effective_uid == effective_uid()
                && expected_group != own_process_group() =>
        {
            group_authorized = true;
            leader_start = Some((snapshot.start_seconds, snapshot.start_microseconds));
            query_event(
                executable,
                "group-ready",
                &format!("generation={generation};pgid={expected_group}"),
            );
        }
        Ok(snapshot) => {
            quarantined = true;
            query_event(
                executable,
                "group-escape-observed",
                &format!(
                    "generation={generation};pid={pid};pgid={}",
                    snapshot.process_group_id
                ),
            );
        }
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            leader_snapshot_missing = true;
            // A process snapshot can race a very short-lived, otherwise
            // ordinary query child. ESRCH here is only an observation gap:
            // cleanup must first prove the exact unreaped Child exited with
            // WNOWAIT and then verify that its process group has no residual
            // members before deciding whether this was safe. Marking the
            // result quarantined here would make that later clean closeout
            // sticky and discard valid version output.
            query_event(
                executable,
                "group-unobserved",
                &format!("generation={generation};pid={pid};error={error}"),
            );
        }
        Err(error) => {
            quarantined = true;
            query_event(
                executable,
                "group-unobserved",
                &format!("generation={generation};pid={pid};error={error}"),
            );
        }
    }

    let Some(mut stdout) = child.stdout.take() else {
        return cleanup_query_child(
            child,
            None,
            executable,
            &generation,
            QueryCleanupState::new(group_authorized, quarantined, leader_snapshot_missing, None),
        );
    };
    if let Err(error) = set_nonblocking(stdout.as_raw_fd()) {
        log(
            LogLevel::Error,
            &format!(
                "Installed-version query could not set non-blocking mode; failing closed: {error}"
            ),
        );
        return cleanup_query_child(
            child,
            Some(stdout),
            executable,
            &generation,
            QueryCleanupState::new(group_authorized, quarantined, leader_snapshot_missing, None),
        );
    }

    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::with_capacity(MAX_OUTPUT_BYTES);
    let mut buffer = [0_u8; 1024];
    let mut eof = false;
    let mut exit_observed = false;
    // A missing first snapshot remains provisionally readable. The
    // WNOWAIT-backed cleanup path is the authority that turns this race into
    // either a valid clean completion or a fail-closed quarantine.
    let mut invalid = quarantined;

    while !invalid && !(eof && exit_observed) {
        if Instant::now() >= deadline {
            query_event(
                executable,
                "deadline",
                &format!("generation={generation};pid={pid}"),
            );
            invalid = true;
            break;
        }

        if group_authorized && !exit_observed {
            match snapshot(pid) {
                Ok(snapshot)
                    if snapshot.pid == pid
                        && snapshot.process_group_id == expected_group
                        && snapshot.effective_uid == effective_uid()
                        && leader_start
                            == Some((snapshot.start_seconds, snapshot.start_microseconds)) => {}
                Ok(snapshot) => {
                    group_authorized = false;
                    quarantined = true;
                    invalid = true;
                    query_event(
                        executable,
                        "group-escape-observed",
                        &format!(
                            "generation={generation};pid={pid};pgid={}",
                            snapshot.process_group_id
                        ),
                    );
                    break;
                }
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
                    // The direct child may have exited just before kqueue is
                    // drained. Missing is not an identity mismatch; the
                    // unreaped Child and NOTE_EXIT watcher still define the
                    // safe closeout path.
                }
                Err(error) => {
                    group_authorized = false;
                    quarantined = true;
                    invalid = true;
                    query_event(
                        executable,
                        "group-unobserved",
                        &format!("generation={generation};pid={pid};error={error}"),
                    );
                    break;
                }
            }
        }

        match stdout.read(&mut buffer) {
            Ok(0) => {
                if !eof {
                    query_event(
                        executable,
                        "stdout-close-observed",
                        &format!("generation={generation};pid={pid}"),
                    );
                }
                eof = true;
            }
            Ok(count) => {
                if bytes.is_empty() {
                    query_event(
                        executable,
                        "stdout-first-read",
                        &format!("generation={generation};pid={pid}"),
                    );
                }
                if bytes.len() + count > MAX_OUTPUT_BYTES {
                    query_event(
                        executable,
                        "overflow-detected",
                        &format!(
                            "generation={generation};pid={pid};bytes={}",
                            bytes.len() + count
                        ),
                    );
                    invalid = true;
                    break;
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => {
                log(
                    LogLevel::Error,
                    &format!("Installed-version query could not read stdout: {error}"),
                );
                invalid = true;
                break;
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        match event_queue.wait(remaining.min(POLL_INTERVAL)) {
            Ok(events) if events.contains(&Event::ChildExit(pid)) => {
                if !exit_observed {
                    query_event(
                        executable,
                        "leader-exit-observed",
                        &format!("generation={generation};pid={pid}"),
                    );
                }
                exit_observed = true;
            }
            Ok(_) => {}
            Err(error) => {
                log(
                    LogLevel::Error,
                    &format!(
                        "Installed-version query lost its child-exit watcher for PID {pid}: {error}"
                    ),
                );
                invalid = true;
                break;
            }
        }
    }

    let version = (!invalid && eof && exit_observed)
        .then(|| parse_version(bytes))
        .flatten();
    cleanup_query_child(
        child,
        Some(stdout),
        executable,
        &generation,
        QueryCleanupState::new(
            group_authorized,
            quarantined,
            leader_snapshot_missing,
            version,
        ),
    )
}

/// Completes the query while `Child` still protects its PID and process-group
/// anchor from reuse. Group cleanup and its bounded confirmation happen before
/// `Child::wait` consumes the leader status.
fn cleanup_query_child(
    mut child: Child,
    stdout: Option<ChildStdout>,
    executable: &Path,
    generation: &str,
    mut state: QueryCleanupState,
) -> VersionQueryResult {
    let pid = child.id();
    let outcome = converge_query_group(
        &mut child,
        state.group_authorized,
        state.leader_snapshot_missing,
        state.version.is_none() || state.quarantined,
        executable,
        generation,
    );
    state.quarantined |= outcome.quarantined;

    if state.version.is_some() && !outcome.residual && !state.quarantined {
        query_event(
            executable,
            "clean-completion",
            &format!("generation={generation};pid={pid}"),
        );
    }

    drop(stdout);
    query_event(
        executable,
        "stdout-fd-closed",
        &format!("generation={generation};pid={pid}"),
    );
    let status = match child.wait() {
        Ok(status) => {
            query_event(
                executable,
                "leader-reaped",
                &format!("generation={generation};pid={pid}"),
            );
            status
        }
        Err(error) => {
            log(
                LogLevel::Error,
                &format!("Installed-version query could not reap PID {pid}: {error}"),
            );
            return if state.quarantined {
                VersionQueryResult::Quarantined
            } else {
                VersionQueryResult::Unavailable
            };
        }
    };

    if state.quarantined {
        VersionQueryResult::Quarantined
    } else if status.success() {
        state.version.map_or(
            VersionQueryResult::Unavailable,
            VersionQueryResult::Available,
        )
    } else {
        VersionQueryResult::Unavailable
    }
}

fn parse_version(bytes: Vec<u8>) -> Option<String> {
    let version = String::from_utf8(bytes).ok()?.trim().to_owned();
    (!version.is_empty()
        && version.len() <= MAX_VERSION_BYTES
        && !version.chars().any(|character| character.is_control()))
    .then_some(version)
}

pub(crate) fn query_generation() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{timestamp}", std::process::id())
}

pub(crate) fn query_event(executable: &Path, event: &str, detail: &str) {
    #[cfg(any(test, feature = "test-fixture"))]
    crate::test_events::emit(executable, event, detail);
    #[cfg(not(any(test, feature = "test-fixture")))]
    let _ = (executable, event, detail);
}

struct QueryCompletion<'a> {
    executable: &'a Path,
    generation: &'a str,
}

impl Drop for QueryCompletion<'_> {
    fn drop(&mut self) {
        query_event(self.executable, "worker-complete", self.generation);
    }
}

#[cfg(any(test, feature = "test-fixture"))]
pub fn query_installed_version_for_test(executable: &Path, timeout: Duration) -> Option<String> {
    match query_installed_version_impl(
        executable,
        timeout,
        query_generation(),
        &set_nonblocking,
        &process_snapshot,
    ) {
        VersionQueryResult::Available(version) => Some(version),
        VersionQueryResult::Unavailable | VersionQueryResult::Quarantined => None,
    }
}

#[cfg(any(test, feature = "test-fixture"))]
pub fn query_installed_version_with_for_test(
    executable: &Path,
    timeout: Duration,
    set_nonblocking: &dyn Fn(std::os::unix::io::RawFd) -> io::Result<()>,
) -> Option<String> {
    match query_installed_version_impl(
        executable,
        timeout,
        query_generation(),
        set_nonblocking,
        &process_snapshot,
    ) {
        VersionQueryResult::Available(version) => Some(version),
        VersionQueryResult::Unavailable | VersionQueryResult::Quarantined => None,
    }
}

#[cfg(any(test, feature = "test-fixture"))]
pub fn query_installed_version_with_snapshot_for_test(
    executable: &Path,
    timeout: Duration,
    snapshot: &dyn Fn(u32) -> io::Result<crate::platform::ProcessSnapshot>,
) -> Option<String> {
    match query_installed_version_impl(
        executable,
        timeout,
        query_generation(),
        &set_nonblocking,
        snapshot,
    ) {
        VersionQueryResult::Available(version) => Some(version),
        VersionQueryResult::Unavailable | VersionQueryResult::Quarantined => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_output_is_small_printable_text() {
        assert_eq!(parse_version(b"1.2.3\n".to_vec()).as_deref(), Some("1.2.3"));
        assert_eq!(parse_version(Vec::new()), None);
        assert_eq!(parse_version(b"bad\0value".to_vec()), None);
        assert_eq!(parse_version(vec![b'x'; MAX_VERSION_BYTES + 1]), None);
    }
}
