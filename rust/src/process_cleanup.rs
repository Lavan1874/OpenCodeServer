//! Owned-child exit observation and bounded cooperative group convergence.
//!
//! This module keeps the waitid/WNOWAIT state transitions beside the group
//! observation primitives. The public process wrapper only delegates to these
//! operations; no descendant ledger or persisted cleanup state is introduced.

use crate::platform::{ChildExitObservation, peek_child_exit};
use crate::process::{ExitReason, owned_identity_matches};
use crate::process_group::{GroupObservation, observe_owned, signal_owned};
use crate::runtime_state::ProcessRecord;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

/// Graceful-stop window for a child whose identity registration failed. A
/// survivor remains owned and is never automatically SIGKILLed.
const UNREGISTERED_CHILD_STOP_GRACE: Duration = Duration::from_secs(2);

pub struct PendingGroupCleanup {
    leader_exit: ExitReason,
    terminate_requested: bool,
    signal_allowed: bool,
}

impl PendingGroupCleanup {
    pub(crate) fn new(leader_exit: ExitReason) -> Self {
        Self {
            leader_exit,
            terminate_requested: false,
            signal_allowed: true,
        }
    }

    pub(crate) fn leader_exit(&self) -> &ExitReason {
        &self.leader_exit
    }

    pub(crate) fn terminate_requested(&self) -> bool {
        self.terminate_requested
    }

    pub(crate) fn mark_terminate_requested(&mut self) {
        self.terminate_requested = true;
    }

    pub(crate) fn signal_allowed(&self) -> bool {
        self.signal_allowed
    }

    pub(crate) fn refuse_signals(&mut self) {
        self.signal_allowed = false;
    }
}

pub(crate) enum UnregisteredChildShutdown {
    Reaped(ExitStatus),
    Survived {
        cleanup: Option<PendingGroupCleanup>,
    },
}

pub(crate) fn poll_owned_child(
    child: &mut Child,
    record: &ProcessRecord,
    cleanup: &mut Option<PendingGroupCleanup>,
    identity_failed: bool,
) -> io::Result<Option<ExitReason>> {
    let pid = child.id();
    if cleanup.is_some() {
        return poll_pending_group(child, record, pid, cleanup);
    }

    if identity_failed {
        return poll_identity_failed_child(child, record, pid, cleanup);
    }

    let Some(observed) = peek_child_exit(pid)? else {
        // A failed identity probe is not permission to signal or abandon an
        // owned child. The Child handle remains authoritative until waitid
        // reports an exit, so keep supervising it.
        // A previously verified record becoming false is an escape and must
        // be reported fail-closed. An already-unconfirmed survivor has never
        // authorized a signal; its owned Child anchor is still needed to
        // classify and reap a direct PID exit without repeating IdentityChanged.
        if !record.identity_unconfirmed && matches!(owned_identity_matches(record)?, Some(false)) {
            return Ok(Some(ExitReason::IdentityChanged));
        }
        return Ok(None);
    };
    // If the leader is already waitable, retain the same fail-closed check
    // when the kernel can still expose its final live identity. A group
    // signal is never authorized after an observed escape; ESRCH here is
    // tolerated only because WNOWAIT has independently anchored this Child.
    if !record.identity_unconfirmed && matches!(owned_identity_matches(record)?, Some(false)) {
        return Ok(Some(ExitReason::IdentityChanged));
    }
    let leader_exit = classify_child_exit(observed);
    let mut pending = PendingGroupCleanup::new(leader_exit);
    let observation = match observe_owned(record, pid) {
        Ok(observation) => observation,
        Err(error) => {
            pending.refuse_signals();
            *cleanup = Some(pending);
            return Err(error);
        }
    };
    match observation {
        GroupObservation::Empty => {
            let status = child.wait()?;
            Ok(Some(classify_exit(status)))
        }
        GroupObservation::HasMembers => {
            if let Err(error) = signal_owned(record, pid, true, libc::SIGTERM) {
                pending.refuse_signals();
                *cleanup = Some(pending);
                return Err(error);
            }
            pending.mark_terminate_requested();
            *cleanup = Some(pending);
            Ok(None)
        }
    }
}

/// Once a live owned child has escaped its recorded group, the supervisor is
/// deliberately read-only: it retains the Child anchor, waits for the direct
/// process to become waitable, and only reaps after the recorded group is
/// observed empty. No group signal is permitted after the identity failure.
fn poll_identity_failed_child(
    child: &mut Child,
    record: &ProcessRecord,
    pid: u32,
    cleanup: &mut Option<PendingGroupCleanup>,
) -> io::Result<Option<ExitReason>> {
    let Some(observed) = peek_child_exit(pid)? else {
        return Ok(None);
    };
    let mut pending = PendingGroupCleanup::new(classify_child_exit(observed));
    pending.refuse_signals();
    match observe_owned(record, pid) {
        Ok(GroupObservation::Empty) => {
            let status = child.wait()?;
            Ok(Some(classify_exit(status)))
        }
        Ok(GroupObservation::HasMembers) => {
            *cleanup = Some(pending);
            Ok(None)
        }
        Err(error) => {
            *cleanup = Some(pending);
            Err(error)
        }
    }
}

fn poll_pending_group(
    child: &mut Child,
    record: &ProcessRecord,
    pid: u32,
    cleanup: &mut Option<PendingGroupCleanup>,
) -> io::Result<Option<ExitReason>> {
    match observe_owned(record, pid) {
        Ok(GroupObservation::Empty) => {
            let status = child.wait()?;
            let exit = classify_exit(status);
            *cleanup = None;
            Ok(Some(exit))
        }
        Ok(GroupObservation::HasMembers) => {
            let should_signal = cleanup
                .as_ref()
                .is_some_and(|pending| pending.signal_allowed() && !pending.terminate_requested());
            if should_signal {
                if let Err(error) = signal_owned(record, pid, true, libc::SIGTERM) {
                    if let Some(pending) = cleanup.as_mut() {
                        pending.refuse_signals();
                    }
                    return Err(error);
                }
                if let Some(pending) = cleanup.as_mut() {
                    pending.mark_terminate_requested();
                }
            }
            Ok(None)
        }
        Err(error) => {
            if let Some(pending) = cleanup.as_mut() {
                pending.refuse_signals();
            }
            Err(error)
        }
    }
}

pub(crate) fn shutdown_unregistered_child(
    child: &mut Child,
    record: &ProcessRecord,
) -> UnregisteredChildShutdown {
    let deadline = Instant::now() + UNREGISTERED_CHILD_STOP_GRACE;
    let mut cleanup = None;
    let mut terminate_requested = false;

    loop {
        if let Some(pending) = cleanup.as_mut() {
            match observe_owned(record, child.id()) {
                Ok(GroupObservation::Empty) => match child.wait() {
                    Ok(status) => return UnregisteredChildShutdown::Reaped(status),
                    Err(_) => return UnregisteredChildShutdown::Survived { cleanup },
                },
                Ok(GroupObservation::HasMembers) => {
                    if pending.signal_allowed() && !pending.terminate_requested() {
                        if signal_owned(record, child.id(), true, libc::SIGTERM).is_err() {
                            pending.refuse_signals();
                            return UnregisteredChildShutdown::Survived { cleanup };
                        }
                        pending.mark_terminate_requested();
                    }
                }
                Err(_) => {
                    pending.refuse_signals();
                    return UnregisteredChildShutdown::Survived { cleanup };
                }
            }
        } else {
            match peek_child_exit(child.id()) {
                Ok(Some(observed)) => {
                    let mut pending = PendingGroupCleanup::new(classify_child_exit(observed));
                    match observe_owned(record, child.id()) {
                        Ok(GroupObservation::Empty) => match child.wait() {
                            Ok(status) => return UnregisteredChildShutdown::Reaped(status),
                            Err(_) => {
                                return UnregisteredChildShutdown::Survived {
                                    cleanup: Some(pending),
                                };
                            }
                        },
                        Ok(GroupObservation::HasMembers) => {
                            if signal_owned(record, child.id(), true, libc::SIGTERM).is_err() {
                                pending.refuse_signals();
                                return UnregisteredChildShutdown::Survived {
                                    cleanup: Some(pending),
                                };
                            }
                            pending.mark_terminate_requested();
                            cleanup = Some(pending);
                        }
                        Err(_) => {
                            pending.refuse_signals();
                            return UnregisteredChildShutdown::Survived {
                                cleanup: Some(pending),
                            };
                        }
                    }
                }
                Ok(None) => {
                    if !terminate_requested {
                        if signal_owned(record, child.id(), false, libc::SIGTERM).is_err() {
                            return UnregisteredChildShutdown::Survived { cleanup: None };
                        }
                        terminate_requested = true;
                    }
                }
                Err(_) => return UnregisteredChildShutdown::Survived { cleanup: None },
            }
        }

        if Instant::now() >= deadline {
            return UnregisteredChildShutdown::Survived { cleanup };
        }
        thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) fn classify_exit(status: ExitStatus) -> ExitReason {
    if let Some(code) = status.code() {
        ExitReason::Exited(code)
    } else if let Some(signal) = status.signal() {
        ExitReason::Signaled(signal)
    } else {
        ExitReason::Disappeared
    }
}

fn classify_child_exit(observed: ChildExitObservation) -> ExitReason {
    if let Some(code) = observed.code {
        ExitReason::Exited(code)
    } else if let Some(signal) = observed.signal {
        ExitReason::Signaled(signal)
    } else {
        ExitReason::Disappeared
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_waitid_exit_observations() {
        assert_eq!(
            classify_child_exit(ChildExitObservation {
                code: Some(7),
                signal: None,
            }),
            ExitReason::Exited(7)
        );
        assert_eq!(
            classify_child_exit(ChildExitObservation {
                code: None,
                signal: Some(libc::SIGTERM),
            }),
            ExitReason::Signaled(libc::SIGTERM)
        );
    }
}
