use crate::VERSION;
use crate::config::{ValidatedConfig, load_and_validate, load_or_create, validation_report};
use crate::config_fingerprint::ConfigFingerprintKey;
use crate::credential_grant::CredentialGrant;
use crate::keychain::{KeychainProbe, KeychainRead};
use crate::paths::AppPaths;
use crate::platform::{LogLevel, log};
use crate::process::{
    ExitReason, ManagedProcess, RecordIdentity, authorized_process_group_has_members,
    inspect_record_identity,
};
use crate::protocol::{
    ActionCapabilities, Command, DesiredState, FdaState, HealthState, NotificationEvent,
    NotificationKind, PROTOCOL_VERSION, PasswordState, Response, ServerState, Status,
};
use crate::runtime_state::RuntimeState;
use crate::version_query::VersionQueryResult;
#[cfg(any(test, feature = "test-fixture"))]
pub use crate::version_query::{
    query_installed_version_for_test as query_installed_version,
    query_installed_version_with_for_test as query_installed_version_with,
    query_installed_version_with_snapshot_for_test as query_installed_version_with_snapshot,
};
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const HEALTH_INTERVAL: Duration = Duration::from_secs(3);
const STARTING_HEALTH_INTERVAL: Duration = Duration::from_secs(1);
const CONFIG_RECHECK_INTERVAL: Duration = Duration::from_secs(60);
const VERSION_INTERVAL: Duration = Duration::from_secs(60);
const VERSION_RETRY_INTERVAL: Duration = Duration::from_secs(5);
/// Default observation bound for one informational installed-version query.
const DEFAULT_VERSION_QUERY_TIMEOUT: Duration = Duration::from_secs(30);
const VERSION_IN_FLIGHT_RECHECK: Duration = Duration::from_millis(200);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const HEALTH_IN_FLIGHT_RECHECK: Duration = Duration::from_millis(200);
const HEALTH_OBSERVATION_BOUND: Duration = Duration::from_secs(10);
/// Once a health worker has exceeded its observation bound, keep polling at a
/// relaxed cadence. This preserves eventual convergence without waking the
/// otherwise idle supervisor five times a second forever on a stuck resolver.
const HEALTH_OVERDUE_RECHECK: Duration = Duration::from_secs(5);
const GRACEFUL_STOP: Duration = Duration::from_secs(15);
const STARTUP_HEALTH_ALLOWANCE: Duration = Duration::from_secs(15);
const STABLE_RUN_INTERVAL: Duration = Duration::from_secs(300);
/// Interval for re-checking whether an unverified process PID has
/// disappeared. The supervisor is in `Failed` state during this period;
/// the check lets it self-converge when the PID is provably gone without
/// requiring an OpenCodeServerAgent restart or manual repair.
const UNVERIFIED_CHECK_INTERVAL: Duration = Duration::from_secs(3);
/// After an explicit stop, the just-terminated OpenCode can still hold the
/// configured endpoint for a few hundred milliseconds while its process
/// finishes exiting. Startup retries ride out that window; a genuine
/// foreign listener still fails once the budget expires.
const PORT_RELEASE_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const PORT_RELEASE_RETRY_BUDGET: Duration = Duration::from_secs(10);
/// Poll cadence for an in-flight decrypt-class Keychain read (the system
/// consent dialog is answered asynchronously by the user).
const CREDENTIAL_REFRESH_RECHECK: Duration = Duration::from_millis(200);
/// A decrypt-class Keychain read blocks its worker on the system consent
/// dialog with no documented timeout; log once if the user leaves the dialog
/// pending this long.
const CREDENTIAL_REFRESH_OVERDUE: Duration = Duration::from_secs(300);
/// Default budget for waiting on the configured endpoint address to be
/// assigned locally (boot-time race with configd; see ADR 0013).
const DEFAULT_NETWORK_WAIT_BUDGET: Duration = Duration::from_secs(60);
const RESTART_BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartTrigger {
    /// User request, or agent boot with `DesiredState::Running`.
    Cold,
    /// Bounded automatic recovery after an unexpected exit.
    Recovery,
    /// Respawn after our own explicit stop; the just-stopped server may
    /// still be releasing the configured endpoint for a moment.
    AfterStop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PendingLifecycleIntent {
    Start,
    Stop,
    Restart,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingDurableConvergence {
    Start(StartTrigger),
    Stop,
}

/// Per-instance timing budgets. Production uses `SupervisorOptions::default`;
/// the integration tests construct smaller budgets through
/// `Supervisor::with_options`, so test tuning never depends on mutable
/// process-global environment shared by parallel tests.
#[derive(Clone, Debug)]
pub struct SupervisorOptions {
    /// How long startup waits for the configured endpoint address to be
    /// assigned locally (ADR 0013) before declaring failure.
    pub network_wait_budget: Duration,
    /// Observation bound for one installed-version query. The worker enforces
    /// it on the real subprocess and reaps its direct child before returning.
    pub version_query_timeout: Duration,
}

struct VersionQueryInFlight {
    dispatched: Instant,
    generation: String,
    executable: PathBuf,
    worker: Option<JoinHandle<()>>,
    receiver: mpsc::Receiver<VersionQueryResult>,
}

/// Whether the OpenCode password stored in the login keychain is usable by
/// this agent. `AccessPending` is a soft state: an item may exist but the
/// user has not (yet) granted this binary access; it must never be treated
/// as "no password configured" or as a reason to delete anything.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CredentialState {
    NotConfigured,
    AccessPending,
    Available,
}

/// One decrypt-class Keychain read running on its own worker thread. The
/// read blocks inside `SecItemCopyMatching` until the user answers the
/// system consent dialog (macOS 26 offers no query key that suppresses the
/// dialog for legacy-keychain items), so it must never run on the event
/// loop; the dialog is only ever raised by the explicit Settings
/// "Allow Keychain Access…" action.
struct CredentialRefreshInFlight {
    dispatched: Instant,
    /// The account the read was issued for. The configuration may move on
    /// while the dialog is pending; a result for a stale account is
    /// discarded rather than merged into the wrong item's configuration.
    account: String,
    worker: Option<JoinHandle<()>>,
    receiver: mpsc::Receiver<KeychainRead>,
}

impl Default for SupervisorOptions {
    fn default() -> Self {
        Self {
            network_wait_budget: DEFAULT_NETWORK_WAIT_BUDGET,
            version_query_timeout: DEFAULT_VERSION_QUERY_TIMEOUT,
        }
    }
}

pub struct Supervisor {
    paths: AppPaths,
    config_fingerprint_key: ConfigFingerprintKey,
    options: SupervisorOptions,
    agent_started: Instant,
    runtime: RuntimeState,
    /// A missing state file is a valid first-run condition, but a file that
    /// exists and cannot be read is not. Keep that distinction so startup can
    /// remain diagnostically reachable without treating an unreadable record
    /// as "no process" and launching a second OpenCode.
    runtime_state_loaded: bool,
    /// Whether the in-memory runtime state is known to match a successful
    /// atomic save. A failed save or an unsynced rename keeps lifecycle
    /// actions behind this gate until a later save succeeds.
    runtime_state_reliable: bool,
    runtime_state_retry_pending: bool,
    runtime_state_retry_at: Option<Instant>,
    runtime_state_error: Option<String>,
    /// A failed durable clear of the pre-spawn launch marker must be retried
    /// before another OpenCode launch is considered safe.
    launch_pending_clear_requested: bool,
    /// The child has exited, but the process-record clear crossed a rename
    /// whose parent-directory sync was uncertain. Keep the exit transition in
    /// memory until a later durable retry can resume notification/recovery.
    pending_process_exit: Option<(bool, ExitReason)>,
    /// The requested lifecycle action is waiting for a durable desired-state
    /// write or launch-marker unwind. A later durable retry consumes this
    /// one-shot intent; a newer explicit command replaces it only when its
    /// own desired-state write succeeds or becomes the new uncertain intent.
    pending_lifecycle_intent: Option<PendingLifecycleIntent>,
    /// A post-durability lifecycle action. It is consumed once only after a
    /// retry confirms that the state clear or launch-marker unwind is safe.
    /// A newer explicit command cancels it.
    pending_durable_convergence: Option<PendingDurableConvergence>,
    process: Option<ManagedProcess>,
    latest_config: Option<ValidatedConfig>,
    active_config: Option<ValidatedConfig>,
    server_state: ServerState,
    health_state: HealthState,
    fda_state: FdaState,
    installed_version: Option<String>,
    running_version: Option<String>,
    config_error: Option<String>,
    last_error: Option<String>,
    notification: Option<NotificationEvent>,
    last_health_check: Option<Instant>,
    /// Monotonic task generation. It invalidates completed health results
    /// across process/config changes, including an A -> B -> A cycle.
    health_generation: u64,
    health_generation_basis: Option<health_worker::HealthCheckKey>,
    last_config_check: Instant,
    credentials: credential::CredentialController,
    version_queries: version::VersionQueryCoordinator,
    health_checks: health_worker::HealthCheckCoordinator,
    process_started: Option<Instant>,
    stop_deadline: Option<Instant>,
    restart_after_stop: bool,
    /// A direct OpenCode leader exited while its authorized group still had
    /// cooperative members. Keep the group in graceful cleanup until it is
    /// empty; only then may automatic recovery start a replacement.
    group_cleanup_recovery: bool,
    next_restart: Option<Instant>,
    next_port_retry: Option<Instant>,
    port_release_deadline: Option<Instant>,
    network_wait_deadline: Option<Instant>,
    pending_start_trigger: Option<StartTrigger>,
    restart_attempt_index: usize,
    recovery_incident_active: bool,
    health_incident_active: bool,
    health_failures: u32,
    unverified_process_record: bool,
    unverified_check_at: Option<Instant>,
    /// An identity-verified process was reattached although its recorded
    /// configuration no longer matches the current one. The process stays
    /// managed (Stop/Restart remain available and are the convergence path),
    /// no second OpenCode is spawned, and `config_pending` asks the GUI to
    /// offer the restart. In-memory only: the next startup re-derives the
    /// same state from the record's fingerprint.
    stale_config_process: bool,
}

impl Supervisor {
    pub fn new(paths: AppPaths) -> io::Result<Self> {
        Self::with_options(paths, SupervisorOptions::default())
    }

    pub fn with_options(paths: AppPaths, options: SupervisorOptions) -> io::Result<Self> {
        paths.ensure_directories()?;
        let (mut runtime, runtime_state_loaded, runtime_state_error) =
            match RuntimeState::load(&paths) {
                Ok(runtime) => (runtime, true, None),
                Err(error) => {
                    let message = format!("Runtime state could not be loaded: {error}");
                    log(LogLevel::Error, &message);
                    // The fallback value is for status construction only. It
                    // is never persisted and never authorizes a start: an
                    // unreadable record may still describe a live OpenCode.
                    (RuntimeState::default(), false, Some(message))
                }
            };
        let config_fingerprint_key = ConfigFingerprintKey::load_or_create(&paths)?;
        let (latest_config, config_error) = match load_or_create(&paths) {
            Ok(config) => (Some(config), None),
            Err(error) => (None, Some(error.to_string())),
        };
        // The installed-version query spawns the configured executable as a
        // subprocess and can take seconds on a cold system. It is deferred to
        // a worker so OpenCodeServerAgent can bind its IPC socket and answer
        // status requests almost immediately after exec; see ADR 0009.
        let fda_state = probe_full_disk_access();
        let now = Instant::now();
        let credentials = credential::CredentialController::new(
            CredentialGrant::new(&paths),
            crate::keychain::probe_item,
            crate::keychain::read_password,
            crate::keychain::signing_team_identifier,
        );
        let version_queries =
            version::VersionQueryCoordinator::new(crate::version_query::query_installed_version);
        let mut supervisor = Self {
            paths,
            config_fingerprint_key,
            options,
            agent_started: now,
            runtime: runtime.clone(),
            runtime_state_loaded,
            runtime_state_reliable: runtime_state_loaded,
            runtime_state_retry_pending: false,
            runtime_state_retry_at: None,
            runtime_state_error: runtime_state_error.clone(),
            launch_pending_clear_requested: false,
            pending_process_exit: None,
            pending_lifecycle_intent: None,
            pending_durable_convergence: None,
            process: None,
            latest_config,
            active_config: None,
            server_state: ServerState::Stopped,
            health_state: HealthState::Unknown,
            fda_state,
            installed_version: None,
            running_version: None,
            config_error,
            last_error: runtime_state_error,
            notification: runtime.notification.clone(),
            last_health_check: None,
            health_generation: 0,
            health_generation_basis: None,
            last_config_check: now,
            credentials,
            version_queries,
            health_checks: health_worker::HealthCheckCoordinator::new(
                crate::health::check,
                HEALTH_OBSERVATION_BOUND,
            ),
            process_started: None,
            stop_deadline: None,
            restart_after_stop: false,
            group_cleanup_recovery: false,
            next_restart: None,
            next_port_retry: None,
            port_release_deadline: None,
            network_wait_deadline: None,
            pending_start_trigger: None,
            restart_attempt_index: 0,
            recovery_incident_active: false,
            health_incident_active: false,
            health_failures: 0,
            unverified_process_record: false,
            unverified_check_at: None,
            stale_config_process: false,
        };

        // Merge the Keychain credential into the freshly loaded
        // configuration before it feeds reattach fingerprint verification or
        // an initial spawn; the on-disk plist no longer carries a password.
        if let Some(config) = supervisor.latest_config.take() {
            let merged = supervisor.merge_credentials(config);
            supervisor.latest_config = Some(merged);
        }

        if supervisor.runtime_state_loaded && supervisor.runtime.launch_pending.is_none() {
            if let Some(record) = runtime.process.take() {
                supervisor.try_reattach(record)?;
            }
            if supervisor.process.is_none() && !supervisor.unverified_process_record {
                supervisor.runtime.process = None;
                if supervisor.pending_durable_convergence.is_none() {
                    supervisor.pending_durable_convergence = Some(
                        if supervisor.runtime.desired_state == DesiredState::Running {
                            PendingDurableConvergence::Start(StartTrigger::Cold)
                        } else {
                            PendingDurableConvergence::Stop
                        },
                    );
                }
                let outcome = supervisor.persist_runtime_detailed();
                if outcome == runtime_durability::RuntimePersistence::Durable {
                    supervisor.resume_pending_durable_convergence();
                } else {
                    supervisor.server_state = ServerState::Failed;
                    supervisor.health_state = HealthState::Unknown;
                }
            } else if supervisor.process.is_some()
                && supervisor.runtime.desired_state == DesiredState::Stopped
            {
                supervisor.begin_stop(false);
            }
        } else if !supervisor.runtime_state_loaded {
            // Keep the IPC server available so the user can see the precise
            // storage failure. No record was read, so no process may be
            // started, signaled, or claimed to be absent.
            supervisor.server_state = ServerState::Failed;
        } else {
            // A launch marker without a finalized process record means the
            // previous OpenCodeServerAgent may have created a child and then
            // disappeared before its identity became durable. Do not infer
            // that no child exists: the only safe recovery is explicit repair
            // after the interrupted transaction is resolved.
            let message =
                "A previous OpenCode launch was not durably finalized; OpenCodeServerAgent will not start another OpenCode"
                    .to_owned();
            supervisor.runtime_state_error = Some(message.clone());
            supervisor.last_error = Some(message);
            supervisor.server_state = ServerState::Failed;
            supervisor.health_state = HealthState::Unknown;
        }

        log(LogLevel::Notice, "OpenCodeServerAgent started");
        Ok(supervisor)
    }

    pub fn handle(&mut self, command: Command) -> Response {
        let result = match command {
            Command::Status => Ok(()),
            Command::Start => self.request_start(),
            Command::Stop => self.request_stop(),
            Command::ContinueStop => self.continue_stop(),
            Command::ForceStop => self.force_stop(),
            Command::Restart => self.request_restart(),
            Command::RefreshFda => {
                self.fda_state = probe_full_disk_access();
                log(
                    LogLevel::Info,
                    &format!("Full Disk Access probe result: {:?}", self.fda_state),
                );
                Ok(())
            }
            Command::RefreshCredentials => {
                self.request_credential_refresh();
                Ok(())
            }
            Command::CredentialChanged => {
                self.mark_credential_changed();
                Ok(())
            }
            Command::CredentialRemoved => {
                self.mark_credential_removed();
                Ok(())
            }
            Command::Subscribe => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "subscriptions are managed by the IPC layer, not by commands",
            )),
            Command::ValidateConfig => {
                let report = validation_report(&self.paths.config_file);
                return Response::validation(report, self.status());
            }
        };
        match result {
            Ok(()) => Response::success(self.status()),
            Err(error) => Response::error(error.to_string(), Some(self.status())),
        }
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        self.retry_runtime_state_if_due(now);
        self.poll_process(now);
        if self.unverified_process_record
            && self.process.is_none()
            && self.unverified_check_at.is_some_and(|at| now >= at)
        {
            self.check_unverified_process(now);
        }
        if self.server_state == ServerState::Stopping
            && self.stop_deadline.is_some_and(|deadline| now >= deadline)
        {
            self.server_state = ServerState::StopTimedOut;
            self.stop_deadline = None;
            self.last_error =
                Some("Graceful stop timed out — continue waiting or use “Force Stop…”".to_owned());
            log(
                LogLevel::Error,
                "Graceful stop interval expired; force termination was not sent",
            );
        }

        if self
            .next_restart
            .is_some_and(|next_restart| now >= next_restart)
            && self.process.is_none()
        {
            self.next_restart = None;
            self.start_now(StartTrigger::Recovery);
        }

        if self.next_port_retry.is_some_and(|retry| now >= retry) && self.process.is_none() {
            self.next_port_retry = None;
            let trigger = self
                .pending_start_trigger
                .take()
                .unwrap_or(StartTrigger::AfterStop);
            self.start_now(trigger);
        }

        if now.duration_since(self.last_config_check) >= CONFIG_RECHECK_INTERVAL {
            self.refresh_config();
            self.last_config_check = now;
        }
        self.poll_health_check(now);
        if self.process.is_some()
            && !matches!(
                self.server_state,
                ServerState::Stopping | ServerState::StopTimedOut
            )
            && now >= self.health_due(now)
        {
            self.dispatch_health_check(now);
        }
        self.poll_version_query(now);
        self.poll_credential_refresh(now);
        if self.process.is_some()
            && self
                .process_started
                .is_some_and(|started| now.duration_since(started) >= STABLE_RUN_INTERVAL)
        {
            self.restart_attempt_index = 0;
        }
    }

    /// The next instant at which `tick` has work to do, used by the event
    /// loop as its kqueue timeout. Returns `None` when nothing is scheduled.
    ///
    /// Only instants strictly in the future are candidates: one-shot
    /// deadlines that already fired (for example the stable-run reset, whose
    /// marker is never cleared) must never produce a zero-length wait, or
    /// the event loop would busy-spin calling `tick` forever.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let mut earliest: Option<Instant> = None;
        let mut consider = |instant: Instant| {
            if let Some(instant) = future_deadline(instant, now) {
                earliest = Some(earliest.map_or(instant, |current: Instant| current.min(instant)));
            }
        };
        consider(self.last_config_check + CONFIG_RECHECK_INTERVAL);
        if self.credentials.refresh_in_flight() {
            consider(now + CREDENTIAL_REFRESH_RECHECK);
        }
        if self.version_queries.in_flight() {
            consider(now + VERSION_IN_FLIGHT_RECHECK);
        } else if self.version_query_due(now) {
            consider(now);
        }
        if self.health_checks.in_flight() {
            let interval = if self.health_checks.overdue_logged() {
                HEALTH_OVERDUE_RECHECK
            } else {
                HEALTH_IN_FLIGHT_RECHECK
            };
            consider(now + interval);
        } else if self.health_task_is_available()
            && !matches!(
                self.server_state,
                ServerState::Stopping | ServerState::StopTimedOut
            )
        {
            consider(self.health_due(now));
        }
        // Lifecycle deadlines remain scheduled independently of health
        // eligibility. A stopping process must still reach StopTimedOut, and
        // the stable-run reset is useful even when health is temporarily
        // unavailable or an in-flight check is stuck.
        if self.process.is_some() {
            if let Some(deadline) = self.stop_deadline {
                consider(deadline);
            }
            if let Some(started) = self.process_started {
                consider(started + STABLE_RUN_INTERVAL);
            }
        }
        if let Some(next_restart) = self.next_restart {
            consider(next_restart);
        }
        if let Some(next_port_retry) = self.next_port_retry {
            consider(next_port_retry);
        }
        if let Some(runtime_state_retry_at) = self.runtime_state_retry_at {
            consider(runtime_state_retry_at);
        }
        if self.unverified_process_record && self.process.is_none() {
            consider(
                self.unverified_check_at
                    .unwrap_or(now + UNVERIFIED_CHECK_INTERVAL),
            );
        }
        earliest
    }

    /// Reap and classify a child exit reported by the kqueue NOTE_EXIT
    /// filter. The periodic `tick` poll remains as a fallback.
    pub fn poll_process_now(&mut self) {
        let now = Instant::now();
        self.poll_process(now);
    }

    /// Applies a configuration change reported by the kqueue vnode filter
    /// and resets the slow fallback recheck timer.
    pub fn refresh_config_now(&mut self) {
        self.refresh_config();
        self.last_config_check = Instant::now();
    }

    pub fn process_pid(&self) -> Option<u32> {
        self.process.as_ref().map(|process| process.record().pid)
    }

    /// Exposes the command result to process-supervision integration tests so
    /// they can assert the authoritative `io::ErrorKind` without changing the
    /// wire protocol, which intentionally carries only a bounded message.
    #[cfg(any(test, feature = "test-fixture"))]
    pub fn request_restart_for_test(&mut self) -> io::Result<()> {
        self.request_restart()
    }

    pub fn status(&self) -> Status {
        let config = self.active_config.as_ref().or(self.latest_config.as_ref());
        let endpoint = config
            .map(ValidatedConfig::endpoint)
            .unwrap_or_else(|| "Unavailable".to_owned());
        let username = config
            .map(|config| config.effective_username.clone())
            .unwrap_or_else(|| "opencode".to_owned());
        let authentication_enabled = config
            .map(ValidatedConfig::authentication_enabled)
            .unwrap_or(false);
        let password_state = match self.credentials.state() {
            CredentialState::NotConfigured => PasswordState::NotConfigured,
            CredentialState::AccessPending => PasswordState::AccessPending,
            CredentialState::Available => PasswordState::Configured,
        };
        let uptime_seconds = self.process.as_ref().map(|process| {
            SystemTime::now()
                .duration_since(
                    UNIX_EPOCH + Duration::from_secs(process.record().started_at_unix_seconds),
                )
                .unwrap_or_default()
                .as_secs()
        });
        let config_pending = (self.stale_config_process && self.process.is_some())
            || match (&self.active_config, &self.latest_config) {
                (Some(active), Some(latest)) => {
                    active.source != latest.source
                        || active.configured_executable != latest.configured_executable
                }
                _ => false,
            };
        let stop_grace_remaining_seconds = self.stop_deadline.map(|deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .as_secs()
                .saturating_add(1)
        });
        Status {
            protocol_version: PROTOCOL_VERSION,
            agent_version: VERSION.to_owned(),
            agent_uptime_seconds: self.agent_started.elapsed().as_secs(),
            desired_state: self.runtime.desired_state,
            server_state: self.server_state,
            health: self.health_state,
            fda: self.fda_state,
            uptime_seconds,
            endpoint,
            username,
            password_state,
            authentication_enabled,
            action_capabilities: self.action_capabilities(),
            installed_version: self.installed_version.clone(),
            running_version: self.running_version.clone(),
            version_pending: matches!(
                (&self.installed_version, &self.running_version),
                (Some(installed), Some(running)) if installed != running
            ),
            config_pending,
            config_error: self.config_error.clone(),
            last_error: self
                .runtime_state_error
                .clone()
                .or_else(|| self.last_error.clone()),
            pid: self.process.as_ref().map(|process| process.record().pid),
            stop_grace_remaining_seconds,
            notification: self.notification.clone(),
            process_started_at_unix_seconds: self
                .process
                .as_ref()
                .map(|process| process.record().started_at_unix_seconds),
            bundle_version: crate::BUNDLE_VERSION.to_owned(),
        }
    }

    /// Computes the action set from OpenCodeServerAgent's authoritative
    /// process, credential, and runtime-state facts.  These are a snapshot,
    /// not a promise that a later command cannot race with an identity or
    /// filesystem change; the command handlers remain authoritative.
    fn action_capabilities(&self) -> ActionCapabilities {
        let lifecycle_persistence_ready = self.lifecycle_persistence_ready();
        let process_signal_authorized = self
            .process
            .as_ref()
            .is_some_and(ManagedProcess::signal_authorized);
        let no_process_to_start = self.process.is_none() && !self.unverified_process_record;
        let credential_available = self.credentials.state() != CredentialState::AccessPending;

        ActionCapabilities {
            // Start is meaningful only when no managed process exists.  An
            // unverified record is a fail-closed process-presence claim, not
            // evidence that the endpoint is free.
            start: lifecycle_persistence_ready && no_process_to_start && credential_available,
            // Stop can cancel a running desired state even when no process
            // exists (for example a failed automatic start).  A managed
            // process must have a signal target that the command can use.
            stop: lifecycle_persistence_ready
                && (process_signal_authorized
                    || (no_process_to_start
                        && self.runtime.desired_state == DesiredState::Running)),
            // Restart either converges an authorized managed process or
            // starts from an empty, verified runtime record.  Keychain
            // access is a hard precondition for both paths.
            restart: lifecycle_persistence_ready
                && credential_available
                && (process_signal_authorized || no_process_to_start),
            // Continue Waiting only extends a graceful-stop interval after
            // OpenCodeServerAgent has observed its deadline.  It does not
            // persist an intent or signal a process itself.
            continue_stop: self.server_state == ServerState::StopTimedOut,
            // The GUI exposes Force Stop only after the graceful deadline has
            // expired.  Unlike Continue Waiting it needs the managed process
            // and its current signal authority; opencodeserverctl's explicit
            // --force command retains its own command-layer semantics.
            force_stop: self.server_state == ServerState::StopTimedOut && process_signal_authorized,
        }
    }

    /// Whether a lifecycle command can cross its durable desired-state
    /// boundary.  Pending launch markers and uncertain/unreadable runtime
    /// state deliberately disable Start/Stop/Restart until the retry path
    /// proves the record safe again.  Continue/Force do not use this gate:
    /// they operate on an already-observed stop interval and do not mutate
    /// durable desired state.
    fn lifecycle_persistence_ready(&self) -> bool {
        self.runtime_state_loaded
            && self.runtime_state_reliable
            && !self.runtime_state_retry_pending
            && self.runtime_state_error.is_none()
            && self.runtime.launch_pending.is_none()
            && !self.launch_pending_clear_requested
    }

    fn refresh_config(&mut self) {
        match load_and_validate(&self.paths.config_file) {
            Ok(config) => {
                let merged = self.merge_credentials(config);
                let changed = self.latest_config.as_ref() != Some(&merged);
                self.latest_config = Some(merged);
                self.config_error = None;
                if changed {
                    self.sync_health_generation();
                }
                // A reverted (or newly readable) configuration may match the
                // recorded fingerprint again, upgrading a stale attachment.
                self.recheck_stale_process();
            }
            Err(error) => {
                self.config_error = Some(error.to_string());
                if self.process.is_none() {
                    self.latest_config = None;
                }
            }
        }
    }

    fn emit_notification(
        &mut self,
        kind: NotificationKind,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> bool {
        let event_id = match new_notification_event_id() {
            Ok(event_id) => event_id,
            Err(error) => {
                log(
                    LogLevel::Fault,
                    &format!("Unable to generate a notification event ID: {error}"),
                );
                return false;
            }
        };
        self.notification = Some(NotificationEvent {
            event_id,
            kind,
            title: title.into(),
            message: message.into(),
        });
        self.runtime.notification = self.notification.clone();
        self.persist_runtime()
    }
}

fn new_notification_event_id() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)?;
    // RFC 9562 UUIDv4: random payload with the version and variant bits set.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    ))
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.finish_version_query_for_shutdown();
        // An in-flight credential worker only blocks inside
        // `SecItemCopyMatching` on the authorization dialog and owns no
        // child process; dropping its handle detaches it and the exiting
        // process reclaims the thread, so it must not be joined here.
        log(
            LogLevel::Notice,
            "OpenCodeServerAgent exiting without signaling the managed OpenCode",
        );
    }
}

/// Only instants strictly in the future may drive the event-loop timeout.
/// A one-shot deadline that already fired must not schedule a zero-length
/// wait, or the loop would spin calling `tick` back to back.
fn future_deadline(instant: Instant, now: Instant) -> Option<Instant> {
    (instant > now).then_some(instant)
}

fn join_version_query_worker(worker: &mut Option<JoinHandle<()>>) {
    let Some(worker) = worker.take() else {
        return;
    };
    if worker.join().is_err() {
        log(
            LogLevel::Error,
            "Installed-version query worker terminated unexpectedly",
        );
    }
}

fn join_credential_refresh_worker(worker: &mut Option<JoinHandle<()>>) {
    let Some(worker) = worker.take() else {
        return;
    };
    if worker.join().is_err() {
        log(
            LogLevel::Error,
            "Credential refresh worker terminated unexpectedly",
        );
    }
}

/// The `last_error` text for a health-check HTTP 401. When the credential
/// item is provably gone, the running OpenCode still carries the password it
/// was spawned with, so the guidance differs from an ordinary mismatch.
fn unauthorized_credential_message(state: CredentialState) -> &'static str {
    match state {
        CredentialState::NotConfigured => {
            "The saved password was removed from Keychain — re-save it in Settings"
        }
        _ => "OpenCode rejected the stored password — re-save it in Settings and restart",
    }
}

fn probe_full_disk_access() -> FdaState {
    let Some(home) = std::env::var_os("HOME") else {
        return FdaState::UnableToDetermine;
    };
    let target = std::path::PathBuf::from(home).join("Library/Safari/History.db");
    match File::open(target) {
        Ok(file) => match file.metadata() {
            Ok(_) => FdaState::Verified,
            Err(_) => FdaState::UnableToDetermine,
        },
        Err(error)
            if error.kind() == io::ErrorKind::PermissionDenied
                || error.raw_os_error() == Some(libc::EPERM) =>
        {
            FdaState::NotVerified
        }
        Err(_) => FdaState::UnableToDetermine,
    }
}

mod credential;
#[cfg(test)]
mod credential_tests;
mod health;
mod health_worker;
#[cfg(test)]
mod health_worker_tests;
mod launch_transaction;
mod lifecycle;
mod process_exit;
mod reattach;
mod reattach_policy;
#[cfg(test)]
mod reattach_policy_tests;
mod runtime_durability;
#[cfg(test)]
mod tests;
mod version;
#[cfg(test)]
mod version_tests;
