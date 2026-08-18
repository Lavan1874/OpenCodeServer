//! OpenCode child-exit observation, authorized-group convergence, and recovery.
//! The Child anchor and runtime record stay managed until group cleanup and
//! durable state clearing both complete.

use super::runtime_durability::RuntimePersistence;
use super::*;
use crate::runtime_state::ProcessRecord;

impl Supervisor {
    /// Orchestrates one process-supervision tick through focused helpers.
    pub(super) fn poll_process(&mut self, _now: Instant) {
        if !self.upgrade_unconfirmed_identity() {
            return;
        }
        let was_explicit_stop = matches!(
            self.server_state,
            ServerState::Stopping | ServerState::StopTimedOut
        );
        let exit = self.poll_process_exit();
        if self.handle_pending_group_cleanup(was_explicit_stop) {
            return;
        }
        let Some(exit) = exit else {
            return;
        };
        if self.handle_attached_leader_disappearance(&exit) || self.handle_identity_changed(&exit) {
            return;
        }
        self.complete_process_exit(was_explicit_stop, exit);
    }

    /// Upgrades a transiently unconfirmed owned Child only after its anchored
    /// snapshot proves the executable and dedicated group again.
    fn upgrade_unconfirmed_identity(&mut self) -> bool {
        let Some(process) = self.process.as_mut() else {
            return true;
        };
        if !process.confirm_unconfirmed_identity() {
            return true;
        }
        self.runtime.process = Some(process.record().clone());
        if !self.persist_runtime() {
            self.server_state = ServerState::Failed;
            self.health_state = HealthState::Unknown;
            log(
                LogLevel::Fault,
                "OpenCode identity was confirmed in memory but could not be persisted; it remains supervised",
            );
            return false;
        }
        log(
            LogLevel::Notice,
            "Confirmed the kernel identity of a previously unconfirmed OpenCode survivor",
        );
        true
    }

    fn poll_process_exit(&mut self) -> Option<ExitReason> {
        self.process
            .as_mut()
            .and_then(|process| match process.poll_exit() {
                Ok(exit) => exit,
                Err(error) => {
                    self.last_error = Some(format!("OpenCode process inspection failed: {error}"));
                    None
                }
            })
    }

    /// Keeps the Child and WNOWAIT anchor until the leader's group converges.
    fn handle_pending_group_cleanup(&mut self, was_explicit_stop: bool) -> bool {
        let Some(process) = self.process.as_ref() else {
            return false;
        };
        if process.pending_group_exit().is_none() {
            return false;
        }
        if !process.pending_group_authorized() {
            self.stop_deadline = None;
            self.server_state = ServerState::Failed;
            self.last_error = Some(
                "OpenCode exited but its process-group identity could not be verified — no signal or restart was attempted"
                    .to_owned(),
            );
            self.persist_runtime();
            return true;
        }
        if !was_explicit_stop && !self.group_cleanup_recovery {
            self.group_cleanup_recovery = true;
            self.restart_after_stop = false;
        }
        if !matches!(
            self.server_state,
            ServerState::Stopping | ServerState::StopTimedOut
        ) {
            self.server_state = ServerState::Stopping;
            self.health_state = HealthState::Unknown;
            self.stop_deadline = Some(Instant::now() + GRACEFUL_STOP);
            self.last_error = Some(
                "OpenCode exited; waiting for its authorized process group to finish stopping"
                    .to_owned(),
            );
            self.persist_runtime();
            log(
                LogLevel::Notice,
                "OpenCode leader exited; keeping its authorized process group under graceful supervision",
            );
        }
        true
    }

    /// Classifies a missing Attached leader by read-only group observation.
    fn handle_attached_leader_disappearance(&mut self, exit: &ExitReason) -> bool {
        if *exit != ExitReason::Disappeared
            || self
                .process
                .as_ref()
                .is_none_or(ManagedProcess::is_owned_child)
        {
            return false;
        }
        let Some(mut record) = self
            .process
            .as_ref()
            .map(|process| process.record().clone())
        else {
            return true;
        };
        match authorized_process_group_has_members(&record) {
            Ok(false) => false,
            Ok(true) | Err(_) => {
                record.identity_unconfirmed = true;
                record.start_seconds = 0;
                record.start_microseconds = 0;
                self.process = None;
                self.active_config = None;
                self.stale_config_process = false;
                self.running_version = None;
                self.health_state = HealthState::Unknown;
                self.stop_deadline = None;
                self.process_started = None;
                self.restart_after_stop = false;
                self.group_cleanup_recovery = false;
                self.next_restart = None;
                self.mark_unverified_process(
                    record,
                    "the reattached OpenCode leader disappeared while its process group remained or could not be verified",
                );
                true
            }
        }
    }

    /// Removes signal authority after an identity escape while retaining an
    /// owned Child for safe direct reaping.
    fn handle_identity_changed(&mut self, exit: &ExitReason) -> bool {
        if *exit != ExitReason::IdentityChanged {
            return false;
        }
        let Some(mut record) = self
            .process
            .as_ref()
            .map(|process| process.record().clone())
        else {
            return true;
        };
        record.identity_unconfirmed = true;
        record.start_seconds = 0;
        record.start_microseconds = 0;
        if self
            .process
            .as_ref()
            .is_some_and(ManagedProcess::is_owned_child)
        {
            if let Some(process) = self.process.as_mut() {
                process.mark_identity_failed();
            }
        } else {
            self.process = None;
        }
        self.active_config = None;
        self.stale_config_process = false;
        self.running_version = None;
        self.health_state = HealthState::Unknown;
        self.mark_unverified_process(
            record,
            "the attached process identity changed before it could be revalidated",
        );
        true
    }

    fn complete_process_exit(&mut self, was_explicit_stop: bool, exit: ExitReason) {
        log(
            if was_explicit_stop {
                LogLevel::Notice
            } else {
                LogLevel::Error
            },
            &format!("OpenCode {exit}"),
        );
        let previous_record = self.runtime.process.take();
        let record_for_retry = previous_record.clone().or_else(|| {
            self.process
                .as_ref()
                .map(|process| process.record().clone())
        });
        self.clear_live_process_state();
        if self.clear_process_record_durably(record_for_retry, was_explicit_stop, &exit) {
            self.resume_after_process_exit(was_explicit_stop, exit);
        }
    }

    fn clear_live_process_state(&mut self) {
        self.process = None;
        self.active_config = None;
        self.stale_config_process = false;
        self.running_version = None;
        self.health_state = HealthState::Unknown;
        self.stop_deadline = None;
        self.process_started = None;
        self.unverified_process_record = false;
        self.unverified_check_at = None;
    }

    /// Clears the process record transactionally, retaining evidence on failure.
    fn clear_process_record_durably(
        &mut self,
        record_for_retry: Option<ProcessRecord>,
        was_explicit_stop: bool,
        exit: &ExitReason,
    ) -> bool {
        self.runtime.process = None;
        match self.persist_runtime_detailed() {
            RuntimePersistence::Durable => true,
            RuntimePersistence::Failed => {
                self.runtime.process = record_for_retry;
                self.unverified_process_record = true;
                self.unverified_check_at = Some(Instant::now() + UNVERIFIED_CHECK_INTERVAL);
                self.next_restart = None;
                self.next_port_retry = None;
                self.port_release_deadline = None;
                self.pending_start_trigger = None;
                self.server_state = ServerState::Failed;
                self.health_state = HealthState::Unknown;
                log(
                    LogLevel::Fault,
                    "OpenCode exited but its runtime record could not be cleared; recovery is blocked until the clear is durable",
                );
                false
            }
            RuntimePersistence::Uncertain => {
                self.pending_process_exit = Some((was_explicit_stop, exit.clone()));
                self.next_restart = None;
                self.next_port_retry = None;
                self.port_release_deadline = None;
                self.pending_start_trigger = None;
                self.server_state = ServerState::Failed;
                self.health_state = HealthState::Unknown;
                log(
                    LogLevel::Fault,
                    "OpenCode exited but its runtime record clear is not yet durable; recovery is blocked until retry",
                );
                false
            }
        }
    }

    pub(super) fn resume_after_process_exit(&mut self, was_explicit_stop: bool, exit: ExitReason) {
        if was_explicit_stop {
            // Explicit Stop remains authoritative over an earlier automatic
            // residual cleanup. ForceStop is another action in that stopped
            // intent and must not resurrect recovery or restart.
            if self.runtime.desired_state == DesiredState::Stopped {
                self.group_cleanup_recovery = false;
                self.restart_after_stop = false;
                self.next_restart = None;
                self.server_state = ServerState::Stopped;
                self.last_error = None;
            } else if self.group_cleanup_recovery {
                self.group_cleanup_recovery = false;
                self.schedule_next_restart();
            } else if self.restart_after_stop {
                self.restart_after_stop = false;
                self.start_now(StartTrigger::AfterStop);
            } else {
                self.server_state = ServerState::Stopped;
                self.last_error = None;
            }
            return;
        }
        if self.runtime.desired_state == DesiredState::Running {
            self.last_error = Some(format!("OpenCode {exit}; automatic recovery is pending"));
            if !self.recovery_incident_active {
                self.recovery_incident_active = true;
                let _ = self.emit_notification(
                    NotificationKind::Failure,
                    "OpenCode stopped unexpectedly",
                    "OpenCodeServer is attempting bounded automatic recovery.",
                );
            }
            self.schedule_next_restart();
        } else {
            self.server_state = ServerState::Stopped;
        }
    }

    pub(super) fn schedule_next_restart(&mut self) {
        if self.restart_attempt_index >= RESTART_BACKOFF.len() {
            self.finish_recovery_failure();
            return;
        }
        let delay = RESTART_BACKOFF[self.restart_attempt_index];
        self.restart_attempt_index += 1;
        self.next_restart = Some(Instant::now() + delay);
        self.server_state = ServerState::WaitingToRestart;
        self.last_error = Some(format!(
            "Automatic recovery attempt {} of {} will start in {} seconds",
            self.restart_attempt_index,
            RESTART_BACKOFF.len(),
            delay.as_secs()
        ));
        log(
            LogLevel::Info,
            &format!(
                "Scheduled automatic recovery attempt {} of {}",
                self.restart_attempt_index,
                RESTART_BACKOFF.len()
            ),
        );
    }

    pub(super) fn finish_recovery_failure(&mut self) {
        self.next_restart = None;
        self.server_state = ServerState::Failed;
        self.last_error =
            Some("OpenCode automatic recovery stopped after five bounded attempts".to_owned());
        if self.recovery_incident_active {
            self.recovery_incident_active = false;
            self.emit_notification(
                NotificationKind::FinalFailure,
                "OpenCode recovery stopped",
                "Five recovery attempts were exhausted. Review the configuration and logs.",
            );
        }
        log(
            LogLevel::Fault,
            "Automatic recovery stopped after five bounded attempts",
        );
    }
}
