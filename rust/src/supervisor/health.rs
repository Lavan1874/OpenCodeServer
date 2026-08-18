use super::*;
use crate::health;
use crate::supervisor::health_worker::{HealthCheckCompletion, HealthCheckKey};

impl Supervisor {
    fn health_interval(&self) -> Duration {
        match self.server_state {
            ServerState::Starting | ServerState::Unhealthy => STARTING_HEALTH_INTERVAL,
            _ => HEALTH_INTERVAL,
        }
    }

    pub(super) fn health_due(&self, now: Instant) -> Instant {
        self.last_health_check
            .map_or(now, |last| last + self.health_interval())
    }

    /// Builds the currentness key and the configuration authorized for one
    /// health request. A stale attachment is eligible only after its recorded
    /// configuration matches again and its process identity is still intact;
    /// the post-result identity check remains in `apply_stale_health_result`.
    fn current_health_task(&self) -> Option<(HealthCheckKey, ValidatedConfig)> {
        let process = self.process.as_ref()?;
        let record = process.record();
        if let Some(config) = self.active_config.as_ref() {
            let fingerprint = self.config_fingerprint_key.fingerprint(config);
            return Some((
                HealthCheckKey::from_record(record, fingerprint)
                    .with_generation(self.health_generation),
                config.clone(),
            ));
        }
        if !self.stale_config_process {
            return None;
        }
        let config = self.latest_config.as_ref()?;
        if !self
            .config_fingerprint_key
            .verifies(&record.config_fingerprint, config)
        {
            return None;
        }
        match inspect_record_identity(record) {
            Ok(RecordIdentity::Current | RecordIdentity::ExecutableVanished) => Some((
                HealthCheckKey::from_record(record, record.config_fingerprint.clone())
                    .with_generation(self.health_generation),
                config.clone(),
            )),
            _ => None,
        }
    }

    pub(super) fn health_task_is_available(&self) -> bool {
        self.current_health_task().is_some()
    }

    /// Observes the active health task basis at a mutation boundary. The
    /// generation is intentionally not derived from the fingerprint alone:
    /// returning from configuration B to the earlier A must still invalidate
    /// an in-flight A result.
    pub(super) fn sync_health_generation(&mut self) {
        let basis = self
            .current_health_task()
            .map(|(key, _)| key.without_generation());
        if self.health_generation_basis != basis {
            self.health_generation = self.health_generation.saturating_add(1);
            self.health_generation_basis = basis;
        }
    }

    /// Starts a single health request without waiting for DNS or network I/O.
    pub(super) fn dispatch_health_check(&mut self, now: Instant) {
        self.sync_health_generation();
        if !self.runtime_state_ready()
            || matches!(
                self.server_state,
                ServerState::Stopping | ServerState::StopTimedOut
            )
        {
            return;
        }
        let Some((key, config)) = self.current_health_task() else {
            return;
        };
        // This timestamp represents the dispatch attempt. Completion updates
        // it again so a slow worker cannot trigger a catch-up burst.
        self.last_health_check = Some(now);
        let _ = self
            .health_checks
            .dispatch(now, key, config, HEALTH_TIMEOUT);
    }

    /// Polls the worker without waiting, then applies a result only if its
    /// process identity and active configuration still match the task key.
    pub(super) fn poll_health_check(&mut self, now: Instant) {
        self.sync_health_generation();
        let current_key = self.current_health_task().map(|(key, _)| key);
        let Some(completion) = self.health_checks.poll(now, current_key.as_ref()) else {
            return;
        };
        self.last_health_check = Some(now);
        if !self.runtime_state_ready()
            || matches!(
                self.server_state,
                ServerState::Stopping | ServerState::StopTimedOut
            )
        {
            return;
        }
        let Some((current_key, _)) = self.current_health_task() else {
            return;
        };
        if current_key != completion.key {
            return;
        }
        if self.active_config.is_none() && self.stale_config_process {
            self.apply_stale_health_result(now, completion);
        } else {
            self.apply_health_result(now, completion.result);
        }
    }

    /// Applies the established health state machine. Keeping this on the
    /// Supervisor preserves notification, startup allowance, and recovery
    /// semantics while the coordinator owns only I/O and currentness.
    fn apply_health_result(
        &mut self,
        now: Instant,
        result: Result<health::HealthResult, health::HealthError>,
    ) {
        match result {
            Ok(result) if result.healthy => {
                let transitioned = self.health_state != HealthState::Healthy;
                self.health_state = HealthState::Healthy;
                self.server_state = ServerState::Healthy;
                self.health_failures = 0;
                self.last_error = None;
                if self.running_version.as_deref() != Some(&result.version) {
                    self.running_version = Some(result.version.clone());
                    if let Some(process) = self.process.as_mut() {
                        process.record_mut().running_version = Some(result.version);
                        self.runtime.process = Some(process.record().clone());
                        if !self.persist_runtime() {
                            self.server_state = ServerState::Failed;
                            self.health_state = HealthState::Unknown;
                            return;
                        }
                    }
                }
                if self.recovery_incident_active {
                    self.recovery_incident_active = false;
                    self.health_incident_active = false;
                    self.emit_notification(
                        NotificationKind::Recovered,
                        "OpenCode recovered",
                        "OpenCode is healthy again.",
                    );
                } else if self.health_incident_active {
                    self.health_incident_active = false;
                    self.emit_notification(
                        NotificationKind::Recovered,
                        "OpenCode health recovered",
                        "The health endpoint is responding normally again.",
                    );
                }
                if transitioned {
                    let elapsed = self
                        .process_started
                        .map(|started| now.duration_since(started).as_secs_f64())
                        .unwrap_or_default();
                    log(
                        LogLevel::Notice,
                        &format!("OpenCode health changed to healthy {elapsed:.1}s after spawn"),
                    );
                }
            }
            other => {
                let unauthorized = matches!(other, Err(health::HealthError::Unauthorized));
                self.health_failures = self.health_failures.saturating_add(1);
                self.health_state = HealthState::Unhealthy;
                let still_starting = self
                    .process_started
                    .is_some_and(|started| now.duration_since(started) < STARTUP_HEALTH_ALLOWANCE)
                    && self.server_state == ServerState::Starting;
                if still_starting
                    && (self.health_failures == 1 || self.health_failures.is_multiple_of(5))
                {
                    let reason = match &other {
                        Ok(_) => "endpoint reported unhealthy".to_owned(),
                        Err(error) => error.to_string(),
                    };
                    let elapsed = self
                        .process_started
                        .map(|started| now.duration_since(started).as_secs_f64())
                        .unwrap_or_default();
                    log(
                        LogLevel::Notice,
                        &format!(
                            "OpenCode is not yet healthy {elapsed:.1}s after spawn (check {}): {reason}",
                            self.health_failures
                        ),
                    );
                }
                if !still_starting {
                    self.server_state = ServerState::Unhealthy;
                    self.last_error = Some(if unauthorized {
                        unauthorized_credential_message(self.credentials.state()).to_owned()
                    } else {
                        "OpenCode is running, but /global/health is not healthy".to_owned()
                    });
                    if self.health_failures >= 3 && !self.health_incident_active {
                        self.health_incident_active = true;
                        self.emit_notification(
                            NotificationKind::Failure,
                            "OpenCode is not healthy",
                            if unauthorized {
                                "The stored password was rejected (HTTP 401). Re-save the password \
                                 in Settings and restart OpenCode; the process was left running."
                            } else {
                                "The process is still running; OpenCodeServer will not restart it solely because a health check failed."
                            },
                        );
                        log(
                            LogLevel::Error,
                            "OpenCode health failed repeatedly; process was left running",
                        );
                    }
                }
            }
        }
    }

    /// Completes the stale-attachment convergence path without running its
    /// authenticated request on the event loop. Identity is checked again
    /// after a healthy answer before the process becomes fully attached.
    fn apply_stale_health_result(&mut self, now: Instant, completion: HealthCheckCompletion) {
        let Ok(result) = completion.result else {
            return;
        };
        if !result.healthy {
            return;
        }
        let Some(process) = self.process.as_ref() else {
            return;
        };
        let record = process.record().clone();
        let Some(config) = self.latest_config.clone() else {
            return;
        };
        if !self
            .config_fingerprint_key
            .verifies(completion.key.config_fingerprint(), &config)
        {
            return;
        }
        match inspect_record_identity(&record) {
            Ok(RecordIdentity::Current | RecordIdentity::ExecutableVanished) => {}
            _ => return,
        }
        self.running_version = Some(result.version.clone());
        let mut record = record;
        record.running_version = Some(result.version);
        record.config_fingerprint = self.config_fingerprint_key.fingerprint(&config);
        self.process = Some(ManagedProcess::attach(record.clone()));
        self.runtime.process = Some(record);
        self.active_config = Some(config);
        self.stale_config_process = false;
        self.server_state = ServerState::Healthy;
        self.health_state = HealthState::Healthy;
        self.last_error = None;
        self.process_started = Some(now);
        if !self.persist_runtime() {
            self.server_state = ServerState::Failed;
            self.health_state = HealthState::Unknown;
        }
        log(
            LogLevel::Notice,
            "OpenCode configuration matches again; stale attachment upgraded to full supervision",
        );
    }
}
