//! Disposable installed-version query process-group closeout.
//!
//! The query has no persisted ownership record. Its live `Child` remains
//! unreaped while this helper validates and, when needed, force-closes the
//! anchored group. A missing first snapshot may be recovered only when
//! `waitid(WNOWAIT)` anchors that exact child and the authorized group has no
//! residual members; every other identity uncertainty is direct-child-only
//! and quarantines the result, never inferring a foreign group target.

use crate::platform::{
    LogLevel, effective_uid, log, own_process_group, peek_child_exit, process_group_member_ids,
    process_snapshot, send_process_group_signal,
};
use crate::version_query::query_event;
use std::io;
use std::path::Path;
use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

const GROUP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const GROUP_ANCHOR_TIMEOUT: Duration = Duration::from_millis(250);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct QueryGroupOutcome {
    pub(crate) residual: bool,
    pub(crate) quarantined: bool,
}

pub(crate) fn converge_query_group(
    child: &mut Child,
    group_authorized: bool,
    leader_snapshot_missing: bool,
    terminate: bool,
    executable: &Path,
    generation: &str,
) -> QueryGroupOutcome {
    let pid = child.id();
    // A first snapshot can race a very fast leader exit. The unreaped Child
    // is then a non-reusable PID/PGID anchor, but only after WNOWAIT proves
    // that this exact child has exited. A later live escape does not get this
    // fallback: it remains direct-child-only and fail-closed.
    let waitable_group_anchor = leader_snapshot_missing
        && wait_for_waitable_group_anchor(pid)
        && pid > 1
        && pid != own_process_group();
    if !group_authorized && !waitable_group_anchor {
        query_event(
            executable,
            "leader-signal-requested",
            &format!("generation={generation};pid={pid}"),
        );
        let _ = child.kill();
        return QueryGroupOutcome {
            residual: false,
            quarantined: true,
        };
    }

    let members = match authorized_query_group_members(pid) {
        Ok(members) => members,
        Err(error) => {
            query_event(
                executable,
                "group-unobserved",
                &format!("generation={generation};pid={pid};error={error}"),
            );
            // The direct Child is still the only trustworthy target after
            // group inspection fails.
            let _ = child.kill();
            return QueryGroupOutcome {
                residual: false,
                quarantined: true,
            };
        }
    };

    if members.is_empty() {
        if terminate {
            signal_group(child, executable, generation);
        }
        return QueryGroupOutcome {
            residual: false,
            quarantined: false,
        };
    }

    query_event(
        executable,
        "group-residual-observed",
        &format!("generation={generation};pid={pid}"),
    );
    signal_group(child, executable, generation);
    wait_for_group_empty(pid, executable, generation);
    QueryGroupOutcome {
        residual: true,
        quarantined: true,
    }
}

/// A missing first snapshot can race a very short-lived leader. Keep the
/// unreaped `Child` as the only anchor and wait briefly for `waitid(WNOWAIT)`
/// to observe that exact child. No process-group inspection or signal is
/// attempted during this retry window.
fn wait_for_waitable_group_anchor(pid: u32) -> bool {
    let deadline = Instant::now() + GROUP_ANCHOR_TIMEOUT;
    loop {
        match peek_child_exit(pid) {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(None) | Err(_) => return false,
        }
    }
}

fn signal_group(child: &mut Child, executable: &Path, generation: &str) {
    let pid = child.id();
    query_event(
        executable,
        "signal-authorized",
        &format!("generation={generation};pid={pid};target=group"),
    );
    query_event(
        executable,
        "signal-requested",
        &format!("generation={generation};pid={pid};target=group"),
    );
    if let Err(error) = send_process_group_signal(pid, libc::SIGKILL)
        && error.raw_os_error() != Some(libc::ESRCH)
    {
        log(
            LogLevel::Error,
            &format!("Installed-version query could not close process group {pid}: {error}"),
        );
        let _ = child.kill();
    }
}

fn wait_for_group_empty(pid: u32, executable: &Path, generation: &str) {
    let deadline = Instant::now() + GROUP_CLEANUP_TIMEOUT;
    loop {
        match authorized_query_group_members(pid) {
            Ok(members) if members.is_empty() => return,
            Ok(_) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(_) => {
                log(
                    LogLevel::Error,
                    &format!(
                        "Installed-version query process group {pid} remained alive after explicit cleanup"
                    ),
                );
                return;
            }
            Err(error) => {
                log(
                    LogLevel::Error,
                    &format!(
                        "Installed-version query could not confirm process-group cleanup {pid}: {error}"
                    ),
                );
                query_event(
                    executable,
                    "group-unobserved",
                    &format!("generation={generation};pid={pid};error={error}"),
                );
                return;
            }
        }
    }
}

fn authorized_query_group_members(process_group_id: u32) -> io::Result<Vec<u32>> {
    if process_group_id <= 1 || process_group_id == own_process_group() {
        return Err(io::Error::other(
            "refusing to inspect OpenCodeServerAgent's own process group",
        ));
    }
    let mut members = Vec::new();
    let uid = effective_uid();
    for pid in process_group_member_ids(process_group_id)? {
        if pid <= 1 {
            return Err(io::Error::other(
                "installed-version query process group listed a system PID",
            ));
        }
        if pid == process_group_id {
            continue;
        }
        match process_snapshot(pid) {
            Ok(snapshot)
                if snapshot.pid == pid
                    && snapshot.effective_uid == uid
                    && snapshot.process_group_id == process_group_id =>
            {
                members.push(pid);
            }
            Ok(_) => {
                return Err(io::Error::other(
                    "installed-version query process group contains an untrusted member",
                ));
            }
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(members)
}
