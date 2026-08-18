//! Small, in-memory process-group observation and cleanup primitives.
//!
//! An owned group is signalable only while its direct `Child` remains
//! waitable. `waitid(WNOWAIT)` supplies that non-reusable anchor; the member
//! list is then used only to verify the still-live group before a cooperative
//! signal. Reattached records have no such anchor and are observation-only
//! when their recorded leader is missing.

use crate::platform::{
    effective_uid, own_process_group, peek_child_exit, process_group_member_ids, process_snapshot,
    send_process_group_signal,
};
use crate::runtime_state::ProcessRecord;
use std::io;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GroupObservation {
    Empty,
    HasMembers,
}

/// Observes members other than the still-owned direct leader.
pub(crate) fn observe_owned(
    record: &ProcessRecord,
    leader_pid: u32,
) -> io::Result<GroupObservation> {
    validate_owned_anchor(record, leader_pid)?;
    observe_members(record.process_group_id, Some(leader_pid))
}

/// Returns whether a reattached record's group has any observable members.
/// This is deliberately read-only. A persisted record with a missing leader
/// cannot authorize a group signal after the agent restarts.
pub(crate) fn observe_attached(record: &ProcessRecord) -> io::Result<bool> {
    validate_group_id(record.process_group_id)?;
    match observe_members(record.process_group_id, None)? {
        GroupObservation::Empty => Ok(false),
        GroupObservation::HasMembers => Ok(true),
    }
}

/// Signals an owned group after validating its anchored group and every
/// currently observable member. `leader_exited` is true only for the pending
/// cleanup state, where `waitid(WNOWAIT)` has already established that the
/// leader remains waitable even if proc_pidinfo no longer reports it.
pub(crate) fn signal_owned(
    record: &ProcessRecord,
    leader_pid: u32,
    leader_exited: bool,
    signal: i32,
) -> io::Result<()> {
    validate_owned_anchor(record, leader_pid)?;
    if !leader_exited {
        validate_live_leader(record, leader_pid)?;
    }
    observe_members(record.process_group_id, Some(leader_pid))?;
    send_process_group_signal(record.process_group_id, signal)
}

fn validate_owned_anchor(record: &ProcessRecord, leader_pid: u32) -> io::Result<()> {
    if leader_pid != record.pid
        || record.process_group_id != leader_pid
        || leader_pid <= 1
        || leader_pid == own_process_group()
    {
        return Err(io::Error::other(
            "the owned OpenCode child does not have a safe dedicated process-group anchor",
        ));
    }
    Ok(())
}

fn validate_group_id(process_group_id: u32) -> io::Result<()> {
    if process_group_id <= 1 || process_group_id == own_process_group() {
        return Err(io::Error::other(
            "refusing to inspect OpenCodeServerAgent's own process group",
        ));
    }
    Ok(())
}

fn validate_live_leader(record: &ProcessRecord, leader_pid: u32) -> io::Result<()> {
    match process_snapshot(leader_pid) {
        Ok(snapshot) => validate_leader_snapshot(record, &snapshot),
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            // proc_pidinfo may stop reporting a terminating child before the
            // waitable state is visible there. Require waitid to prove this is
            // our still-owned exit; a missing proof is fail-closed.
            match peek_child_exit(leader_pid)? {
                Some(_) => Ok(()),
                None => Err(io::Error::other(
                    "the owned OpenCode leader disappeared without a waitable exit",
                )),
            }
        }
        Err(error) => Err(error),
    }
}

fn validate_leader_snapshot(
    record: &ProcessRecord,
    snapshot: &crate::platform::ProcessSnapshot,
) -> io::Result<()> {
    let starts_match = record.identity_unconfirmed
        || (snapshot.start_seconds == record.start_seconds
            && snapshot.start_microseconds == record.start_microseconds);
    if snapshot.pid != record.pid || snapshot.effective_uid != effective_uid() || !starts_match {
        return Err(io::Error::other(
            "the owned OpenCode leader identity no longer matches",
        ));
    }
    if snapshot.process_group_id != record.process_group_id {
        return Err(io::Error::other(
            "the owned OpenCode leader abandoned its dedicated process group",
        ));
    }
    Ok(())
}

fn observe_members(process_group_id: u32, skip_pid: Option<u32>) -> io::Result<GroupObservation> {
    validate_group_id(process_group_id)?;
    let mut has_members = false;
    for pid in process_group_member_ids(process_group_id)? {
        if pid <= 1 {
            return Err(io::Error::other(
                "the OpenCode process group listing contained a system PID",
            ));
        }
        if skip_pid == Some(pid) {
            continue;
        }
        match process_snapshot(pid) {
            Ok(snapshot)
                if snapshot.pid == pid
                    && snapshot.process_group_id == process_group_id
                    && snapshot.effective_uid == effective_uid() =>
            {
                has_members = true;
            }
            Ok(_) => {
                return Err(io::Error::other(
                    "the OpenCode process group contains an untrusted member",
                ));
            }
            // Membership can change while it is observed. A member that is
            // already gone is not evidence of a foreign process; the next
            // observation remains authoritative for convergence.
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(if has_members {
        GroupObservation::HasMembers
    } else {
        GroupObservation::Empty
    })
}
