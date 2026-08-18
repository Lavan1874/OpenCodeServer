use super::reattach_policy::{
    FinalAction, HealthVerdict, InitialAction, decide_after_health, decide_initial,
};
use super::runtime_durability::RuntimePersistence;
use super::*;
use crate::health;

impl Supervisor {
    pub(super) fn try_reattach(
        &mut self,
        record: crate::runtime_state::ProcessRecord,
    ) -> io::Result<()> {
        // Collect the facts, ask the pure policy for each decision phase,
        // and flat-dispatch the returned action: decide_initial encodes
        // gates 0–3, and — only on NeedsHealthCheck — the conditional
        // health check plus a second identity inspection feed
        // decide_after_health (gate 4). The policy performs no I/O and
        // holds no state; every side effect below stays in the existing
        // helpers. See docs/refactor/reattachment-policy-boundary.md.
        let identity = inspect_record_identity(&record).map_err(|_| ());
        // `inspect_record_identity` intentionally describes the direct
        // process only. A leader can leave descendants behind, so a missing
        // leader is not stale until the recorded group is also observed
        // empty. Without the original Child anchor this observation is
        // read-only: never reattach or signal a missing leader's group.
        if matches!(identity, Ok(RecordIdentity::Missing)) {
            match authorized_process_group_has_members(&record) {
                Ok(true) => {
                    self.mark_unverified_process(
                        record,
                        "the recorded leader is gone but its process group still has members; no signal authority remains after restart",
                    );
                    return Ok(());
                }
                Err(_) => {
                    self.mark_unverified_process(
                        record,
                        "the recorded leader is gone but its process-group identity could not be verified",
                    );
                    return Ok(());
                }
                Ok(_) => {}
            }
        }
        if !record.identity_unconfirmed
            && matches!(identity, Ok(RecordIdentity::ExecutableVanished))
        {
            log(
                LogLevel::Notice,
                "The recorded OpenCode executable file was replaced on disk; kernel process identity still matches",
            );
        }
        let config = self.latest_config.clone();
        let config_matches = config.as_ref().is_some_and(|config| {
            self.config_fingerprint_key
                .verifies(&record.config_fingerprint, config)
        });
        match decide_initial(
            record.identity_unconfirmed,
            identity,
            config,
            config_matches,
            self.credentials.state(),
        ) {
            InitialAction::DiscardStaleRecord { reason } => {
                self.discard_stale_process_record(reason);
            }
            InitialAction::MarkUnverified { reason } => {
                self.mark_unverified_process(record, reason);
            }
            InitialAction::AttachUnconfirmed { reason } => {
                self.attach_unconfirmed_process(record, reason);
            }
            InitialAction::AttachStaleConfig { reason } => {
                self.attach_stale_config_process(record, reason);
            }
            InitialAction::NeedsHealthCheck { config } => {
                // Synchronous bounded I/O by design: this runs inside
                // Supervisor::new, before the event loop starts, and the
                // reattach decision (gate 4) needs the authenticated result
                // before any state is committed. Same ADR 0009 exemption as
                // `check_health`: HEALTH_TIMEOUT bounds the wait at 2 s.
                let health = health::check(&config, HEALTH_TIMEOUT);
                // Digest the health result into the policy-local tri-state
                // verdict before the second identity inspection.
                let health = match health.as_ref() {
                    Ok(result) if result.healthy => HealthVerdict::Healthy {
                        version: &result.version,
                    },
                    Ok(_) => HealthVerdict::Unhealthy,
                    Err(_) => HealthVerdict::Failed,
                };
                let identity = inspect_record_identity(&record).map_err(|_| ());
                match decide_after_health(identity, health, config) {
                    FinalAction::ReattachHealthy { version, config } => {
                        log(
                            LogLevel::Notice,
                            "Strict process identity and health checks passed; reattached to OpenCode",
                        );
                        self.running_version = Some(version.clone());
                        let mut record = record;
                        record.running_version = Some(version);
                        record.config_fingerprint =
                            self.config_fingerprint_key.fingerprint(&config);
                        self.process = Some(ManagedProcess::attach(record.clone()));
                        self.runtime.process = Some(record);
                        self.active_config = Some(config);
                        self.server_state = ServerState::Healthy;
                        self.health_state = HealthState::Healthy;
                        self.process_started = Some(Instant::now());
                        if !self.persist_runtime() {
                            self.server_state = ServerState::Failed;
                            self.health_state = HealthState::Unknown;
                            log(
                                LogLevel::Fault,
                                "Reattached OpenCode but its process identity could not be persisted; it remains supervised",
                            );
                        }
                    }
                    FinalAction::DiscardStaleRecord { reason } => {
                        self.discard_stale_process_record(reason);
                    }
                    FinalAction::MarkUnverified { reason } => {
                        self.mark_unverified_process(record, reason);
                    }
                    FinalAction::AttachUnconfirmed { reason } => {
                        self.attach_unconfirmed_process(record, reason);
                    }
                }
            }
        }
        Ok(())
    }

    fn discard_stale_process_record(&mut self, reason: &str) -> bool {
        let previous_record = self.runtime.process.take();
        self.runtime.process = None;
        self.unverified_process_record = false;
        self.unverified_check_at = None;
        match self.persist_runtime_detailed() {
            RuntimePersistence::Durable => {}
            RuntimePersistence::Failed => {
                // A failed atomic save leaves the previous state file in
                // place. Restore the in-memory record and keep it unverified
                // so a later retry cannot start a second OpenCode or claim
                // that stale state was durably removed.
                self.runtime.process = previous_record;
                self.unverified_process_record = true;
                self.unverified_check_at = Some(Instant::now() + UNVERIFIED_CHECK_INTERVAL);
                self.server_state = ServerState::Failed;
                log(
                    LogLevel::Fault,
                    "Could not durably discard the stale OpenCode process record; it remains unverified for retry",
                );
                return false;
            }
            RuntimePersistence::Uncertain => {
                // The rename may have made the cleared state visible. Do not
                // restore an old record into memory; the retry gate keeps
                // lifecycle actions stopped until the clear is durable.
                self.pending_durable_convergence =
                    Some(if self.runtime.desired_state == DesiredState::Running {
                        PendingDurableConvergence::Start(if self.restart_after_stop {
                            StartTrigger::AfterStop
                        } else {
                            StartTrigger::Cold
                        })
                    } else {
                        PendingDurableConvergence::Stop
                    });
                self.server_state = ServerState::Failed;
                self.health_state = HealthState::Unknown;
                log(
                    LogLevel::Fault,
                    "Stale OpenCode process record clear may be visible but is not durable; retrying before lifecycle actions",
                );
                return false;
            }
        }
        log(
            LogLevel::Notice,
            &format!("Discarded stale OpenCode process record: {reason}; no process was signaled"),
        );
        true
    }

    /// Periodically re-checks whether an unverified process PID has
    /// disappeared. If the PID is provably gone (`ESRCH`), the record is
    /// discarded and the supervisor converges according to `desired_state`.
    /// Any live process, mismatch, or inspection error keeps the record
    /// unverified: no signal, no takeover, no second OpenCode.
    pub(super) fn check_unverified_process(&mut self, now: Instant) {
        self.unverified_check_at = Some(now + UNVERIFIED_CHECK_INTERVAL);
        let Some(record) = self.runtime.process.clone() else {
            return;
        };
        match inspect_record_identity(&record) {
            Ok(RecordIdentity::Missing) => {
                if let Ok(false) = authorized_process_group_has_members(&record) {
                    if !self.discard_stale_process_record(
                        "the unverified process and its authorized process group are gone; resuming normal supervision",
                    ) {
                        return;
                    }
                    if self.restart_after_stop {
                        self.restart_after_stop = false;
                        self.restart_attempt_index = 0;
                        self.next_restart = None;
                        self.start_now(StartTrigger::AfterStop);
                    } else if self.runtime.desired_state == DesiredState::Running {
                        self.restart_attempt_index = 0;
                        self.next_restart = None;
                        self.start_now(StartTrigger::Cold);
                    } else {
                        self.server_state = ServerState::Stopped;
                        self.last_error = None;
                    }
                }
                // A descendant can keep the dedicated group alive after the
                // recorded PID disappears. Keep blocking, and never infer a
                // foreign signal target from an uncertain group observation.
            }
            _ => {
                // Still alive or uncertain - keep blocking
            }
        }
    }

    pub(super) fn mark_unverified_process(
        &mut self,
        record: crate::runtime_state::ProcessRecord,
        reason: &str,
    ) {
        self.runtime.process = Some(record);
        self.unverified_process_record = true;
        self.unverified_check_at = Some(Instant::now() + UNVERIFIED_CHECK_INTERVAL);
        self.server_state = ServerState::Failed;
        self.last_error = Some(
            "Existing OpenCode could not be reattached safely — it was left running and not signaled"
                .to_owned(),
        );
        let _ = self.persist_runtime();
        log(
            LogLevel::Fault,
            &format!(
                "Existing OpenCode process could not be safely reattached and was left untouched: {reason}"
            ),
        );
    }

    fn attach_unconfirmed_process(
        &mut self,
        record: crate::runtime_state::ProcessRecord,
        reason: &str,
    ) {
        self.process = Some(ManagedProcess::attach(record.clone()));
        self.runtime.process = Some(record);
        self.unverified_process_record = true;
        self.unverified_check_at = Some(Instant::now() + UNVERIFIED_CHECK_INTERVAL);
        self.server_state = ServerState::Failed;
        self.last_error = Some(
            "An existing process was reattached with unconfirmed identity — it remains supervised"
                .to_owned(),
        );
        let _ = self.persist_runtime();
        log(
            LogLevel::Notice,
            &format!(
                "Reattached an existing process with unconfirmed identity ({reason}); no second OpenCode will be started"
            ),
        );
    }

    /// Adopts an identity-verified process whose recorded configuration no
    /// longer matches the current one. The kernel process identity was just
    /// verified against the spawn record, so the process is provably the
    /// child this product started — the configuration drift does not weaken
    /// that evidence, and stop-time identity revalidation still guards every
    /// signal. The process therefore stays managed: Stop and Restart remain
    /// available (Restart is the documented convergence path), no second
    /// OpenCode competes for the port, and the user is never stranded with
    /// an unmanaged process holding the endpoint. `config_pending` tells the
    /// GUI to offer the restart; `recheck_stale_process` upgrades the
    /// attachment in place if the configuration matches again (a credential
    /// became readable, or the change was reverted).
    fn attach_stale_config_process(
        &mut self,
        record: crate::runtime_state::ProcessRecord,
        reason: &str,
    ) {
        self.process = Some(ManagedProcess::attach(record.clone()));
        self.runtime.process = Some(record);
        self.active_config = None;
        self.stale_config_process = true;
        self.server_state = ServerState::Unhealthy;
        self.health_state = HealthState::Unknown;
        self.last_error = Some(format!(
            "OpenCode is running with its previous configuration — {reason}"
        ));
        if !self.persist_runtime() {
            self.server_state = ServerState::Failed;
            self.health_state = HealthState::Unknown;
        }
        log(
            LogLevel::Notice,
            "Reattached an identity-verified OpenCode whose configuration is outdated; it remains managed and Restart applies the changes",
        );
    }

    /// Marks a stale-configuration attachment eligible for the asynchronous
    /// health worker when the current configuration once again matches the
    /// recorded fingerprint. The worker's task key carries this identity and
    /// configuration generation, and `apply_stale_health_result` performs the
    /// required post-health identity recheck before upgrading the attachment.
    /// Any mismatch keeps the stale attachment — Restart remains the
    /// convergence path.
    pub(super) fn recheck_stale_process(&mut self) {
        if !self.stale_config_process {
            return;
        }
        let Some(record) = self.runtime.process.clone() else {
            self.stale_config_process = false;
            return;
        };
        if self.process.is_none() {
            self.stale_config_process = false;
            return;
        }
        match inspect_record_identity(&record) {
            Ok(RecordIdentity::Current | RecordIdentity::ExecutableVanished) => {}
            // Exit and identity changes converge through the normal
            // supervision paths; anything else keeps the stale attachment.
            _ => return,
        }
        let Some(config) = self.latest_config.clone() else {
            return;
        };
        let config_matches = self
            .config_fingerprint_key
            .verifies(&record.config_fingerprint, &config);
        if !config_matches {
            return;
        }
        // Keep the strict identity-before-health gate, but leave the
        // authenticated request to the single-flight worker. The result path
        // rechecks this same record before it can be adopted.
        if let Ok(RecordIdentity::Current | RecordIdentity::ExecutableVanished) =
            inspect_record_identity(&record)
        {
            self.last_health_check = None;
        }
    }
}
