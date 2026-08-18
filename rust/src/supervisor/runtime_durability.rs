use super::*;
use crate::runtime_state::RuntimeStateSaveOutcome;

/// Retry a failed runtime-state write through the event loop. A pending
/// lifecycle intent is applied only after this retry is durable, so IPC
/// remains available and a storage fault cannot spin the agent.
const RUNTIME_STATE_RETRY_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RuntimePersistence {
    Durable,
    Failed,
    Uncertain,
}

impl Supervisor {
    pub(super) fn persist_runtime(&mut self) -> bool {
        // Metadata paths (health, version, notifications) must not consume
        // the retry window for a failed lifecycle write. Only an explicit
        // command or the scheduled durability retry may clear this gate.
        if self.runtime_state_retry_pending {
            return false;
        }
        self.persist_runtime_detailed() == RuntimePersistence::Durable
    }

    pub(super) fn persist_runtime_detailed(&mut self) -> RuntimePersistence {
        if !self.runtime_state_loaded {
            // Never overwrite an unreadable state file with a guessed
            // default. This keeps a potentially live process discoverable by
            // a later repair/restart instead of destroying its only record.
            return RuntimePersistence::Failed;
        }
        let outcome = self.runtime.save_with_durability(&self.paths);
        match outcome {
            RuntimeStateSaveOutcome::Durable => {
                let previous_runtime_error = self.runtime_state_error.take();
                self.runtime_state_reliable = true;
                self.runtime_state_retry_pending = false;
                self.runtime_state_retry_at = None;
                if self.last_error.as_deref() == previous_runtime_error.as_deref() {
                    self.last_error = None;
                }
                if self.runtime.launch_pending.is_none() {
                    self.launch_pending_clear_requested = false;
                }
                RuntimePersistence::Durable
            }
            RuntimeStateSaveOutcome::Failed(error) => {
                self.record_runtime_state_failure(error);
                RuntimePersistence::Failed
            }
            RuntimeStateSaveOutcome::Uncertain(error) => {
                self.record_runtime_state_failure(error);
                RuntimePersistence::Uncertain
            }
        }
    }

    fn record_runtime_state_failure(&mut self, error: io::Error) {
        let message = format!("Runtime state could not be saved: {error}");
        self.runtime_state_reliable = false;
        self.runtime_state_retry_pending = true;
        self.runtime_state_retry_at = Some(Instant::now() + RUNTIME_STATE_RETRY_INTERVAL);
        self.runtime_state_error = Some(message.clone());
        self.last_error = Some(message);
        log(LogLevel::Error, "Runtime state could not be saved");
    }

    /// Persisting the desired state is a transaction boundary for every
    /// explicit lifecycle command. A definite write failure restores the
    /// prior in-memory intent; an unsynced rename keeps the new intent and
    /// defers its one-shot action until a durable retry. Both return an
    /// IPC-visible error before any process signal or spawn is attempted.
    pub(super) fn persist_desired_state(
        &mut self,
        desired_state: DesiredState,
        intent: PendingLifecycleIntent,
    ) -> io::Result<()> {
        if !self.runtime_state_loaded {
            return Err(self.runtime_state_unavailable_error());
        }
        let previous = self.runtime.clone();
        let previous_intent = self.pending_lifecycle_intent;
        let previous_convergence = self.pending_durable_convergence;
        self.runtime.desired_state = desired_state;
        let outcome = self.persist_runtime_detailed();
        match outcome {
            RuntimePersistence::Durable => {
                self.pending_lifecycle_intent = None;
                self.pending_durable_convergence = None;
                Ok(())
            }
            RuntimePersistence::Failed => {
                self.runtime = previous;
                self.pending_lifecycle_intent = previous_intent;
                self.pending_durable_convergence = previous_convergence;
                Err(self.runtime_state_unavailable_error())
            }
            RuntimePersistence::Uncertain => {
                // The rename may already have made the new intent visible.
                // Keep the candidate in memory and wait for a durable retry;
                // restoring the old value here could make memory and disk
                // disagree in either direction.
                self.pending_lifecycle_intent = Some(intent);
                self.pending_durable_convergence = None;
                Err(self.runtime_state_unavailable_error())
            }
        }
    }

    fn runtime_state_unavailable_error(&self) -> io::Error {
        io::Error::other(
            self.runtime_state_error
                .as_deref()
                .unwrap_or("Runtime state is not durably available"),
        )
    }

    pub(super) fn runtime_state_ready(&mut self) -> bool {
        if let Some(error) = self.runtime_state_error.clone() {
            self.last_error = Some(error);
            return false;
        }
        if self.runtime.launch_pending.is_some() || self.launch_pending_clear_requested {
            let message =
                "An OpenCode launch transaction is not durably finalized; lifecycle start is blocked"
                    .to_owned();
            self.runtime_state_error = Some(message.clone());
            self.last_error = Some(message);
            return false;
        }
        if self.runtime_state_loaded && self.runtime_state_reliable {
            return true;
        }
        let message = "Runtime state is not durably available".to_owned();
        self.runtime_state_error = Some(message.clone());
        self.last_error = Some(message);
        false
    }

    /// Keep the marker-unwind retry alive when a newer command arrives after
    /// the original marker write became visible. The command replaces the
    /// old post-durability action, but it must not strand the marker with no
    /// scheduled retry.
    pub(super) fn defer_durable_convergence(&mut self, convergence: PendingDurableConvergence) {
        let retry_scheduled =
            self.runtime_state_retry_pending && self.runtime_state_retry_at.is_some();
        if self.launch_pending_clear_requested || retry_scheduled {
            self.pending_durable_convergence = Some(convergence);
            self.runtime_state_retry_pending = true;
            self.runtime_state_retry_at = Some(Instant::now() + RUNTIME_STATE_RETRY_INTERVAL);
        }
    }

    pub(super) fn defer_lifecycle_intent(&mut self, intent: PendingLifecycleIntent) {
        if self.launch_pending_clear_requested {
            self.pending_lifecycle_intent = Some(intent);
            self.runtime_state_retry_pending = true;
            self.runtime_state_retry_at = Some(Instant::now() + RUNTIME_STATE_RETRY_INTERVAL);
        }
    }

    /// Consume one post-durability lifecycle action. The caller invokes this
    /// only after the runtime-state retry gate and any launch-marker unwind
    /// have both completed durably.
    pub(super) fn resume_pending_durable_convergence(&mut self) {
        if !self.runtime_state_loaded
            || !self.runtime_state_reliable
            || self.runtime_state_retry_pending
            || self.runtime.launch_pending.is_some()
            || self.launch_pending_clear_requested
        {
            return;
        }
        let Some(convergence) = self.pending_durable_convergence.take() else {
            return;
        };
        match convergence {
            PendingDurableConvergence::Start(trigger)
                if self.runtime.desired_state == DesiredState::Running
                    && self.process.is_none()
                    && !self.unverified_process_record =>
            {
                self.start_now(trigger);
            }
            PendingDurableConvergence::Stop
                if self.runtime.desired_state == DesiredState::Stopped
                    && !self.unverified_process_record =>
            {
                if self.process.is_some() {
                    self.begin_stop(false);
                } else {
                    self.server_state = ServerState::Stopped;
                    self.health_state = HealthState::Unknown;
                    self.last_error = None;
                }
            }
            _ => {}
        }
    }

    pub(super) fn retry_runtime_state_if_due(&mut self, now: Instant) {
        if !self.runtime_state_loaded
            || !self.runtime_state_retry_pending
            || !self.runtime_state_retry_at.is_some_and(|at| now >= at)
        {
            return;
        }
        self.runtime_state_retry_at = None;
        let mut outcome = self.persist_runtime_detailed();
        if outcome == RuntimePersistence::Durable
            && self.launch_pending_clear_requested
            && self.runtime.launch_pending.is_some()
        {
            // A pre-spawn marker whose own save was uncertain never reached
            // spawn. Once its retry is durable, unwind that marker before
            // allowing another launch attempt. If this second save fails,
            // the normal retry gate keeps the marker fail-closed.
            self.runtime.launch_pending = None;
            outcome = self.persist_runtime_detailed();
        }
        if outcome == RuntimePersistence::Durable {
            if let Some(intent) = self.pending_lifecycle_intent.take() {
                // A newer explicit command supersedes a deferred automatic
                // exit transition. Its apply method clears the old pending
                // process-exit action before signaling or spawning.
                self.pending_process_exit = None;
                self.apply_pending_lifecycle_intent(intent);
            } else if let Some((was_explicit_stop, exit)) = self.pending_process_exit.take() {
                self.resume_after_process_exit(was_explicit_stop, exit);
            } else {
                self.resume_pending_durable_convergence();
            }
        }
    }
}
