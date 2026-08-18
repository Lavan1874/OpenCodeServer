use super::*;
use crate::version_query::{VersionQueryResult, query_event, query_generation};
use std::path::Path;

/// The installed-version query mechanism extracted from `Supervisor` (see
/// `docs/refactor/version-query-coordinator-boundary.md`). Owns the
/// single-flight worker, its retry cadence, the overdue latch, and the
/// quarantine circuit breaker; product data and side effects stay on the
/// Supervisor side and cross the boundary explicitly.
pub(crate) struct VersionQueryCoordinator {
    version_query: Option<VersionQueryInFlight>,
    version_query_overdue_logged: bool,
    last_version_attempt: Option<Instant>,
    /// An identity anomaly disables repeated informational queries for the
    /// same executable until configuration changes or this Agent restarts.
    version_query_quarantined_executable: Option<PathBuf>,
    query: fn(&Path, Duration, String) -> VersionQueryResult,
}

impl VersionQueryCoordinator {
    pub(crate) fn new(query: fn(&Path, Duration, String) -> VersionQueryResult) -> Self {
        Self {
            version_query: None,
            version_query_overdue_logged: false,
            last_version_attempt: None,
            version_query_quarantined_executable: None,
            query,
        }
    }
}

#[cfg(test)]
impl VersionQueryCoordinator {
    pub(super) fn in_flight_for_test(&self) -> &Option<VersionQueryInFlight> {
        &self.version_query
    }
    pub(super) fn set_in_flight_for_test(&mut self, in_flight: VersionQueryInFlight) {
        self.version_query = Some(in_flight);
    }
    pub(super) fn overdue_logged_for_test(&self) -> bool {
        self.version_query_overdue_logged
    }
    pub(super) fn set_overdue_logged_for_test(&mut self, value: bool) {
        self.version_query_overdue_logged = value;
    }
    pub(super) fn last_attempt_for_test(&self) -> Option<Instant> {
        self.last_version_attempt
    }
    pub(super) fn set_last_attempt_for_test(&mut self, attempt: Option<Instant>) {
        self.last_version_attempt = attempt;
    }
    pub(super) fn quarantined_for_test(&self) -> Option<&PathBuf> {
        self.version_query_quarantined_executable.as_ref()
    }
    pub(super) fn set_quarantined_for_test(&mut self, executable: Option<PathBuf>) {
        self.version_query_quarantined_executable = executable;
    }
}

/// The result of one completed installed-version query, reported to the
/// Supervisor for the product-state application the controller must not own.
pub(crate) enum VersionQueryOutcome {
    /// A version was read; the Supervisor applies it (and backfills the
    /// running version).
    Available(String),
    /// The query finished without a version.
    Unavailable,
    /// The worker observed a process identity anomaly; the Supervisor clears
    /// the informational installed version while the controller opens the
    /// circuit breaker for the executable it queried.
    Quarantined,
}

impl VersionQueryCoordinator {
    /// Whether a query is currently in flight. `next_deadline` schedules the
    /// recheck poll and the shutdown drain loop uses it as its condition.
    pub(crate) fn in_flight(&self) -> bool {
        self.version_query.is_some()
    }

    /// Whether a new query is due: a configured executable exists, the
    /// quarantine breaker does not cover it, and the retry/refresh interval
    /// since the last attempt has elapsed. `has_version` selects the
    /// interval — the Supervisor signals whether an installed version is
    /// already known instead of the controller reading it.
    pub(crate) fn due(&self, now: Instant, executable: Option<&Path>, has_version: bool) -> bool {
        let Some(executable) = executable else {
            return false;
        };
        if self
            .version_query_quarantined_executable
            .as_ref()
            .is_some_and(|quarantined| quarantined == executable)
        {
            return false;
        }
        let interval = if has_version {
            VERSION_INTERVAL
        } else {
            VERSION_RETRY_INTERVAL
        };
        self.last_version_attempt
            .is_none_or(|last| now.duration_since(last) >= interval)
    }

    /// Runs the informational installed-version query off the event loop. One
    /// worker owns one direct child and reports only after reaping it. An
    /// observed process-group escape or identity-inspection failure opens a
    /// circuit breaker for this executable, preventing a five-second respawn
    /// loop around a behavior the product does not support.
    pub(crate) fn poll_version_query(
        &mut self,
        now: Instant,
        executable: Option<&Path>,
        timeout: Duration,
        has_version: bool,
    ) -> Option<VersionQueryOutcome> {
        let Some(mut in_flight) = self.version_query.take() else {
            let executable = executable?;
            if !self.due(now, Some(executable), has_version) {
                return None;
            }
            let executable = executable.to_path_buf();
            let generation = query_generation();
            let (sender, receiver) = mpsc::channel();
            query_event(&executable, "single-flight-acquire", &generation);
            let worker_generation = generation.clone();
            let worker_executable = executable.clone();
            let query = self.query;
            let worker = thread::Builder::new()
                .name("version-query".to_owned())
                .spawn(move || {
                    let outcome = query(&worker_executable, timeout, worker_generation);
                    let _ = sender.send(outcome);
                });
            match worker {
                Ok(worker) => {
                    self.version_query = Some(VersionQueryInFlight {
                        dispatched: now,
                        generation,
                        executable,
                        worker: Some(worker),
                        receiver,
                    });
                }
                Err(error) => {
                    query_event(&executable, "single-flight-release", &generation);
                    log(
                        LogLevel::Error,
                        &format!("Unable to start the installed-version query worker: {error}"),
                    );
                    self.last_version_attempt = Some(now);
                }
            }
            return None;
        };

        match in_flight.receiver.try_recv() {
            Ok(outcome) => {
                join_version_query_worker(&mut in_flight.worker);
                query_event(
                    &in_flight.executable,
                    "single-flight-release",
                    &in_flight.generation,
                );
                self.version_query_overdue_logged = false;

                let still_current =
                    executable.is_some_and(|current| *current == in_flight.executable);
                if !still_current {
                    self.last_version_attempt = Some(now);
                    return None;
                }

                let outcome = match outcome {
                    VersionQueryResult::Available(version) => {
                        self.version_query_quarantined_executable = None;
                        self.last_version_attempt = Some(Instant::now());
                        VersionQueryOutcome::Available(version)
                    }
                    VersionQueryResult::Unavailable => {
                        self.last_version_attempt = Some(Instant::now());
                        VersionQueryOutcome::Unavailable
                    }
                    VersionQueryResult::Quarantined => {
                        self.last_version_attempt = Some(now);
                        self.version_query_quarantined_executable =
                            Some(in_flight.executable.clone());
                        VersionQueryOutcome::Quarantined
                    }
                };
                Some(outcome)
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                join_version_query_worker(&mut in_flight.worker);
                query_event(
                    &in_flight.executable,
                    "single-flight-release",
                    &in_flight.generation,
                );
                self.last_version_attempt = Some(now);
                self.version_query_overdue_logged = false;
                None
            }
            Err(mpsc::TryRecvError::Empty) => {
                if !self.version_query_overdue_logged
                    && now.duration_since(in_flight.dispatched) > timeout.saturating_mul(2)
                {
                    self.version_query_overdue_logged = true;
                    log(
                        LogLevel::Error,
                        "Installed-version query worker exceeded twice its observation bound",
                    );
                }
                self.version_query = Some(in_flight);
                None
            }
        }
    }
}

impl Supervisor {
    /// Thin delegator: computes the boundary inputs (configured executable,
    /// query timeout, whether a version is already known) and applies the
    /// returned outcome to the product state.
    pub(super) fn poll_version_query(&mut self, now: Instant) {
        let executable = self
            .latest_config
            .as_ref()
            .map(|config| config.configured_executable.as_path());
        let timeout = self.options.version_query_timeout;
        let has_version = self.installed_version.is_some();
        let outcome =
            self.version_queries
                .poll_version_query(now, executable, timeout, has_version);
        let Some(outcome) = outcome else {
            return;
        };
        match outcome {
            VersionQueryOutcome::Available(version) => self.apply_installed_version(Some(version)),
            VersionQueryOutcome::Unavailable => self.apply_installed_version(None),
            VersionQueryOutcome::Quarantined => {
                self.installed_version = None;
                log(
                    LogLevel::Fault,
                    "Installed-version queries were disabled for the configured executable after a process identity anomaly",
                );
            }
        }
    }

    /// OpenCodeServerAgent must not drop a live version-query receiver while
    /// its worker still owns a Child. Drain the query-owned state before the
    /// agent exits so a normal shutdown cannot detach the worker or leave a
    /// pending Child for launchd to inherit. The managed OpenCode is not
    /// touched by this method.
    pub fn finish_version_query_for_shutdown(&mut self) {
        while self.version_queries.in_flight() {
            self.poll_version_query(Instant::now());
            if self.version_queries.in_flight() {
                thread::sleep(VERSION_IN_FLIGHT_RECHECK);
            }
        }
    }

    pub(super) fn version_query_due(&self, now: Instant) -> bool {
        let executable = self
            .latest_config
            .as_ref()
            .map(|config| config.configured_executable.as_path());
        let has_version = self.installed_version.is_some();
        self.version_queries.due(now, executable, has_version)
    }

    fn apply_installed_version(&mut self, version: Option<String>) {
        let Some(version) = version else {
            return;
        };
        self.installed_version = Some(version.clone());
        if self.running_version.is_none() {
            self.running_version = Some(version.clone());
        }
        if let Some(process) = self.process.as_mut()
            && process.record().running_version.is_none()
        {
            process.record_mut().running_version = Some(version);
            self.runtime.process = Some(process.record().clone());
            if !self.persist_runtime() {
                self.server_state = ServerState::Failed;
                self.health_state = HealthState::Unknown;
            }
        }
    }
}
