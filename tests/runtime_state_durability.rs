#![cfg(feature = "test-fixture")]

use opencodeserver::config::{ConfigFile, write_config_atomically};
use opencodeserver::paths::AppPaths;
use opencodeserver::platform::{process_exists, process_snapshot, send_process_group_signal};
use opencodeserver::protocol::{Command, DesiredState, ServerState};
use opencodeserver::runtime_state::{
    ProcessRecord, RuntimeState, fail_runtime_state_directory_sync_after_for_tests,
    fail_runtime_state_directory_sync_for_tests, fail_runtime_state_save_after_for_tests,
    fail_runtime_state_saves_for_tests,
};
use opencodeserver::supervisor::Supervisor;
use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static TEST_NONCE: AtomicU64 = AtomicU64::new(1);

fn test_paths(label: &str) -> AppPaths {
    let nonce = TEST_NONCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    AppPaths::from_support_dir(std::env::temp_dir().join(format!(
        "opencodeserver-runtime-durability-{label}-{}-{timestamp}-{nonce}",
        std::process::id()
    )))
}

fn write_test_config(paths: &AppPaths, port: u16) {
    write_test_config_with_executable(
        paths,
        port,
        Path::new(env!("CARGO_BIN_EXE_opencodeserver-test-child")),
    );
}

fn write_test_config_with_executable(paths: &AppPaths, port: u16, executable: &Path) {
    write_config_atomically(
        &paths.config_file,
        &ConfigFile {
            port,
            username: format!("runtime-state-{}", std::process::id()),
            executable_path: executable.to_string_lossy().into_owned(),
            ..ConfigFile::default()
        },
    )
    .expect("write test config");
}

fn available_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("reserve test port")
        .local_addr()
        .expect("port address")
        .port()
}

struct PortReservation {
    executable: PathBuf,
    listener: TcpListener,
    port: u16,
}

impl PortReservation {
    fn for_paths(paths: &AppPaths) -> Self {
        let executable = paths.support_dir.join("opencode");
        fs::copy(env!("CARGO_BIN_EXE_opencodeserver-test-child"), &executable)
            .expect("copy fixture for port reservation");
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve fixture port");
        let port = listener
            .local_addr()
            .expect("read reserved fixture port")
            .port();
        fs::write(
            executable.with_file_name("port-reservation-held"),
            b"held\n",
        )
        .expect("publish fixture port reservation");
        Self {
            executable,
            listener,
            port,
        }
    }

    fn release(self) {
        drop(self.listener);
        fs::write(
            self.executable.with_file_name("port-reservation.release"),
            b"release\n",
        )
        .expect("publish fixture port release");
        let ready = self.executable.with_file_name("port-bind.ready");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.is_file() {
            assert!(
                Instant::now() < deadline,
                "fixture did not bind the reserved port after release"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn remove_support_dir(paths: &AppPaths) {
    fs::remove_dir_all(&paths.support_dir).expect("remove test support directory");
}

#[test]
fn durable_start_retry_after_launch_marker_directory_sync_starts_once() {
    let paths = test_paths("marker-directory-retry");
    paths
        .ensure_directories()
        .expect("create support directory");
    write_test_config(&paths, available_port());
    RuntimeState {
        desired_state: DesiredState::Stopped,
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("save stopped state");

    let mut supervisor = Supervisor::new(paths.clone()).expect("supervisor");
    // The desired-state write is the first sync and succeeds. The following
    // sync belongs to the pre-spawn marker, so no child may be created until
    // that uncertain rename is confirmed by the retry.
    let directory_sync_failure =
        fail_runtime_state_directory_sync_after_for_tests(&paths.run_dir, 1);
    let response = supervisor.handle(Command::Start);
    assert!(!response.ok, "an uncertain launch marker must block Start");
    assert_eq!(supervisor.status().pid, None);
    let pending = RuntimeState::load(&paths).expect("load uncertain launch marker");
    assert_eq!(pending.desired_state, DesiredState::Running);
    assert!(pending.launch_pending.is_some());
    drop(directory_sync_failure);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let pid = loop {
        supervisor.tick();
        if let Some(pid) = supervisor.status().pid {
            break pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "durable launch-marker retry did not resume Start"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(process_exists(pid));
    for _ in 0..10 {
        supervisor.tick();
        assert_eq!(
            supervisor.status().pid,
            Some(pid),
            "the post-durability Start must be consumed exactly once"
        );
        thread::sleep(Duration::from_millis(20));
    }

    supervisor.handle(Command::Stop);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while supervisor.status().pid.is_some() {
        supervisor.tick();
        assert!(
            std::time::Instant::now() < deadline,
            "OpenCode did not stop after marker retry"
        );
        thread::sleep(Duration::from_millis(20));
    }
    drop(supervisor);
    remove_support_dir(&paths);
}

#[test]
fn restart_respawn_marker_directory_sync_retry_starts_once() {
    let paths = test_paths("restart-marker-directory-retry");
    paths
        .ensure_directories()
        .expect("create support directory");
    write_test_config(&paths, available_port());
    RuntimeState::default()
        .save(&paths)
        .expect("save running state");

    let mut supervisor = Supervisor::new(paths.clone()).expect("supervisor");
    let old_pid = supervisor
        .status()
        .pid
        .expect("OpenCode started from default running state");
    // Restart performs: desired-state save, process-record clear, then the
    // respawn marker save. Allow the first two directory syncs and fail only
    // the marker, proving the restart path has its own one-shot convergence.
    let directory_sync_failure =
        fail_runtime_state_directory_sync_after_for_tests(&paths.run_dir, 2);
    let response = supervisor.handle(Command::Restart);
    assert!(
        response.ok,
        "Restart intent and stop signal should be durable"
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while supervisor.status().pid == Some(old_pid) {
        supervisor.poll_process_now();
        assert!(
            std::time::Instant::now() < deadline,
            "the explicit Restart did not observe the old OpenCode exit"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(supervisor.status().pid, None);
    assert_eq!(supervisor.status().server_state, ServerState::Failed);
    assert!(
        RuntimeState::load(&paths)
            .expect("load uncertain respawn marker")
            .launch_pending
            .is_some()
    );
    drop(directory_sync_failure);

    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    let new_pid = loop {
        supervisor.tick();
        if let Some(pid) = supervisor.status().pid {
            break pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "durable respawn-marker retry did not resume Restart"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert_ne!(new_pid, old_pid);
    assert!(process_exists(new_pid));
    for _ in 0..10 {
        supervisor.tick();
        assert_eq!(
            supervisor.status().pid,
            Some(new_pid),
            "the respawn trigger must be consumed exactly once"
        );
        thread::sleep(Duration::from_millis(20));
    }

    supervisor.handle(Command::Stop);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while supervisor.status().pid.is_some() {
        supervisor.tick();
        assert!(
            std::time::Instant::now() < deadline,
            "OpenCode did not stop after Restart convergence"
        );
        thread::sleep(Duration::from_millis(20));
    }
    drop(supervisor);
    remove_support_dir(&paths);
}

#[test]
fn startup_stale_record_clear_directory_sync_retry_converges_to_one_start() {
    let paths = test_paths("stale-clear-directory-retry");
    paths
        .ensure_directories()
        .expect("create support directory");
    write_test_config(&paths, available_port());

    let fixture = env!("CARGO_BIN_EXE_opencodeserver-test-child");
    let mut stale_child = ProcessCommand::new(fixture)
        .arg("wait")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn stale fixture");
    let stale_pid = stale_child.id();
    let stale_snapshot = process_snapshot(stale_pid).expect("snapshot stale fixture");
    send_process_group_signal(stale_snapshot.process_group_id, libc::SIGTERM)
        .expect("terminate stale fixture");
    stale_child.wait().expect("reap stale fixture");

    RuntimeState {
        desired_state: DesiredState::Running,
        process: Some(ProcessRecord {
            pid: stale_snapshot.pid,
            process_group_id: stale_snapshot.process_group_id,
            start_seconds: stale_snapshot.start_seconds,
            start_microseconds: stale_snapshot.start_microseconds,
            executable: fixture.to_owned(),
            started_at_unix_seconds: 1,
            running_version: None,
            config_fingerprint: opencodeserver::config_fingerprint::ConfigFingerprint {
                version: 1,
                hmac_sha256: "00".repeat(32),
            },
            identity_unconfirmed: false,
        }),
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("save stale process state");

    // Startup first clears the provably missing record. The rename becomes
    // visible but its directory sync is uncertain, so startup must remain
    // failed and diagnostic until a later durable retry proves the clear.
    let directory_sync_failure = fail_runtime_state_directory_sync_for_tests(&paths.run_dir);
    let mut supervisor = Supervisor::new(paths.clone()).expect("diagnostic supervisor");
    assert_eq!(supervisor.status().pid, None);
    assert_eq!(supervisor.status().server_state, ServerState::Failed);
    assert!(
        supervisor
            .status()
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("Runtime state could not be saved"))
    );
    drop(directory_sync_failure);

    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    let pid = loop {
        supervisor.tick();
        if let Some(pid) = supervisor.status().pid {
            break pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "stale-record clear retry did not converge to startup"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(process_exists(pid));
    for _ in 0..10 {
        supervisor.tick();
        assert_eq!(
            supervisor.status().pid,
            Some(pid),
            "stale-record convergence must start exactly once"
        );
        thread::sleep(Duration::from_millis(20));
    }

    supervisor.handle(Command::Stop);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while supervisor.status().pid.is_some() {
        supervisor.tick();
        assert!(
            std::time::Instant::now() < deadline,
            "OpenCode did not stop after stale-record convergence"
        );
        thread::sleep(Duration::from_millis(20));
    }
    drop(supervisor);
    remove_support_dir(&paths);
}

#[test]
fn unreadable_runtime_state_stays_diagnostic_and_never_starts_default_open_code() {
    let paths = test_paths("unreadable");
    paths
        .ensure_directories()
        .expect("create support directory");
    let port = available_port();
    write_test_config(&paths, port);
    let corrupt_state = br"not valid runtime state".to_vec();
    fs::write(&paths.runtime_state, &corrupt_state).expect("write corrupt state");
    fs::set_permissions(&paths.runtime_state, fs::Permissions::from_mode(0o600))
        .expect("restrict corrupt state");

    let mut supervisor = Supervisor::new(paths.clone()).expect("diagnostic supervisor");
    let status = supervisor.status();
    assert_eq!(status.server_state, ServerState::Failed);
    assert_eq!(status.pid, None);
    assert!(
        status
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("Runtime state could not be loaded"))
    );
    let response = supervisor.handle(Command::Start);
    assert!(!response.ok, "an unreadable state must refuse Start");
    assert_eq!(supervisor.status().pid, None);
    assert_eq!(
        fs::read(&paths.runtime_state).expect("read corrupt state"),
        corrupt_state
    );

    drop(supervisor);
    remove_support_dir(&paths);
}

#[test]
fn intent_save_failure_does_not_start_or_signal_open_code() {
    let paths = test_paths("intent");
    paths
        .ensure_directories()
        .expect("create support directory");
    write_test_config(&paths, available_port());
    RuntimeState {
        desired_state: DesiredState::Stopped,
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("save stopped state");

    let mut supervisor = Supervisor::new(paths.clone()).expect("supervisor");
    let start_failure = fail_runtime_state_saves_for_tests(&paths.runtime_state);
    let response = supervisor.handle(Command::Start);
    assert!(!response.ok, "Start must report the durable-intent failure");
    assert_eq!(supervisor.status().pid, None);
    assert_eq!(supervisor.status().desired_state, DesiredState::Stopped);
    assert_eq!(
        RuntimeState::load(&paths)
            .expect("load state after refused start")
            .desired_state,
        DesiredState::Stopped
    );
    drop(start_failure);

    let marker_failure = fail_runtime_state_save_after_for_tests(&paths.runtime_state, 1);
    let response = supervisor.handle(Command::Start);
    assert!(
        !response.ok,
        "a launch marker save failure must refuse spawn before creating OpenCode"
    );
    assert_eq!(supervisor.status().pid, None);
    let durable_state = RuntimeState::load(&paths).expect("load state after marker failure");
    assert_eq!(durable_state.desired_state, DesiredState::Running);
    assert_eq!(durable_state.launch_pending, None);
    drop(marker_failure);

    supervisor.handle(Command::Start);
    let pid = supervisor
        .status()
        .pid
        .expect("OpenCode started after retry");
    assert!(process_exists(pid), "started OpenCode must be alive");

    let restart_failure = fail_runtime_state_saves_for_tests(&paths.runtime_state);
    let response = supervisor.handle(Command::Restart);
    assert!(
        !response.ok,
        "Restart must report the durable-intent failure"
    );
    assert_eq!(supervisor.status().pid, Some(pid));
    assert!(
        process_exists(pid),
        "failed Restart must not signal OpenCode"
    );
    drop(restart_failure);

    let stop_failure = fail_runtime_state_saves_for_tests(&paths.runtime_state);
    let response = supervisor.handle(Command::Stop);
    assert!(!response.ok, "Stop must report the durable-intent failure");
    assert_eq!(supervisor.status().pid, Some(pid));
    assert!(process_exists(pid), "failed Stop must not signal OpenCode");
    assert_eq!(
        RuntimeState::load(&paths)
            .expect("load state after refused stop")
            .desired_state,
        DesiredState::Running
    );
    drop(stop_failure);

    supervisor.handle(Command::Stop);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while supervisor.status().pid.is_some() {
        supervisor.tick();
        assert!(
            std::time::Instant::now() < deadline,
            "OpenCode did not stop"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(supervisor.status().server_state, ServerState::Stopped);

    drop(supervisor);
    remove_support_dir(&paths);
}

#[test]
fn uncertain_start_waits_for_durable_retry_and_runs_once() {
    let paths = test_paths("uncertain-start");
    paths
        .ensure_directories()
        .expect("create support directory");
    write_test_config(&paths, available_port());
    RuntimeState {
        desired_state: DesiredState::Stopped,
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("save stopped state");

    let mut supervisor = Supervisor::new(paths.clone()).expect("supervisor");
    let directory_sync_failure = fail_runtime_state_directory_sync_for_tests(&paths.run_dir);
    let response = supervisor.handle(Command::Start);
    assert!(
        !response.ok,
        "Start must wait for durable intent confirmation"
    );
    assert_eq!(supervisor.status().pid, None);
    assert_eq!(supervisor.status().desired_state, DesiredState::Running);
    assert!(
        supervisor
            .status()
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("Runtime state could not be saved"))
    );
    assert_eq!(
        RuntimeState::load(&paths)
            .expect("load renamed state")
            .desired_state,
        DesiredState::Running
    );
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        supervisor.status().pid,
        None,
        "an uncertain intent must not spawn before its directory sync is repaired"
    );
    drop(directory_sync_failure);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let pid = loop {
        supervisor.tick();
        if let Some(pid) = supervisor.status().pid {
            break pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "durable intent retry did not resume Start"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(process_exists(pid));
    for _ in 0..10 {
        supervisor.tick();
        assert_eq!(
            supervisor.status().pid,
            Some(pid),
            "the deferred Start must be consumed exactly once"
        );
        thread::sleep(Duration::from_millis(20));
    }

    supervisor.handle(Command::Stop);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while supervisor.status().pid.is_some() {
        supervisor.tick();
        assert!(
            std::time::Instant::now() < deadline,
            "OpenCode did not stop after deferred Start"
        );
        thread::sleep(Duration::from_millis(20));
    }
    drop(supervisor);
    remove_support_dir(&paths);
}

#[test]
fn uncertain_stop_waits_for_durable_retry_before_signaling() {
    let paths = test_paths("uncertain-stop");
    paths
        .ensure_directories()
        .expect("create support directory");
    write_test_config(&paths, available_port());
    RuntimeState::default()
        .save(&paths)
        .expect("save running state");

    let mut supervisor = Supervisor::new(paths.clone()).expect("supervisor");
    let pid = supervisor
        .status()
        .pid
        .expect("OpenCode started from default running state");
    let directory_sync_failure = fail_runtime_state_directory_sync_for_tests(&paths.run_dir);
    let response = supervisor.handle(Command::Stop);
    assert!(
        !response.ok,
        "Stop must wait for durable intent confirmation"
    );
    assert_eq!(supervisor.status().pid, Some(pid));
    thread::sleep(Duration::from_millis(100));
    assert!(
        process_exists(pid),
        "an uncertain Stop must not signal OpenCode before directory sync"
    );
    drop(directory_sync_failure);

    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    while supervisor.status().pid.is_some() {
        supervisor.tick();
        assert!(
            std::time::Instant::now() < deadline,
            "deferred Stop did not terminate OpenCode"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(supervisor.status().server_state, ServerState::Stopped);
    drop(supervisor);
    remove_support_dir(&paths);
}

#[test]
fn uncertain_restart_waits_for_durable_retry_before_signaling_once() {
    let paths = test_paths("uncertain-restart");
    paths
        .ensure_directories()
        .expect("create support directory");
    write_test_config(&paths, available_port());
    RuntimeState::default()
        .save(&paths)
        .expect("save running state");

    let mut supervisor = Supervisor::new(paths.clone()).expect("supervisor");
    let old_pid = supervisor
        .status()
        .pid
        .expect("OpenCode started from default running state");
    let directory_sync_failure = fail_runtime_state_directory_sync_for_tests(&paths.run_dir);
    let response = supervisor.handle(Command::Restart);
    assert!(
        !response.ok,
        "Restart must wait for durable intent confirmation"
    );
    assert_eq!(supervisor.status().pid, Some(old_pid));
    thread::sleep(Duration::from_millis(100));
    assert!(
        process_exists(old_pid),
        "an uncertain Restart must not signal OpenCode before directory sync"
    );
    drop(directory_sync_failure);

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let new_pid = loop {
        supervisor.tick();
        if let Some(pid) = supervisor.status().pid
            && pid != old_pid
        {
            break pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "deferred Restart did not replace OpenCode"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(process_exists(new_pid));
    supervisor.handle(Command::Stop);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while supervisor.status().pid.is_some() {
        supervisor.tick();
        assert!(
            std::time::Instant::now() < deadline,
            "OpenCode did not stop after deferred Restart"
        );
        thread::sleep(Duration::from_millis(20));
    }
    drop(supervisor);
    remove_support_dir(&paths);
}

#[test]
fn runtime_state_error_remains_visible_after_recovery_is_scheduled() {
    let paths = test_paths("notification-error-priority");
    paths
        .ensure_directories()
        .expect("create support directory");
    write_test_config(&paths, available_port());
    RuntimeState::default()
        .save(&paths)
        .expect("save running state");

    let mut supervisor = Supervisor::new(paths.clone()).expect("supervisor");
    let pid = supervisor
        .status()
        .pid
        .expect("OpenCode started from default running state");
    let notification_failure = fail_runtime_state_save_after_for_tests(&paths.runtime_state, 1);
    let snapshot = process_snapshot(pid).expect("snapshot OpenCode before unexpected exit");
    send_process_group_signal(snapshot.process_group_id, libc::SIGTERM)
        .expect("signal fixture OpenCode");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while supervisor.status().pid.is_some() {
        supervisor.tick();
        assert!(
            std::time::Instant::now() < deadline,
            "unexpected OpenCode exit was not observed"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        supervisor.status().server_state,
        ServerState::WaitingToRestart
    );
    assert!(
        supervisor
            .status()
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("Runtime state could not be saved")),
        "the runtime-state failure must outrank the recovery schedule text"
    );
    drop(notification_failure);

    // Clear the pending recovery intent without starting another fixture;
    // the durable Stop also proves the state fault can converge normally
    // after the failed notification save is repaired.
    let response = supervisor.handle(Command::Stop);
    assert!(
        response.ok,
        "Stop should recover after the save fault is removed"
    );
    assert_eq!(supervisor.status().server_state, ServerState::Stopped);
    drop(supervisor);
    remove_support_dir(&paths);
}

#[test]
fn recovery_waits_for_metadata_durability_without_resetting_budget() {
    let paths = test_paths("recovery-metadata-retry");
    paths
        .ensure_directories()
        .expect("create support directory");
    write_test_config(&paths, available_port());
    RuntimeState::default()
        .save(&paths)
        .expect("save running state");

    let mut supervisor = Supervisor::new(paths.clone()).expect("supervisor");
    let first_pid = supervisor
        .status()
        .pid
        .expect("OpenCode started from default running state");
    // Let the process-record clear through, then fail the notification and
    // every metadata retry. The first recovery backoff must not lose its
    // Recovery trigger merely because runtime-state readiness is false.
    let metadata_failure = fail_runtime_state_save_after_for_tests(&paths.runtime_state, 1);
    let snapshot = process_snapshot(first_pid).expect("snapshot OpenCode");
    send_process_group_signal(snapshot.process_group_id, libc::SIGTERM)
        .expect("signal fixture OpenCode");

    let exit_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while supervisor.status().pid.is_some() {
        supervisor.tick();
        assert!(
            std::time::Instant::now() < exit_deadline,
            "unexpected OpenCode exit was not observed"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        supervisor.status().server_state,
        ServerState::WaitingToRestart
    );

    let first_backoff_deadline = std::time::Instant::now() + Duration::from_millis(1_300);
    while std::time::Instant::now() < first_backoff_deadline {
        supervisor.tick();
        assert_eq!(
            supervisor.status().pid,
            None,
            "Recovery must remain blocked while metadata is not durable"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(supervisor.status().server_state, ServerState::Failed);
    assert!(
        supervisor
            .status()
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("Runtime state could not be saved"))
    );
    drop(metadata_failure);

    let recovery_deadline = std::time::Instant::now() + Duration::from_secs(6);
    let recovered_pid = loop {
        supervisor.tick();
        if let Some(pid) = supervisor.status().pid {
            break pid;
        }
        assert!(
            std::time::Instant::now() < recovery_deadline,
            "Recovery did not resume after metadata durability returned"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert_ne!(recovered_pid, first_pid);
    assert!(process_exists(recovered_pid));

    let healthy_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while supervisor.status().server_state != ServerState::Healthy {
        supervisor.tick();
        assert!(
            std::time::Instant::now() < healthy_deadline,
            "recovered OpenCode did not become healthy"
        );
        thread::sleep(Duration::from_millis(20));
    }

    // A second unexpected exit must use attempt 2, proving that the
    // durability wait did not reset the existing bounded recovery budget.
    let snapshot = process_snapshot(recovered_pid).expect("snapshot recovered OpenCode");
    send_process_group_signal(snapshot.process_group_id, libc::SIGTERM)
        .expect("signal recovered fixture OpenCode");
    let second_exit_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        supervisor.tick();
        let status = supervisor.status();
        if status.server_state == ServerState::WaitingToRestart {
            assert!(
                status
                    .last_error
                    .as_deref()
                    .is_some_and(|error| error.contains("attempt 2 of 5")),
                "recovery budget reset unexpectedly: {status:?}"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < second_exit_deadline,
            "second unexpected exit did not schedule recovery"
        );
        thread::sleep(Duration::from_millis(20));
    }

    let response = supervisor.handle(Command::Stop);
    assert!(
        response.ok,
        "Stop should cancel the second recovery attempt"
    );
    assert_eq!(supervisor.status().server_state, ServerState::Stopped);
    drop(supervisor);
    remove_support_dir(&paths);
}

#[test]
fn process_identity_save_failure_keeps_child_owned_until_stop_is_durable() {
    let paths = test_paths("identity");
    paths
        .ensure_directories()
        .expect("create support directory");
    write_test_config(&paths, available_port());
    RuntimeState {
        desired_state: DesiredState::Stopped,
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("save stopped state");

    let mut supervisor = Supervisor::new(paths.clone()).expect("supervisor");
    // The first save records the Running intent and the second save records
    // the pre-spawn launch marker. The post-spawn process identity write is
    // deliberately failed, leaving that marker durable on disk.
    let identity_failure = fail_runtime_state_save_after_for_tests(&paths.runtime_state, 2);
    let response = supervisor.handle(Command::Start);
    assert!(
        !response.ok,
        "identity persistence failure must be reported"
    );
    let pid = supervisor
        .status()
        .pid
        .expect("the created child remains attached in memory");
    assert_eq!(supervisor.status().server_state, ServerState::Failed);
    assert!(process_exists(pid), "the child must remain supervised");
    assert_eq!(
        RuntimeState::load(&paths)
            .expect("load state after identity-write failure")
            .process,
        None,
        "the old durable state must remain unchanged"
    );
    assert!(
        RuntimeState::load(&paths)
            .expect("load state after identity-write failure")
            .launch_pending
            .is_some(),
        "the durable launch marker must protect a replacement OpenCodeServerAgent"
    );

    let response = supervisor.handle(Command::Stop);
    assert!(
        !response.ok,
        "Stop cannot proceed while its save still fails"
    );
    assert!(
        process_exists(pid),
        "failed Stop must not abandon the child"
    );
    drop(identity_failure);

    supervisor.handle(Command::Stop);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while supervisor.status().pid.is_some() {
        supervisor.tick();
        assert!(
            std::time::Instant::now() < deadline,
            "OpenCode did not stop"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(supervisor.status().server_state, ServerState::Stopped);

    drop(supervisor);
    remove_support_dir(&paths);
}

#[test]
fn process_record_clear_failure_keeps_stop_intent_and_blocks_recovery() {
    let paths = test_paths("clear-record");
    paths
        .ensure_directories()
        .expect("create support directory");
    let port_reservation = PortReservation::for_paths(&paths);
    write_test_config_with_executable(&paths, port_reservation.port, &port_reservation.executable);
    RuntimeState {
        desired_state: DesiredState::Stopped,
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("save stopped state");

    let mut supervisor = Supervisor::new(paths.clone()).expect("supervisor");
    let start_response = supervisor.handle(Command::Start);
    assert!(
        start_response.ok,
        "OpenCode Start failed before clear-failure injection: {:?}",
        start_response.error
    );
    let pid = supervisor
        .status()
        .pid
        .expect("OpenCode started before clear-failure injection");
    port_reservation.release();

    // The desired Stop write succeeds, then the process-record clear after
    // the graceful exit fails. The durable record must remain as the
    // evidence that the exited process still needs reconciliation.
    let clear_failure = fail_runtime_state_save_after_for_tests(&paths.runtime_state, 1);
    let response = supervisor.handle(Command::Stop);
    assert!(
        response.ok,
        "Stop intent should be durable before signaling"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while supervisor.status().pid.is_some() {
        supervisor.tick();
        assert!(
            std::time::Instant::now() < deadline,
            "OpenCode did not exit after the explicit Stop"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(supervisor.status().server_state, ServerState::Failed);
    assert_eq!(supervisor.status().desired_state, DesiredState::Stopped);
    assert!(!process_exists(pid), "the stopped OpenCode must be gone");
    let durable_state = RuntimeState::load(&paths).expect("load uncleared process record");
    assert_eq!(durable_state.desired_state, DesiredState::Stopped);
    assert!(
        durable_state.process.is_some(),
        "failed record clear must preserve the old process identity"
    );

    // While the storage fault remains, no recovery launch may occur. Once it
    // is repaired, the stale record is cleared durably and the Stop intent
    // converges to Stopped without starting a replacement OpenCode.
    thread::sleep(Duration::from_millis(100));
    assert_eq!(supervisor.status().pid, None);
    drop(clear_failure);
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    while supervisor.status().server_state != ServerState::Stopped {
        supervisor.tick();
        assert_eq!(
            supervisor.status().pid,
            None,
            "record-clear recovery must not start a second OpenCode"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "runtime state did not converge after the clear fault was repaired"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let durable_state = RuntimeState::load(&paths).expect("load cleared process record");
    assert_eq!(durable_state.process, None);

    drop(supervisor);
    remove_support_dir(&paths);
}

#[test]
fn failed_spawn_cannot_retry_until_its_launch_marker_clear_is_durable() {
    let paths = test_paths("spawn-clear");
    paths
        .ensure_directories()
        .expect("create support directory");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind port conflict");
    let port = listener.local_addr().expect("read port conflict").port();
    write_test_config(&paths, port);
    RuntimeState {
        desired_state: DesiredState::Stopped,
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("save stopped state");

    let mut supervisor = Supervisor::new(paths.clone()).expect("supervisor");
    // Constructor save is already complete. Allow the Running intent and
    // launch marker, then fail the clear after the preflight port conflict.
    let clear_failure = fail_runtime_state_save_after_for_tests(&paths.runtime_state, 2);
    let response = supervisor.handle(Command::Start);
    assert!(!response.ok, "a failed marker clear must be reported");
    assert_eq!(supervisor.status().pid, None);
    let durable_state = RuntimeState::load(&paths).expect("load uncleared launch marker");
    assert!(durable_state.launch_pending.is_some());

    let replacement = Supervisor::new(paths.clone()).expect("replacement supervisor");
    assert_eq!(replacement.status().pid, None);
    assert_eq!(replacement.status().server_state, ServerState::Failed);
    drop(replacement);
    drop(clear_failure);
    drop(listener);

    let response = supervisor.handle(Command::Start);
    assert!(
        response.ok,
        "retry should proceed after the marker clear works"
    );
    let pid = supervisor
        .status()
        .pid
        .expect("OpenCode started after durable marker clear");
    assert!(process_exists(pid));
    supervisor.handle(Command::Stop);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while supervisor.status().pid.is_some() {
        supervisor.tick();
        assert!(
            std::time::Instant::now() < deadline,
            "OpenCode did not stop after marker-clear retry"
        );
        thread::sleep(Duration::from_millis(20));
    }
    drop(supervisor);
    remove_support_dir(&paths);
}

#[test]
fn replacement_agent_does_not_start_a_second_child_with_a_pending_launch_marker() {
    let paths = test_paths("replacement");
    paths
        .ensure_directories()
        .expect("create support directory");
    let port = available_port();
    write_test_config(&paths, port);
    RuntimeState {
        desired_state: DesiredState::Stopped,
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("save stopped state");

    let mut supervisor = Supervisor::new(paths.clone()).expect("supervisor");
    let identity_failure = fail_runtime_state_save_after_for_tests(&paths.runtime_state, 2);
    let response = supervisor.handle(Command::Start);
    assert!(!response.ok);
    let old_pid = supervisor.status().pid.expect("old child remains owned");
    let durable_state = RuntimeState::load(&paths).expect("load pending launch state");
    assert!(durable_state.launch_pending.is_some());
    assert_eq!(durable_state.process, None);
    drop(identity_failure);
    // A replacement OpenCodeServerAgent must fail closed before any spawn
    // rather than infer that the missing process record means no child was
    // created. The original OpenCodeServerAgent still owns the child in this
    // process, while the marker covers the crash/restart interval on disk.
    let replacement = Supervisor::new(paths.clone()).expect("replacement supervisor");
    assert_eq!(replacement.status().pid, None);
    assert_eq!(replacement.status().server_state, ServerState::Failed);
    assert!(process_exists(old_pid), "the original child remains alive");
    drop(replacement);
    supervisor.handle(Command::Stop);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while supervisor.status().pid.is_some() {
        supervisor.tick();
        assert!(
            std::time::Instant::now() < deadline,
            "old child did not stop"
        );
        thread::sleep(Duration::from_millis(20));
    }
    remove_support_dir(&paths);
}
