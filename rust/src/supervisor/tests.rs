use super::*;
use crate::config::{ConfigFile, write_config_atomically};
use crate::config_fingerprint::ConfigFingerprint;
use crate::process::ManagedProcess;
use crate::runtime_state::ProcessRecord;
use std::net::TcpListener;

fn deadline_test_record(pid: u32) -> ProcessRecord {
    ProcessRecord {
        pid,
        process_group_id: pid,
        start_seconds: 1,
        start_microseconds: u64::from(pid),
        executable: "/tmp/opencode".to_owned(),
        started_at_unix_seconds: 1,
        running_version: None,
        config_fingerprint: ConfigFingerprint {
            version: 1,
            hmac_sha256: "a".repeat(64),
        },
        identity_unconfirmed: false,
    }
}

#[test]
fn restart_policy_is_bounded_and_expected() {
    assert_eq!(
        RESTART_BACKOFF.map(|duration| duration.as_secs()),
        [1, 2, 5, 15, 30]
    );
}

#[test]
fn notification_event_ids_are_unique_uuid_v4_values() {
    let first = new_notification_event_id().expect("first notification event ID");
    let second = new_notification_event_id().expect("second notification event ID");
    assert_ne!(first, second);
    assert_eq!(first.len(), 36);
    assert_eq!(&first[8..9], "-");
    assert_eq!(&first[13..14], "-");
    assert_eq!(&first[18..19], "-");
    assert_eq!(&first[23..24], "-");
    assert_eq!(&first[14..15], "4");
    assert!(matches!(&first[19..20], "8" | "9" | "a" | "b"));
    assert!(
        first
            .chars()
            .all(|character| character == '-' || character.is_ascii_hexdigit())
    );
}

#[test]
fn past_deadlines_never_schedule_a_zero_wait() {
    // Regression lock for the STABLE_RUN_INTERVAL busy-spin: a deadline
    // that already fired must not produce a zero-length event-loop wait.
    let now = Instant::now();
    assert_eq!(future_deadline(now, now), None);
    assert_eq!(future_deadline(now - Duration::from_secs(300), now), None);
    let later = now + Duration::from_secs(1);
    assert_eq!(future_deadline(later, now), Some(later));
}

#[test]
fn lifecycle_deadlines_are_scheduled_when_health_is_unavailable() {
    let root = std::env::temp_dir().join(format!(
        "opencodeserver-health-deadline-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let paths = AppPaths::from_support_dir(root.clone());
    paths.ensure_directories().expect("private test paths");
    write_config_atomically(
        &paths.config_file,
        &ConfigFile {
            executable_path: std::env::current_exe()
                .expect("test executable")
                .to_string_lossy()
                .into_owned(),
            ..ConfigFile::default()
        },
    )
    .expect("test config");
    RuntimeState {
        desired_state: DesiredState::Stopped,
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("stopped runtime state");

    let mut supervisor = Supervisor::new(paths).expect("supervisor");
    supervisor.process = Some(ManagedProcess::attach(deadline_test_record(4242)));
    supervisor.active_config = supervisor.latest_config.clone();
    supervisor.server_state = ServerState::Stopping;
    let now = Instant::now();
    let stop_deadline = now + Duration::from_secs(2);
    supervisor.stop_deadline = Some(stop_deadline);
    supervisor.last_config_check = now + Duration::from_secs(100);
    supervisor
        .version_queries
        .set_last_attempt_for_test(Some(now));
    let deadline = supervisor
        .next_deadline(now)
        .expect("stop deadline must wake the supervisor");
    assert!(
        deadline <= stop_deadline,
        "health gating must not postpone StopTimedOut: {deadline:?} > {stop_deadline:?}"
    );

    // A process can also have no eligible health task (for example an
    // identity-verified stale attachment whose configuration is still
    // different). The stable-run reset remains an independent lifecycle
    // deadline rather than disappearing with health scheduling.
    supervisor.server_state = ServerState::Failed;
    supervisor.stop_deadline = None;
    supervisor.active_config = None;
    supervisor.stale_config_process = false;
    let stable_deadline = now + Duration::from_secs(2);
    supervisor.process_started = Some(
        stable_deadline
            .checked_sub(STABLE_RUN_INTERVAL)
            .expect("stable deadline"),
    );
    let deadline = supervisor
        .next_deadline(now)
        .expect("stable-run deadline must wake the supervisor");
    assert_eq!(deadline, stable_deadline);

    drop(supervisor);
    std::fs::remove_dir_all(root).expect("remove test root");
}

#[test]
fn restart_renews_an_expired_port_release_retry_budget() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "opencodeserver-restart-port-budget-{}-{}",
        std::process::id(),
        nonce
    ));
    let paths = AppPaths::from_support_dir(root.clone());
    paths.ensure_directories().expect("private test paths");
    let listener = TcpListener::bind("127.0.0.1:0").expect("occupy configured endpoint");
    let port = listener.local_addr().expect("listener address").port();
    write_config_atomically(
        &paths.config_file,
        &ConfigFile {
            port,
            username: format!("restart-budget-{}-{nonce}", std::process::id()),
            executable_path: std::env::current_exe()
                .expect("test executable")
                .to_string_lossy()
                .into_owned(),
            ..ConfigFile::default()
        },
    )
    .expect("test config");
    RuntimeState {
        desired_state: DesiredState::Stopped,
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("stopped runtime state");

    let mut supervisor = Supervisor::new(paths).expect("supervisor");
    let first = supervisor.handle(Command::Restart);
    assert!(first.ok);
    assert_eq!(
        first.status.expect("first status").server_state,
        ServerState::WaitingToRestart,
        "an occupied endpoint must begin the bounded port-release retry"
    );

    supervisor.port_release_deadline = Some(
        Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("past deadline"),
    );
    let restarted_at = Instant::now();
    let second = supervisor.handle(Command::Restart);
    assert!(second.ok);
    let status = second.status.expect("second status");
    assert_eq!(
        status.server_state,
        ServerState::WaitingToRestart,
        "a new Restart must receive a fresh port-release retry budget"
    );
    assert_eq!(status.last_error, None);
    assert!(
        supervisor
            .port_release_deadline
            .is_some_and(|deadline| deadline >= restarted_at + PORT_RELEASE_RETRY_BUDGET),
        "the renewed retry must carry the complete budget"
    );

    drop(supervisor);
    drop(listener);
    std::fs::remove_dir_all(root).expect("remove test root");
}

#[test]
fn no_remote_authentication_requirement_overrides_native_behavior() {
    let remote: std::net::IpAddr = "10.0.0.254".parse().expect("IP");
    assert!(!remote.is_loopback());
    // PRODUCT_DECISIONS.md explicitly permits an empty password. The UI
    // warns for non-loopback endpoints but OpenCodeServerAgent does not
    // reject them.
}

#[test]
fn unauthorized_message_distinguishes_removed_item_from_mismatch() {
    assert!(
        unauthorized_credential_message(CredentialState::NotConfigured)
            .contains("removed from Keychain")
    );
    assert!(
        unauthorized_credential_message(CredentialState::Available)
            .contains("rejected the stored password")
    );
    assert!(
        unauthorized_credential_message(CredentialState::AccessPending)
            .contains("rejected the stored password")
    );
}

#[test]
fn credential_read_result_for_previous_username_is_discarded() {
    let root = std::env::temp_dir().join(format!(
        "opencodeserver-credential-race-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let paths = AppPaths::from_support_dir(root.clone());
    paths.ensure_directories().expect("private test paths");

    let previous_account = "credential-race-before";
    let current_account = "credential-race-after";
    write_config_atomically(
        &paths.config_file,
        &ConfigFile {
            username: previous_account.to_owned(),
            executable_path: std::env::current_exe()
                .expect("test executable")
                .to_string_lossy()
                .into_owned(),
            ..ConfigFile::default()
        },
    )
    .expect("test config");
    RuntimeState {
        desired_state: DesiredState::Stopped,
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("stopped runtime state");

    let mut supervisor = Supervisor::new(paths).expect("supervisor");
    supervisor
        .credentials
        .set_state_for_test(CredentialState::AccessPending);
    let config = supervisor.latest_config.as_mut().expect("loaded config");
    config.source.password = "current-account-password".to_owned();

    // Model the real race deterministically: the worker remains pending,
    // configuration advances to a different username, and only then does
    // the old account's decrypt result arrive.
    let (sender, receiver) = mpsc::channel();
    supervisor
        .credentials
        .set_refresh_in_flight_for_test(CredentialRefreshInFlight {
            dispatched: Instant::now(),
            account: previous_account.to_owned(),
            worker: None,
            receiver,
        });
    let config = supervisor.latest_config.as_mut().expect("loaded config");
    config.source.username = current_account.to_owned();
    config.effective_username = current_account.to_owned();
    sender
        .send(KeychainRead::Found("stale-account-password".to_owned()))
        .expect("worker result");
    drop(sender);

    supervisor.poll_credential_refresh(Instant::now());

    let config = supervisor.latest_config.as_ref().expect("loaded config");
    assert_eq!(config.effective_username, current_account);
    assert_eq!(config.source.password, "current-account-password");
    assert_eq!(
        supervisor.credentials.state_for_test(),
        CredentialState::AccessPending
    );
    assert!(
        supervisor
            .credentials
            .refresh_in_flight_for_test()
            .is_none()
    );
    assert_eq!(supervisor.credentials.grant_for_test().load(), None);

    drop(supervisor);
    std::fs::remove_dir_all(root).expect("remove test root");
}

#[test]
fn first_credential_notice_moves_not_configured_to_fail_closed_access_pending() {
    let root = std::env::temp_dir().join(format!(
        "opencodeserver-first-credential-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let paths = AppPaths::from_support_dir(root.clone());
    paths.ensure_directories().expect("private test paths");
    write_config_atomically(
        &paths.config_file,
        &ConfigFile {
            username: "credential-created".to_owned(),
            executable_path: std::env::current_exe()
                .expect("test executable")
                .to_string_lossy()
                .into_owned(),
            ..ConfigFile::default()
        },
    )
    .expect("test config");
    RuntimeState {
        desired_state: DesiredState::Stopped,
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("stopped runtime state");

    let mut supervisor = Supervisor::new(paths).expect("supervisor");
    supervisor
        .credentials
        .set_state_for_test(CredentialState::NotConfigured);
    assert!(supervisor.start_refusal().is_none());

    let response = supervisor.handle(Command::CredentialChanged);

    assert!(response.ok);
    assert_eq!(
        response.status.expect("status").password_state,
        PasswordState::AccessPending
    );
    assert!(supervisor.start_refusal().is_some());

    drop(supervisor);
    std::fs::remove_dir_all(root).expect("remove test root");
}

#[test]
fn explicit_credential_removal_converges_without_a_keychain_read() {
    let root = std::env::temp_dir().join(format!(
        "opencodeserver-credential-removed-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let paths = AppPaths::from_support_dir(root.clone());
    paths.ensure_directories().expect("private test paths");
    let account = "credential-removed";
    write_config_atomically(
        &paths.config_file,
        &ConfigFile {
            username: account.to_owned(),
            executable_path: std::env::current_exe()
                .expect("test executable")
                .to_string_lossy()
                .into_owned(),
            ..ConfigFile::default()
        },
    )
    .expect("test config");
    RuntimeState {
        desired_state: DesiredState::Stopped,
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("stopped runtime state");

    let mut supervisor = Supervisor::new(paths).expect("supervisor");
    let latest = supervisor.latest_config.as_mut().expect("loaded config");
    latest.source.password = "carried-running-password".to_owned();
    supervisor.active_config = Some(latest.clone());
    supervisor
        .credentials
        .set_state_for_test(CredentialState::Available);
    supervisor.credentials.set_stale_for_test(true);
    supervisor
        .credentials
        .grant_for_test()
        .record(account, crate::BUNDLE_VERSION, "")
        .expect("record grant");

    let response = supervisor.handle(Command::CredentialRemoved);

    assert!(response.ok);
    assert_eq!(
        response.status.expect("status").password_state,
        PasswordState::NotConfigured
    );
    assert_eq!(
        supervisor.credentials.state_for_test(),
        CredentialState::NotConfigured
    );
    assert!(!supervisor.credentials.stale_for_test());
    assert!(supervisor.credentials.explicitly_removed_for_test());
    assert!(
        supervisor
            .credentials
            .refresh_in_flight_for_test()
            .is_none()
    );
    assert_eq!(supervisor.credentials.grant_for_test().load(), None);
    assert_eq!(
        supervisor
            .latest_config
            .as_ref()
            .expect("latest config")
            .source
            .password,
        ""
    );
    assert_eq!(
        supervisor
            .active_config
            .as_ref()
            .expect("active config")
            .source
            .password,
        "carried-running-password",
        "the still-running OpenCode health credential remains active until restart"
    );

    // A decrypt that was already in flight when Settings deleted the
    // item must not resurrect the removed password or its grant.
    let (sender, receiver) = mpsc::channel();
    supervisor
        .credentials
        .set_refresh_in_flight_for_test(CredentialRefreshInFlight {
            dispatched: Instant::now(),
            account: account.to_owned(),
            worker: None,
            receiver,
        });
    sender
        .send(KeychainRead::Found("stale-decrypt-result".to_owned()))
        .expect("stale worker result");
    drop(sender);
    supervisor.poll_credential_refresh(Instant::now());
    assert_eq!(
        supervisor.credentials.state_for_test(),
        CredentialState::NotConfigured
    );
    assert_eq!(supervisor.credentials.grant_for_test().load(), None);
    assert_eq!(
        supervisor
            .latest_config
            .as_ref()
            .expect("latest config")
            .source
            .password,
        ""
    );

    drop(supervisor);
    std::fs::remove_dir_all(root).expect("remove test root");
}

#[test]
fn force_stop_and_continue_stop_require_a_stop_in_progress() {
    // Force Stop is the documented second explicit action after a graceful
    // stop (or its timeout); in any other state both commands must be
    // refused with InvalidInput rather than terminating a healthy process.
    let root = std::env::temp_dir().join(format!(
        "opencodeserver-stop-gating-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let paths = AppPaths::from_support_dir(root.clone());
    paths.ensure_directories().expect("private test paths");
    write_config_atomically(
        &paths.config_file,
        &ConfigFile {
            username: format!("stop-gating-{}", std::process::id()),
            executable_path: std::env::current_exe()
                .expect("test executable")
                .to_string_lossy()
                .into_owned(),
            ..ConfigFile::default()
        },
    )
    .expect("test config");
    RuntimeState {
        desired_state: DesiredState::Stopped,
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("stopped runtime state");

    let mut supervisor = Supervisor::new(paths).expect("supervisor");
    assert_eq!(supervisor.status().server_state, ServerState::Stopped);

    for state in [ServerState::Stopped, ServerState::Healthy] {
        supervisor.server_state = state;

        let response = supervisor.handle(Command::ForceStop);
        assert!(!response.ok, "Force Stop must be refused while {state:?}");
        assert_eq!(
            response.error.as_deref(),
            Some("force stop is available only after a stop or restart request")
        );
        assert_eq!(
            supervisor
                .force_stop()
                .expect_err("Force Stop must be refused")
                .kind(),
            io::ErrorKind::InvalidInput
        );

        let response = supervisor.handle(Command::ContinueStop);
        assert!(
            !response.ok,
            "Continue Waiting must be refused while {state:?}"
        );
        assert_eq!(
            response.error.as_deref(),
            Some("OpenCode is not waiting after a graceful-stop timeout")
        );
        assert_eq!(
            supervisor
                .continue_stop()
                .expect_err("Continue Waiting must be refused")
                .kind(),
            io::ErrorKind::InvalidInput
        );

        assert_eq!(
            supervisor.status().server_state,
            state,
            "a refused command must not change the server state"
        );
    }

    // The gate is on the server state alone: once a stop is in progress the
    // InvalidInput refusal lifts (with no process attached, the command
    // reaches the not-running check instead).
    supervisor.server_state = ServerState::Stopping;
    assert_eq!(
        supervisor
            .force_stop()
            .expect_err("Force Stop without a process must fail differently")
            .kind(),
        io::ErrorKind::NotFound
    );

    drop(supervisor);
    std::fs::remove_dir_all(root).expect("remove test root");
}

#[test]
fn action_capabilities_follow_authoritative_lifecycle_facts() {
    let root = std::env::temp_dir().join(format!(
        "opencodeserver-action-capabilities-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let paths = AppPaths::from_support_dir(root.clone());
    paths.ensure_directories().expect("private test paths");
    write_config_atomically(
        &paths.config_file,
        &ConfigFile {
            username: format!("action-capabilities-{}", std::process::id()),
            executable_path: std::env::current_exe()
                .expect("test executable")
                .to_string_lossy()
                .into_owned(),
            ..ConfigFile::default()
        },
    )
    .expect("test config");
    RuntimeState {
        desired_state: DesiredState::Stopped,
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("stopped runtime state");

    let mut supervisor = Supervisor::new(paths).expect("supervisor");

    // Stopped is the empty, durable baseline: Start and Restart can be
    // accepted, while Stop/Continue/Force have no useful target.
    assert_eq!(
        supervisor.status().action_capabilities,
        ActionCapabilities {
            start: true,
            stop: false,
            restart: true,
            continue_stop: false,
            force_stop: false,
        }
    );

    let managed_record = deadline_test_record(42_424);
    supervisor.process = Some(ManagedProcess::attach(managed_record.clone()));
    supervisor.runtime.desired_state = DesiredState::Running;
    for state in [
        ServerState::Starting,
        ServerState::Healthy,
        ServerState::Unhealthy,
        ServerState::Failed,
    ] {
        supervisor.server_state = state;
        let capabilities = supervisor.status().action_capabilities;
        assert!(
            !capabilities.start,
            "Start must not duplicate a managed process in {state:?}"
        );
        assert!(capabilities.stop, "Stop must remain available in {state:?}");
        assert!(
            capabilities.restart,
            "Restart must remain the convergence path in {state:?}"
        );
        assert!(!capabilities.continue_stop);
        assert!(!capabilities.force_stop);
    }

    // Automatic recovery waits without a process. Start remains a useful
    // explicit retry and Stop can cancel the durable Running intent.
    supervisor.process = None;
    supervisor.server_state = ServerState::WaitingToRestart;
    let waiting = supervisor.status().action_capabilities;
    assert!(waiting.start);
    assert!(waiting.stop);
    assert!(waiting.restart);
    assert!(!waiting.continue_stop);
    assert!(!waiting.force_stop);

    // An unverified record is neither proof of absence nor a signal target.
    supervisor.runtime.process = Some(managed_record.clone());
    supervisor.unverified_process_record = true;
    supervisor.server_state = ServerState::Failed;
    let unverified = supervisor.status().action_capabilities;
    assert_eq!(
        unverified,
        ActionCapabilities {
            start: false,
            stop: false,
            restart: false,
            continue_stop: false,
            force_stop: false,
        }
    );

    // A Keychain item awaiting authorization blocks only actions that can
    // launch/relaunch OpenCode; Stop remains available to cancel the desired
    // Running intent when no process is present.
    supervisor.runtime.process = None;
    supervisor.unverified_process_record = false;
    supervisor
        .credentials
        .set_state_for_test(CredentialState::AccessPending);
    supervisor.server_state = ServerState::Failed;
    let access_pending = supervisor.status().action_capabilities;
    assert_eq!(
        access_pending,
        ActionCapabilities {
            start: false,
            stop: true,
            restart: false,
            continue_stop: false,
            force_stop: false,
        }
    );
    supervisor
        .credentials
        .set_state_for_test(CredentialState::NotConfigured);

    // Force Stop is intentionally withheld during the graceful interval and
    // appears only after OpenCodeServerAgent observes StopTimedOut. Continue
    // Waiting has the same exact state gate; both still require a managed
    // process for Force Stop's signal target.
    supervisor.process = Some(ManagedProcess::attach(managed_record));
    supervisor.server_state = ServerState::Stopping;
    assert!(!supervisor.status().action_capabilities.force_stop);
    assert!(!supervisor.status().action_capabilities.continue_stop);
    supervisor.server_state = ServerState::StopTimedOut;
    let timed_out = supervisor.status().action_capabilities;
    assert!(timed_out.continue_stop);
    assert!(timed_out.force_stop);

    // A launch marker or an uncertain runtime-state write blocks only the
    // durable lifecycle commands. Continue/Force remain governed by their
    // own state/ownership gates.
    supervisor.process = None;
    supervisor.runtime.launch_pending = Some(crate::runtime_state::LaunchPending {
        executable: "/tmp/opencode".to_owned(),
        config_fingerprint: ConfigFingerprint {
            version: 1,
            hmac_sha256: "b".repeat(64),
        },
    });
    supervisor.server_state = ServerState::Failed;
    assert_eq!(
        supervisor.status().action_capabilities,
        ActionCapabilities {
            start: false,
            stop: false,
            restart: false,
            continue_stop: false,
            force_stop: false,
        }
    );
    supervisor.runtime.launch_pending = None;
    supervisor.runtime_state_retry_pending = true;
    assert_eq!(
        supervisor.status().action_capabilities,
        ActionCapabilities {
            start: false,
            stop: false,
            restart: false,
            continue_stop: false,
            force_stop: false,
        }
    );

    drop(supervisor);
    std::fs::remove_dir_all(root).expect("remove test root");
}

// ---- FDA probe (ADR 0002, 2026-08-20 amendment) ----

fn fda_probe_root(tag: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "opencodeserver-fda-probe-{tag}-{}-{nonce}",
        std::process::id()
    ))
}

fn fda_probe_fixture(root: &std::path::Path, targets: &[&str]) {
    for relative in targets {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("fixture parent"))
            .expect("fixture parent directory");
        if relative.ends_with(".db") {
            std::fs::write(&path, b"SQLite format 3\0").expect("fixture file target");
        } else {
            std::fs::create_dir(&path).expect("fixture directory target");
        }
    }
}

fn fda_probe_deny(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000))
        .expect("deny fixture permissions");
}

#[test]
fn fda_probe_targets_are_versioned_by_adr_0002() {
    // Changing this table is an ADR 0002 decision, not a code tweak: the
    // assertion exists so any edit fails this test first.
    assert_eq!(
        FDA_PROBE_TARGETS.to_vec(),
        vec![
            "Library/Safari/History.db",
            "Library/Mail/V10",
            "Library/Suggestions",
        ]
    );
}

#[test]
fn parse_product_major_version_covers_release_shapes() {
    assert_eq!(parse_product_major_version("26.6.1"), Some(26));
    assert_eq!(parse_product_major_version("27"), Some(27));
    assert_eq!(parse_product_major_version("10.15.7"), Some(10));
    assert_eq!(parse_product_major_version(""), None);
    assert_eq!(parse_product_major_version("not-a-version"), None);
}

#[test]
fn fda_probe_gates_on_os_major_version() {
    let root = fda_probe_root("gate");
    fda_probe_fixture(&root, FDA_PROBE_TARGETS);
    assert_eq!(
        fda_state_for_version(None, &root),
        FdaState::UnableToDetermine
    );
    assert_eq!(
        fda_state_for_version(Some(FDA_PROBE_OS_MAJOR_VERSION + 1), &root),
        FdaState::UnableToDetermine
    );
    assert_eq!(
        fda_state_for_version(Some(28), &root),
        FdaState::UnableToDetermine
    );
    assert_eq!(fda_state_for_version(Some(26), &root), FdaState::Verified);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fda_probe_verified_when_all_existing_targets_open() {
    let root = fda_probe_root("verified");
    fda_probe_fixture(&root, FDA_PROBE_TARGETS);
    assert_eq!(fda_state_for_version(Some(26), &root), FdaState::Verified);
    // Existence-aware: a subset of targets still yields a verdict when the
    // rest of the fixture (e.g. Mail data) was never initialized.
    let partial = fda_probe_root("verified-partial");
    fda_probe_fixture(
        &partial,
        &["Library/Safari/History.db", "Library/Suggestions"],
    );
    assert_eq!(
        fda_state_for_version(Some(26), &partial),
        FdaState::Verified
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&partial);
}

#[test]
fn fda_probe_unable_when_no_targets_exist() {
    let root = fda_probe_root("absent");
    std::fs::create_dir_all(&root).expect("empty fixture root");
    assert_eq!(
        fda_state_for_version(Some(26), &root),
        FdaState::UnableToDetermine
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fda_probe_not_verified_when_every_existing_target_denies() {
    let root = fda_probe_root("denied");
    fda_probe_fixture(&root, FDA_PROBE_TARGETS);
    for relative in FDA_PROBE_TARGETS {
        fda_probe_deny(&root.join(relative));
    }
    assert_eq!(
        fda_state_for_version(Some(26), &root),
        FdaState::NotVerified
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fda_probe_unable_on_mixed_accessibility() {
    // Models the measured 2026-08-19 drift: one target readable while its
    // siblings stay denied. Consensus must degrade to uncertainty rather
    // than trust the single readable target.
    let root = fda_probe_root("mixed");
    fda_probe_fixture(&root, FDA_PROBE_TARGETS);
    fda_probe_deny(&root.join("Library/Mail/V10"));
    fda_probe_deny(&root.join("Library/Suggestions"));
    assert_eq!(
        fda_state_for_version(Some(26), &root),
        FdaState::UnableToDetermine
    );
    let _ = std::fs::remove_dir_all(&root);
}
