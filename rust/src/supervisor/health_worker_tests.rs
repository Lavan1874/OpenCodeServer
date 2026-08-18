use super::health_worker::*;
use crate::config::{ConfigFile, ValidatedConfig};
use crate::config_fingerprint::ConfigFingerprint;
use crate::health::{HealthError, HealthResult};
use crate::runtime_state::ProcessRecord;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

fn gate() -> &'static (Mutex<bool>, Condvar) {
    static GATE: OnceLock<(Mutex<bool>, Condvar)> = OnceLock::new();
    GATE.get_or_init(|| (Mutex::new(false), Condvar::new()))
}

fn set_gate(released: bool) {
    let (lock, condition) = gate();
    *lock.lock().expect("health worker gate") = released;
    condition.notify_all();
}

fn controlled_check(_: &ValidatedConfig, _: Duration) -> Result<HealthResult, HealthError> {
    let (lock, condition) = gate();
    let mut released = lock.lock().expect("health worker gate");
    while !*released {
        released = condition.wait(released).expect("health worker gate wait");
    }
    Ok(HealthResult {
        healthy: true,
        version: "fixture".to_owned(),
    })
}

fn config() -> ValidatedConfig {
    ValidatedConfig {
        source: ConfigFile::default(),
        configured_executable: PathBuf::from("/tmp/opencode"),
        canonical_executable: PathBuf::from("/tmp/opencode"),
        effective_username: "opencode".to_owned(),
    }
}

fn record(pid: u32) -> ProcessRecord {
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
            hmac_sha256: format!("{pid:064x}"),
        },
        identity_unconfirmed: false,
    }
}

#[test]
fn health_worker_is_single_flight_and_drops_a_b_a_stale_result() {
    set_gate(false);
    let mut coordinator = HealthCheckCoordinator::new(controlled_check, Duration::from_secs(1));
    let first_config = config();
    let first_key = HealthCheckKey::from_record(
        &record(11),
        ConfigFingerprint {
            version: 1,
            hmac_sha256: "a".repeat(64),
        },
    )
    .with_generation(1);
    let returned_key = first_key.clone().with_generation(3);
    let now = Instant::now();
    assert!(coordinator.dispatch(now, first_key, first_config, Duration::from_secs(1)));
    assert!(!coordinator.dispatch(now, returned_key.clone(), config(), Duration::from_secs(1)));

    // A -> B -> A is represented by generation 1 -> 2 -> 3. The final
    // A fingerprint alone is not enough to accept the old generation-1
    // answer.
    let overdue_at = now + Duration::from_secs(2);
    assert!(coordinator.poll(overdue_at, Some(&returned_key)).is_none());
    assert!(coordinator.overdue_logged());
    set_gate(true);
    let deadline = Instant::now() + Duration::from_secs(2);
    while coordinator.in_flight() {
        assert!(Instant::now() < deadline, "health worker did not finish");
        let _ = coordinator.poll(Instant::now(), Some(&returned_key));
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!coordinator.overdue_logged());
}
