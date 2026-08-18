//! Characterization tests for the installed-version query mechanism, written
//! against the unmodified `Supervisor` before the VersionQueryCoordinator
//! extraction. They lock current behavior so the refactor can be proven
//! behavior-preserving.
//!
//! Every test drives the in-flight branch of `poll_version_query` through a
//! hand-built `VersionQueryInFlight` channel, so no subprocess is ever
//! spawned in-process. The idle→spawn path is covered by the integration
//! suite (single-flight, escape breaker, shutdown drain, closed-stdout
//! cases) and becomes directly unit-testable after the extraction injects
//! the query function.

use super::*;
use crate::config::{ConfigFile, write_config_atomically};

/// A Supervisor over private temp paths with a unique account and
/// `DesiredState::Stopped`, so no OpenCode is ever spawned. The configured
/// executable is this test binary — the path the still-current check will
/// compare against. Returns the root directory for cleanup.
fn test_supervisor(tag: &str) -> (Supervisor, std::path::PathBuf) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "opencodeserver-version-characterization-{tag}-{}-{nonce}",
        std::process::id()
    ));
    let paths = AppPaths::from_support_dir(root.clone());
    paths.ensure_directories().expect("private test paths");
    write_config_atomically(
        &paths.config_file,
        &ConfigFile {
            username: format!("vq-{tag}"),
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
    let supervisor = Supervisor::new(paths).expect("supervisor");
    (supervisor, root)
}

fn finish(supervisor: Supervisor, root: std::path::PathBuf) {
    drop(supervisor);
    std::fs::remove_dir_all(root).expect("remove test root");
}

fn configured_executable() -> std::path::PathBuf {
    std::env::current_exe().expect("test executable")
}

/// Seeds an in-flight query whose channel the test still controls, for the
/// configured executable (so the still-current check accepts results).
fn seed_in_flight(supervisor: &mut Supervisor) -> std::sync::mpsc::Sender<VersionQueryResult> {
    let (sender, receiver) = mpsc::channel();
    supervisor
        .version_queries
        .set_in_flight_for_test(VersionQueryInFlight {
            dispatched: Instant::now(),
            generation: "characterization".to_owned(),
            executable: configured_executable(),
            worker: None,
            receiver,
        });
    sender
}

// ────────────────────────────────────────────────────────────────────
// due(): retry interval and circuit breaker
// ────────────────────────────────────────────────────────────────────

#[test]
fn due_uses_retry_interval_until_a_version_is_installed() {
    let (mut supervisor, root) = test_supervisor("interval");
    let now = Instant::now();
    assert!(
        supervisor.version_query_due(now),
        "no attempt recorded yet: due immediately"
    );

    supervisor
        .version_queries
        .set_last_attempt_for_test(Some(now - Duration::from_secs(30)));
    assert!(
        supervisor.version_query_due(now),
        "30s exceeds the 5s retry interval while no version is installed"
    );
    supervisor.installed_version = Some("1.0.0".to_owned());
    assert!(
        !supervisor.version_query_due(now),
        "with a version installed, 30s does not reach the 60s interval"
    );
    supervisor
        .version_queries
        .set_last_attempt_for_test(Some(now - Duration::from_secs(70)));
    assert!(
        supervisor.version_query_due(now),
        "70s reaches the 60s interval once a version is installed"
    );

    supervisor.latest_config = None;
    assert!(
        !supervisor.version_query_due(now),
        "without configuration nothing is due"
    );
    finish(supervisor, root);
}

#[test]
fn quarantined_executable_blocks_due_until_config_moves() {
    let (mut supervisor, root) = test_supervisor("breaker");
    let now = Instant::now();
    let other = std::path::PathBuf::from("/not/the/configured/executable");

    // A quarantine for a DIFFERENT path does not block.
    supervisor
        .version_queries
        .set_quarantined_for_test(Some(other));
    assert!(supervisor.version_query_due(now));

    // Quarantining the configured executable blocks even with no attempt
    // recorded.
    supervisor
        .version_queries
        .set_quarantined_for_test(Some(configured_executable()));
    assert!(!supervisor.version_query_due(now));

    // Moving the configuration to another executable re-arms the breaker.
    supervisor
        .latest_config
        .as_mut()
        .expect("latest config")
        .configured_executable = std::path::PathBuf::from("/some/other/path");
    assert!(supervisor.version_query_due(now));
    finish(supervisor, root);
}

// ────────────────────────────────────────────────────────────────────
// The three result arms
// ────────────────────────────────────────────────────────────────────

#[test]
fn poll_applies_available_version_and_backfills_running() {
    let (mut supervisor, root) = test_supervisor("available");
    let sender = seed_in_flight(&mut supervisor);
    sender
        .send(VersionQueryResult::Available("9.8.7".to_owned()))
        .expect("worker result");
    drop(sender);

    supervisor.poll_version_query(Instant::now());

    assert_eq!(supervisor.installed_version.as_deref(), Some("9.8.7"));
    assert_eq!(
        supervisor.running_version.as_deref(),
        Some("9.8.7"),
        "a running version is backfilled from the first installed one"
    );
    assert!(supervisor.version_queries.last_attempt_for_test().is_some());
    assert!(supervisor.version_queries.quarantined_for_test().is_none());
    assert!(supervisor.version_queries.in_flight_for_test().is_none());
    finish(supervisor, root);
}

#[test]
fn poll_applies_unavailable_without_touching_versions() {
    let (mut supervisor, root) = test_supervisor("unavailable");
    let sender = seed_in_flight(&mut supervisor);
    sender
        .send(VersionQueryResult::Unavailable)
        .expect("worker result");
    drop(sender);

    supervisor.poll_version_query(Instant::now());

    assert!(supervisor.installed_version.is_none());
    assert!(supervisor.running_version.is_none());
    assert!(supervisor.version_queries.last_attempt_for_test().is_some());
    assert!(supervisor.version_queries.in_flight_for_test().is_none());
    finish(supervisor, root);
}

#[test]
fn poll_quarantined_clears_installed_and_opens_the_breaker() {
    let (mut supervisor, root) = test_supervisor("quarantined");
    supervisor.installed_version = Some("1.0.0".to_owned());
    let sender = seed_in_flight(&mut supervisor);
    sender
        .send(VersionQueryResult::Quarantined)
        .expect("worker result");
    drop(sender);

    supervisor.poll_version_query(Instant::now());

    assert_eq!(
        supervisor.installed_version, None,
        "the informational label must not claim a version after an identity anomaly"
    );
    assert_eq!(
        supervisor.version_queries.quarantined_for_test().cloned(),
        Some(configured_executable())
    );
    assert!(supervisor.version_queries.last_attempt_for_test().is_some());
    assert!(
        !supervisor.version_query_due(Instant::now()),
        "the breaker blocks further queries for this executable"
    );
    finish(supervisor, root);
}

#[test]
fn poll_discards_a_result_when_the_executable_changed() {
    let (mut supervisor, root) = test_supervisor("stale-executable");
    let (sender, receiver) = mpsc::channel();
    supervisor
        .version_queries
        .set_in_flight_for_test(VersionQueryInFlight {
            dispatched: Instant::now(),
            generation: "characterization".to_owned(),
            executable: std::path::PathBuf::from("/the/old/configured/executable"),
            worker: None,
            receiver,
        });
    sender
        .send(VersionQueryResult::Available("2.0.0".to_owned()))
        .expect("worker result");
    drop(sender);

    supervisor.poll_version_query(Instant::now());

    assert_eq!(
        supervisor.installed_version, None,
        "a result for a replaced executable must not be applied"
    );
    assert!(supervisor.version_queries.last_attempt_for_test().is_some());
    assert!(supervisor.version_queries.in_flight_for_test().is_none());
    finish(supervisor, root);
}

// ────────────────────────────────────────────────────────────────────
// Worker bookkeeping
// ────────────────────────────────────────────────────────────────────

#[test]
fn poll_handles_a_disconnected_worker() {
    let (mut supervisor, root) = test_supervisor("disconnected");
    supervisor.version_queries.set_overdue_logged_for_test(true);
    let (_sender, receiver) = mpsc::channel::<VersionQueryResult>();
    supervisor
        .version_queries
        .set_in_flight_for_test(VersionQueryInFlight {
            dispatched: Instant::now(),
            generation: "characterization".to_owned(),
            executable: configured_executable(),
            worker: None,
            receiver,
        });
    drop(_sender);

    supervisor.poll_version_query(Instant::now());

    assert!(supervisor.version_queries.in_flight_for_test().is_none());
    assert!(supervisor.version_queries.last_attempt_for_test().is_some());
    assert!(!supervisor.version_queries.overdue_logged_for_test());
    finish(supervisor, root);
}

#[test]
fn overdue_latch_fires_once_per_query() {
    let (mut supervisor, root) = test_supervisor("overdue");
    let (sender, receiver) = mpsc::channel();
    supervisor
        .version_queries
        .set_in_flight_for_test(VersionQueryInFlight {
            dispatched: Instant::now() - DEFAULT_VERSION_QUERY_TIMEOUT - Duration::from_secs(60),
            generation: "characterization".to_owned(),
            executable: configured_executable(),
            worker: None,
            receiver,
        });

    supervisor.poll_version_query(Instant::now());
    assert!(supervisor.version_queries.in_flight_for_test().is_some());
    assert!(
        supervisor.version_queries.overdue_logged_for_test(),
        "the latch sets once the worker exceeds twice the observation bound"
    );
    supervisor.poll_version_query(Instant::now());
    assert!(
        supervisor.version_queries.overdue_logged_for_test(),
        "the latch stays set while the same worker stays pending"
    );

    sender
        .send(VersionQueryResult::Unavailable)
        .expect("worker result");
    drop(sender);
    supervisor.poll_version_query(Instant::now());
    assert!(
        !supervisor.version_queries.overdue_logged_for_test(),
        "completion resets the latch for the next query"
    );
    finish(supervisor, root);
}

#[test]
fn single_flight_prevents_a_second_spawn_while_in_flight() {
    let (mut supervisor, root) = test_supervisor("single-flight");
    // No attempt recorded: if poll ignored the in-flight record it would
    // consider the query due and spawn a real worker.
    let sender = seed_in_flight(&mut supervisor);

    supervisor.poll_version_query(Instant::now());

    let in_flight = supervisor
        .version_queries
        .in_flight_for_test()
        .as_ref()
        .expect("still in flight");
    assert!(
        in_flight.worker.is_none(),
        "the seeded record must be kept, not replaced by a spawned worker"
    );
    // The original channel still drives the original record.
    sender
        .send(VersionQueryResult::Available("4.5.6".to_owned()))
        .expect("original worker result");
    drop(sender);
    supervisor.poll_version_query(Instant::now());
    assert_eq!(supervisor.installed_version.as_deref(), Some("4.5.6"));
    finish(supervisor, root);
}

#[test]
fn shutdown_drains_the_in_flight_query() {
    let (mut supervisor, root) = test_supervisor("shutdown-drain");
    let sender = seed_in_flight(&mut supervisor);
    sender
        .send(VersionQueryResult::Available("3.2.1".to_owned()))
        .expect("worker result");
    drop(sender);

    supervisor.finish_version_query_for_shutdown();

    assert!(
        supervisor.version_queries.in_flight_for_test().is_none(),
        "the drain loop must converge once the worker reports"
    );
    assert_eq!(supervisor.installed_version.as_deref(), Some("3.2.1"));
    finish(supervisor, root);
}
