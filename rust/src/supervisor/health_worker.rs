use crate::config::ValidatedConfig;
use crate::health::{HealthError, HealthResult};
use crate::platform::{LogLevel, log};
use crate::runtime_state::ProcessRecord;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// The identity and configuration facts that authorized one health request.
/// A PID alone is reusable, so the complete recorded process identity travels
/// with every task and is checked again before its result is applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HealthCheckKey {
    generation: u64,
    pid: u32,
    process_group_id: u32,
    start_seconds: u64,
    start_microseconds: u64,
    executable: String,
    config_fingerprint: crate::config_fingerprint::ConfigFingerprint,
}

impl HealthCheckKey {
    pub(crate) fn from_record(
        record: &ProcessRecord,
        config_fingerprint: crate::config_fingerprint::ConfigFingerprint,
    ) -> Self {
        Self {
            generation: 0,
            pid: record.pid,
            process_group_id: record.process_group_id,
            start_seconds: record.start_seconds,
            start_microseconds: record.start_microseconds,
            executable: record.executable.clone(),
            config_fingerprint,
        }
    }

    pub(crate) fn config_fingerprint(&self) -> &crate::config_fingerprint::ConfigFingerprint {
        &self.config_fingerprint
    }

    pub(crate) fn with_generation(mut self, generation: u64) -> Self {
        self.generation = generation;
        self
    }

    pub(crate) fn without_generation(mut self) -> Self {
        self.generation = 0;
        self
    }
}

struct HealthCheckInFlight {
    dispatched: Instant,
    key: HealthCheckKey,
    worker: Option<JoinHandle<()>>,
    receiver: mpsc::Receiver<Result<HealthResult, HealthError>>,
}

/// The completed answer is deliberately only a bounded health summary. The
/// Supervisor owns all state transitions and notifications; this coordinator
/// only owns the worker and its currentness boundary.
pub(crate) struct HealthCheckCompletion {
    pub(crate) key: HealthCheckKey,
    pub(crate) result: Result<HealthResult, HealthError>,
}

/// Single-flight health-check worker. DNS and socket I/O are allowed to block
/// this worker, never the OpenCodeServerAgent supervision event loop.
pub(crate) struct HealthCheckCoordinator {
    in_flight: Option<HealthCheckInFlight>,
    overdue_logged: bool,
    observation_bound: Duration,
    check: fn(&ValidatedConfig, Duration) -> Result<HealthResult, HealthError>,
}

impl HealthCheckCoordinator {
    pub(crate) fn new(
        check: fn(&ValidatedConfig, Duration) -> Result<HealthResult, HealthError>,
        observation_bound: Duration,
    ) -> Self {
        Self {
            in_flight: None,
            overdue_logged: false,
            observation_bound,
            check,
        }
    }

    pub(crate) fn in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    pub(crate) fn overdue_logged(&self) -> bool {
        self.overdue_logged
    }

    /// Dispatches at most one request. The worker owns the cloned validated
    /// configuration, including its in-memory credential, and never logs it.
    pub(crate) fn dispatch(
        &mut self,
        now: Instant,
        key: HealthCheckKey,
        config: ValidatedConfig,
        timeout: Duration,
    ) -> bool {
        if self.in_flight.is_some() {
            return false;
        }

        // Exactly one bounded summary is ever delivered for a task.
        let (sender, receiver) = mpsc::sync_channel(1);
        let check = self.check;
        let worker = thread::Builder::new()
            .name("health-check".to_owned())
            .spawn(move || {
                let result = check(&config, timeout);
                let _ = sender.send(result);
            });
        match worker {
            Ok(worker) => {
                self.in_flight = Some(HealthCheckInFlight {
                    dispatched: now,
                    key,
                    worker: Some(worker),
                    receiver,
                });
                self.overdue_logged = false;
                true
            }
            Err(error) => {
                log(
                    LogLevel::Error,
                    &format!("Unable to start the health-check worker: {error}"),
                );
                false
            }
        }
    }

    /// Polls without waiting. A completed result is returned only when its
    /// task key still matches the currently managed process and configuration.
    pub(crate) fn poll(
        &mut self,
        now: Instant,
        current_key: Option<&HealthCheckKey>,
    ) -> Option<HealthCheckCompletion> {
        let mut in_flight = self.in_flight.take()?;
        match in_flight.receiver.try_recv() {
            Ok(result) => {
                // The worker sends only after the checker returns, so joining
                // here is bounded by thread teardown rather than network I/O.
                join_health_worker(&mut in_flight.worker);
                self.overdue_logged = false;
                if current_key.is_some_and(|current| current == &in_flight.key) {
                    Some(HealthCheckCompletion {
                        key: in_flight.key,
                        result,
                    })
                } else {
                    log(
                        LogLevel::Notice,
                        "Discarded a completed OpenCode health result for a stale process or configuration",
                    );
                    None
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // Do not join a worker whose checker did not produce a result:
                // a platform resolver or security/network call may still be
                // unwinding. Dropping the handle lets OpenCodeServerAgent
                // continue shutting down without waiting for it.
                self.overdue_logged = false;
                log(
                    LogLevel::Error,
                    "OpenCode health-check worker finished without a result",
                );
                None
            }
            Err(mpsc::TryRecvError::Empty) => {
                if !self.overdue_logged
                    && now.duration_since(in_flight.dispatched) > self.observation_bound
                {
                    self.overdue_logged = true;
                    log(
                        LogLevel::Error,
                        "OpenCode health-check worker exceeded its observation bound",
                    );
                }
                self.in_flight = Some(in_flight);
                None
            }
        }
    }
}

fn join_health_worker(worker: &mut Option<JoinHandle<()>>) {
    let Some(worker) = worker.take() else {
        return;
    };
    if worker.join().is_err() {
        log(
            LogLevel::Error,
            "OpenCode health-check worker terminated unexpectedly",
        );
    }
}
