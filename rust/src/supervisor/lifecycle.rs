use super::*;
use crate::runtime_state::LaunchPending;

impl Supervisor {
    /// Why an explicit start/restart cannot proceed, in user-actionable
    /// terms. Automatic flows (boot, recovery) keep the silent `last_error`
    /// path in `start_now`; explicit commands return this reason as an IPC
    /// error so a menu click is never a silent no-op.
    pub(super) fn start_refusal(&self) -> Option<String> {
        if self.unverified_process_record && self.process.is_none() {
            return Some(
                "An unverified OpenCode process is still running — quit it manually, then start again"
                    .to_owned(),
            );
        }
        if self.credentials.state() == CredentialState::AccessPending {
            return Some(
                "Keychain access not granted — open Settings, choose “Allow Keychain Access…”, then start"
                    .to_owned(),
            );
        }
        None
    }

    pub(super) fn request_start(&mut self) -> io::Result<()> {
        self.persist_desired_state(DesiredState::Running, PendingLifecycleIntent::Start)?;
        self.apply_start()
    }

    fn apply_start(&mut self) -> io::Result<()> {
        self.pending_process_exit = None;
        self.pending_durable_convergence = None;
        if self.process.is_some() {
            return Ok(());
        }
        if let Some(reason) = self.start_refusal() {
            self.last_error = Some(reason.clone());
            self.server_state = ServerState::Failed;
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, reason));
        }
        self.restart_attempt_index = 0;
        self.next_restart = None;
        self.next_port_retry = None;
        self.port_release_deadline = None;
        self.network_wait_deadline = None;
        self.pending_start_trigger = None;
        self.start_now(StartTrigger::Cold);
        if let Some(error) = self.runtime_state_error.clone() {
            return Err(io::Error::other(error));
        }
        Ok(())
    }

    pub(super) fn request_stop(&mut self) -> io::Result<()> {
        self.persist_desired_state(DesiredState::Stopped, PendingLifecycleIntent::Stop)?;
        self.apply_stop()
    }

    fn apply_stop(&mut self) -> io::Result<()> {
        self.pending_process_exit = None;
        self.pending_durable_convergence = None;
        self.restart_after_stop = false;
        self.group_cleanup_recovery = false;
        self.next_restart = None;
        self.next_port_retry = None;
        self.port_release_deadline = None;
        self.network_wait_deadline = None;
        self.pending_start_trigger = None;
        self.begin_stop(false);
        if self.runtime.launch_pending.is_some() || self.launch_pending_clear_requested {
            return Err(io::Error::other(
                "An earlier OpenCode launch was not durably finalized; Stop cannot claim it was stopped",
            ));
        }
        Ok(())
    }

    pub(super) fn request_restart(&mut self) -> io::Result<()> {
        self.persist_desired_state(DesiredState::Running, PendingLifecycleIntent::Restart)?;
        self.apply_restart()
    }

    fn apply_restart(&mut self) -> io::Result<()> {
        self.pending_process_exit = None;
        self.pending_durable_convergence = None;
        self.group_cleanup_recovery = false;
        self.restart_after_stop = self.process.is_some();
        self.next_restart = None;
        self.next_port_retry = None;
        self.port_release_deadline = None;
        self.network_wait_deadline = None;
        self.pending_start_trigger = None;
        self.restart_attempt_index = 0;
        if self.process.is_some() {
            if let Some(reason) = self.start_refusal() {
                self.restart_after_stop = false;
                self.last_error = Some(reason.clone());
                self.server_state = ServerState::Failed;
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, reason));
            }
            self.begin_stop(true);
        } else if let Some(reason) = self.start_refusal() {
            self.last_error = Some(reason.clone());
            self.server_state = ServerState::Failed;
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, reason));
        } else {
            self.start_now(StartTrigger::AfterStop);
            if let Some(error) = self.runtime_state_error.clone() {
                return Err(io::Error::other(error));
            }
        }
        Ok(())
    }

    pub(super) fn apply_pending_lifecycle_intent(&mut self, intent: PendingLifecycleIntent) {
        match intent {
            PendingLifecycleIntent::Start => {
                let _ = self.apply_start();
            }
            PendingLifecycleIntent::Stop => {
                let _ = self.apply_stop();
            }
            PendingLifecycleIntent::Restart => {
                let _ = self.apply_restart();
            }
        }
    }

    pub(super) fn begin_stop(&mut self, restarting: bool) {
        if self.runtime.launch_pending.is_some() || self.launch_pending_clear_requested {
            if self.launch_pending_clear_requested {
                if self.process.is_some() {
                    self.defer_lifecycle_intent(if restarting {
                        PendingLifecycleIntent::Restart
                    } else {
                        PendingLifecycleIntent::Stop
                    });
                } else {
                    self.defer_durable_convergence(if restarting {
                        PendingDurableConvergence::Start(StartTrigger::AfterStop)
                    } else {
                        PendingDurableConvergence::Stop
                    });
                }
            }
            // A prior launch transaction has not reached a durable final
            // state. Defer Stop/Restart until its marker or process identity
            // record is durable; do not signal or claim a stop in between.
            self.server_state = ServerState::Failed;
            self.health_state = HealthState::Unknown;
            self.last_error = Some(
                "An earlier OpenCode launch was not durably finalized — stop the possible process manually, then repair runtime state"
                    .to_owned(),
            );
            log(
                LogLevel::Fault,
                "Refused to claim a stop while an OpenCode launch transaction remains unresolved",
            );
        } else if let Some(process) = &self.process {
            match process.send_terminate() {
                Ok(()) => {
                    self.server_state = ServerState::Stopping;
                    self.health_state = HealthState::Unknown;
                    self.stop_deadline = Some(Instant::now() + GRACEFUL_STOP);
                    self.restart_after_stop = restarting;
                    self.last_error = None;
                    log(
                        LogLevel::Notice,
                        "Sent SIGTERM to the OpenCode process group",
                    );
                }
                Err(error) => {
                    self.server_state = ServerState::Failed;
                    self.last_error = Some(error.to_string());
                    log(
                        LogLevel::Fault,
                        "Refused to stop OpenCode because process identity validation failed",
                    );
                }
            }
        } else if self.unverified_process_record {
            // The record cannot authorize a signal (its identity was never
            // confirmed, or could not be re-verified); claiming "Stopped"
            // while an unknown process may still run would be dishonest.
            self.server_state = ServerState::Failed;
            self.last_error = Some(
                "The existing OpenCode identity was never confirmed — stop it manually, then start again"
                    .to_owned(),
            );
            log(
                LogLevel::Fault,
                "Refused to claim a stop of an unverified existing process",
            );
        } else if restarting {
            self.start_now(StartTrigger::AfterStop);
        } else {
            self.server_state = ServerState::Stopped;
            self.health_state = HealthState::Unknown;
            self.last_error = None;
        }
    }

    pub(super) fn continue_stop(&mut self) -> io::Result<()> {
        if self.server_state != ServerState::StopTimedOut {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "OpenCode is not waiting after a graceful-stop timeout",
            ));
        }
        self.server_state = ServerState::Stopping;
        self.stop_deadline = Some(Instant::now() + GRACEFUL_STOP);
        self.last_error = None;
        Ok(())
    }

    pub(super) fn force_stop(&mut self) -> io::Result<()> {
        if !matches!(
            self.server_state,
            ServerState::Stopping | ServerState::StopTimedOut
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "force stop is available only after a stop or restart request",
            ));
        }
        let process = self
            .process
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "OpenCode is not running"))?;
        process.send_kill()?;
        self.server_state = ServerState::Stopping;
        self.stop_deadline = None;
        self.last_error = None;
        log(
            LogLevel::Error,
            "Explicit user request sent SIGKILL to the OpenCode process group",
        );
        Ok(())
    }

    pub(super) fn start_now(&mut self, trigger: StartTrigger) {
        if !self.runtime_state_ready() {
            self.defer_durable_convergence(PendingDurableConvergence::Start(trigger));
            self.server_state = ServerState::Failed;
            self.health_state = HealthState::Unknown;
            return;
        }
        self.refresh_config();
        let Some(config) = self.latest_config.clone() else {
            self.server_state = ServerState::Failed;
            self.last_error = self.config_error.clone();
            if trigger == StartTrigger::Recovery {
                self.schedule_next_restart();
            }
            return;
        };
        // A password may exist that this binary is not yet authorized to
        // read. Spawning without it would silently start OpenCode with no
        // authentication, so the start is refused with an actionable error
        // instead of downgrading; the Settings "Allow Keychain Access…" flow both
        // grants and resumes a pending start.
        if self.credentials.state() == CredentialState::AccessPending {
            if self.credentials.refresh_in_flight() {
                // A marker-permitted decrypt is already converging on the
                // worker; `apply_credential_read` resumes this start. Do
                // not flap a refusal error for a state that resolves
                // itself within milliseconds in the common case.
                log(
                    LogLevel::Info,
                    "OpenCode start waits for the in-flight Keychain credential read",
                );
                return;
            }
            self.server_state = ServerState::Failed;
            self.health_state = HealthState::Unknown;
            self.last_error = Some(
                "Keychain access not granted — open Settings, choose “Allow Keychain Access…”, then start"
                    .to_owned(),
            );
            log(
                LogLevel::Error,
                "OpenCode start refused: Keychain credential access is pending authorization",
            );
            if trigger == StartTrigger::Recovery {
                self.schedule_next_restart();
            }
            return;
        }
        let config_fingerprint = self.config_fingerprint_key.fingerprint(&config);
        let launch_pending = LaunchPending {
            executable: config.configured_executable.to_string_lossy().into_owned(),
            config_fingerprint: config_fingerprint.clone(),
        };
        if !self.begin_launch(launch_pending) {
            if self.runtime.launch_pending.is_some()
                && self.launch_pending_clear_requested
                && self.runtime_state_retry_pending
            {
                self.pending_durable_convergence = Some(PendingDurableConvergence::Start(trigger));
            }
            self.server_state = ServerState::Failed;
            self.health_state = HealthState::Unknown;
            return;
        }
        // The installed version is informational and arrives asynchronously
        // (see ADR 0009); process identity never depends on it, so spawning
        // must not wait for the version query subprocess.
        match ManagedProcess::spawn(&config, None, config_fingerprint) {
            Ok(process) => {
                let persisted = self.commit_spawned_process(process);
                self.active_config = Some(config);
                self.running_version = None;
                self.server_state = ServerState::Starting;
                self.health_state = HealthState::Unknown;
                self.health_failures = 0;
                self.last_health_check = None;
                self.process_started = Some(Instant::now());
                self.group_cleanup_recovery = false;
                self.next_port_retry = None;
                self.port_release_deadline = None;
                self.network_wait_deadline = None;
                self.pending_start_trigger = None;
                if !persisted {
                    self.server_state = ServerState::Failed;
                    self.health_state = HealthState::Unknown;
                    log(
                        LogLevel::Fault,
                        "OpenCode started but its process identity could not be persisted; it remains supervised",
                    );
                    return;
                }
                self.last_error = None;
                log(
                    LogLevel::Notice,
                    "Started OpenCode in a dedicated process group",
                );
            }
            Err(spawn_error) => {
                let (error, survivor) = spawn_error.into_parts();
                if let Some(child) = survivor {
                    // The child refused the graceful stop inside the bounded
                    // unregistered-stop window. Ownership rule (P1-2): never
                    // drop an unconfirmed child and never SIGKILL it
                    // automatically — keep it as a supervised,
                    // health-check-skipped process so there is no unmanaged
                    // OpenCode instance and no second instance can be
                    // spawned. The user can stop it explicitly; the
                    // supervisor still reaps it if it exits on its own.
                    let persisted = self.commit_spawned_process(child);
                    self.active_config = None;
                    self.server_state = ServerState::Failed;
                    self.health_state = HealthState::Unknown;
                    self.process_started = Some(Instant::now());
                    if persisted {
                        self.last_error = Some(
                            "OpenCode failed identity confirmation after launch — it remains running under supervision"
                                .to_owned(),
                        );
                    }
                    log(
                        LogLevel::Fault,
                        &format!(
                            "OpenCode failed identity confirmation and is kept under supervision: {error}"
                        ),
                    );
                    return;
                }
                if !self.abort_launch() {
                    self.server_state = ServerState::Failed;
                    self.health_state = HealthState::Unknown;
                    self.next_port_retry = None;
                    self.port_release_deadline = None;
                    self.network_wait_deadline = None;
                    self.pending_start_trigger = None;
                    log(
                        LogLevel::Fault,
                        "OpenCode startup failed and the launch marker could not be cleared; retry is blocked",
                    );
                    return;
                }
                if error.kind() == io::ErrorKind::AddrInUse
                    && trigger == StartTrigger::AfterStop
                    && self.schedule_port_release_retry()
                {
                    return;
                }
                if error.kind() == io::ErrorKind::AddrNotAvailable
                    && self.schedule_network_wait_retry(trigger)
                {
                    return;
                }
                self.next_port_retry = None;
                self.port_release_deadline = None;
                self.network_wait_deadline = None;
                self.pending_start_trigger = None;
                self.last_error = Some(if error.kind() == io::ErrorKind::AddrInUse {
                    format!(
                        "Port conflict: endpoint {} is already in use — no process was terminated",
                        config.endpoint()
                    )
                } else {
                    format!("OpenCode could not be started: {error}")
                });
                self.server_state = ServerState::Failed;
                self.health_state = HealthState::Unknown;
                log(LogLevel::Error, "OpenCode startup failed");
                match trigger {
                    StartTrigger::Recovery if error.kind() != io::ErrorKind::AddrInUse => {
                        self.schedule_next_restart();
                    }
                    StartTrigger::Recovery => self.finish_recovery_failure(),
                    _ => {}
                }
            }
        }
    }

    /// Rides out the brief window in which our own just-stopped OpenCode
    /// still holds the configured endpoint (process-group teardown and, on
    /// TIME_WAIT sockets). Returns true when a retry was
    /// scheduled; false once the budget expired, leaving the caller to
    /// surface the port-conflict failure unchanged.
    fn schedule_port_release_retry(&mut self) -> bool {
        let now = Instant::now();
        let deadline = self
            .port_release_deadline
            .get_or_insert(now + PORT_RELEASE_RETRY_BUDGET);
        if now >= *deadline {
            return false;
        }
        self.next_port_retry = Some(now + PORT_RELEASE_RETRY_INTERVAL);
        self.server_state = ServerState::WaitingToRestart;
        self.health_state = HealthState::Unknown;
        self.last_error = None;
        log(
            LogLevel::Info,
            "Configured endpoint is not yet released by the previous OpenCode; retrying startup shortly",
        );
        true
    }

    /// Rides out the window in which the configured endpoint address is not
    /// yet assigned locally — at boot the agent can run before configd has
    /// applied the static address (ADR 0013). Returns true while the budget
    /// lasts, keeping the original trigger for the retry; false once it
    /// expires, leaving the caller to surface the start failure unchanged.
    fn schedule_network_wait_retry(&mut self, trigger: StartTrigger) -> bool {
        let now = Instant::now();
        let deadline = self
            .network_wait_deadline
            .get_or_insert(now + self.options.network_wait_budget);
        if now >= *deadline {
            return false;
        }
        self.pending_start_trigger = Some(trigger);
        self.next_port_retry = Some(now + PORT_RELEASE_RETRY_INTERVAL);
        self.server_state = ServerState::WaitingToRestart;
        self.health_state = HealthState::Unknown;
        self.last_error = None;
        log(
            LogLevel::Notice,
            "Configured endpoint address is not assigned yet; waiting for the network before starting OpenCode",
        );
        true
    }
}
