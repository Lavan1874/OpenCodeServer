use crate::config_fingerprint::ConfigFingerprint;
use crate::paths::AppPaths;
use crate::platform::effective_uid;
use crate::protocol::{DesiredState, NotificationEvent};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "test-fixture")]
use std::path::PathBuf;
#[cfg(feature = "test-fixture")]
use std::sync::Mutex;
#[cfg(feature = "test-fixture")]
use std::sync::atomic::{AtomicUsize, Ordering};

const MAX_STATE_BYTES: u64 = 16 * 1024;

#[cfg(feature = "test-fixture")]
struct TestSaveFailure {
    path: PathBuf,
    /// `None` means fail every save. `Some(n)` lets exactly `n` saves through
    /// before failing, which targets a lifecycle phase such as the process
    /// identity write after the desired-state write.
    successful_saves_before_failure: Option<usize>,
}

#[cfg(feature = "test-fixture")]
static TEST_SAVE_FAILURES: Mutex<Vec<TestSaveFailure>> = Mutex::new(Vec::new());

#[cfg(feature = "test-fixture")]
struct TestDirectorySyncFailure {
    path: PathBuf,
    /// `None` means fail every directory sync. `Some(n)` allows exactly `n`
    /// syncs through before failing every later sync.
    successful_syncs_before_failure: Option<usize>,
}

#[cfg(feature = "test-fixture")]
static TEST_DIRECTORY_SYNC_FAILURES: Mutex<Vec<TestDirectorySyncFailure>> = Mutex::new(Vec::new());

/// External integration tests launch a real OpenCodeServerAgent process, so
/// the in-process guard above cannot reach it. This counter is a test-fixture
/// only boundary: when the child process receives the named environment
/// variable, exactly that process fails saves starting at the requested save
/// number. Production builds do not compile or read this hook.
#[cfg(feature = "test-fixture")]
static TEST_EXTERNAL_SAVE_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Test-only save-failure injection. It is compiled only with the
/// `test-fixture` feature and is never part of a production build.
#[cfg(feature = "test-fixture")]
pub struct RuntimeStateSaveFailureGuard {
    path: PathBuf,
}

#[cfg(feature = "test-fixture")]
impl Drop for RuntimeStateSaveFailureGuard {
    fn drop(&mut self) {
        let mut failures = TEST_SAVE_FAILURES
            .lock()
            .expect("runtime-state test hook mutex");
        failures.retain(|failure| failure.path != self.path);
    }
}

/// Forces `RuntimeState::save` to fail for one exact test path until the
/// returned guard is dropped. This exists solely to exercise the production
/// transaction failure path without deleting or corrupting a real state file.
#[cfg(feature = "test-fixture")]
pub fn fail_runtime_state_saves_for_tests(path: &Path) -> RuntimeStateSaveFailureGuard {
    let path = path.to_owned();
    TEST_SAVE_FAILURES
        .lock()
        .expect("runtime-state test hook mutex")
        .push(TestSaveFailure {
            path: path.clone(),
            successful_saves_before_failure: None,
        });
    RuntimeStateSaveFailureGuard { path }
}

/// Allows a bounded number of saves for one exact test path, then fails every
/// later save until the guard is dropped. This targets a specific lifecycle
/// persistence point without weakening the production storage abstraction.
#[cfg(feature = "test-fixture")]
pub fn fail_runtime_state_save_after_for_tests(
    path: &Path,
    successful_saves_before_failure: usize,
) -> RuntimeStateSaveFailureGuard {
    let path = path.to_owned();
    TEST_SAVE_FAILURES
        .lock()
        .expect("runtime-state test hook mutex")
        .push(TestSaveFailure {
            path: path.clone(),
            successful_saves_before_failure: Some(successful_saves_before_failure),
        });
    RuntimeStateSaveFailureGuard { path }
}

/// Test-only failure injection for the parent-directory sync that follows a
/// successful state-file rename. The guard is path-scoped so parallel tests
/// can use different support directories without weakening production I/O.
#[cfg(feature = "test-fixture")]
pub struct RuntimeStateDirectorySyncFailureGuard {
    path: PathBuf,
}

#[cfg(feature = "test-fixture")]
impl Drop for RuntimeStateDirectorySyncFailureGuard {
    fn drop(&mut self) {
        TEST_DIRECTORY_SYNC_FAILURES
            .lock()
            .expect("runtime-state directory-sync test hook mutex")
            .retain(|failure| failure.path != self.path);
    }
}

#[cfg(feature = "test-fixture")]
pub fn fail_runtime_state_directory_sync_for_tests(
    path: &Path,
) -> RuntimeStateDirectorySyncFailureGuard {
    let path = path.to_owned();
    TEST_DIRECTORY_SYNC_FAILURES
        .lock()
        .expect("runtime-state directory-sync test hook mutex")
        .push(TestDirectorySyncFailure {
            path: path.clone(),
            successful_syncs_before_failure: None,
        });
    RuntimeStateDirectorySyncFailureGuard { path }
}

/// Allows a bounded number of parent-directory syncs for one exact test path,
/// then fails every later sync until the guard is dropped. This targets a
/// later lifecycle phase such as the pre-spawn marker rather than the desired
/// state write that precedes it.
#[cfg(feature = "test-fixture")]
pub fn fail_runtime_state_directory_sync_after_for_tests(
    path: &Path,
    successful_syncs_before_failure: usize,
) -> RuntimeStateDirectorySyncFailureGuard {
    let path = path.to_owned();
    TEST_DIRECTORY_SYNC_FAILURES
        .lock()
        .expect("runtime-state directory-sync test hook mutex")
        .push(TestDirectorySyncFailure {
            path: path.clone(),
            successful_syncs_before_failure: Some(successful_syncs_before_failure),
        });
    RuntimeStateDirectorySyncFailureGuard { path }
}

#[cfg(feature = "test-fixture")]
fn test_save_failure_requested(path: &Path) -> bool {
    let mut failures = TEST_SAVE_FAILURES
        .lock()
        .expect("runtime-state test hook mutex");
    let Some(failure) = failures.iter_mut().find(|failure| failure.path == path) else {
        return false;
    };
    match failure.successful_saves_before_failure.as_mut() {
        None => true,
        Some(remaining) if *remaining == 0 => true,
        Some(remaining) => {
            *remaining -= 1;
            false
        }
    }
}

#[cfg(feature = "test-fixture")]
fn test_directory_sync_failure_requested(path: &Path) -> bool {
    let mut failures = TEST_DIRECTORY_SYNC_FAILURES
        .lock()
        .expect("runtime-state directory-sync test hook mutex");
    let Some(failure) = failures.iter_mut().find(|failure| failure.path == path) else {
        return false;
    };
    match failure.successful_syncs_before_failure.as_mut() {
        None => true,
        Some(remaining) if *remaining == 0 => true,
        Some(remaining) => {
            *remaining -= 1;
            false
        }
    }
}

#[cfg(feature = "test-fixture")]
fn external_test_save_failure_requested() -> bool {
    let Some(limit) = std::env::var_os("OPENCODESERVER_TEST_RUNTIME_STATE_FAIL_AFTER") else {
        return false;
    };
    let Ok(limit) = limit.to_string_lossy().parse::<usize>() else {
        return false;
    };
    TEST_EXTERNAL_SAVE_COUNT.fetch_add(1, Ordering::Relaxed) >= limit
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRecord {
    pub pid: u32,
    pub process_group_id: u32,
    pub start_seconds: u64,
    pub start_microseconds: u64,
    pub executable: String,
    pub started_at_unix_seconds: u64,
    pub running_version: Option<String>,
    pub config_fingerprint: ConfigFingerprint,
    /// True only for a spawn-time survivor whose kernel identity was never
    /// observed (the identity snapshot never succeeded). Such a record can
    /// authorize no signal and no takeover: on OpenCodeServerAgent restart
    /// it is kept as unverified while its PID lives, and discarded only when
    /// the PID is provably gone.
    pub identity_unconfirmed: bool,
}

/// Durable two-phase marker written immediately before OpenCodeServerAgent
/// creates an OpenCode child. A later OpenCodeServerAgent must not interpret a
/// missing process record as proof that no child was created while this marker
/// is present.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchPending {
    pub executable: String,
    pub config_fingerprint: ConfigFingerprint,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeState {
    pub schema_version: u32,
    pub desired_state: DesiredState,
    pub process: Option<ProcessRecord>,
    /// Optional so existing current-schema files remain readable without a
    /// migration or compatibility branch. `Some` is fail-closed evidence of
    /// an interrupted launch transaction.
    #[serde(default)]
    pub launch_pending: Option<LaunchPending>,
    pub notification: Option<NotificationEvent>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            schema_version: 2,
            desired_state: DesiredState::Running,
            process: None,
            launch_pending: None,
            notification: None,
        }
    }
}

impl RuntimeState {
    pub fn load(paths: &AppPaths) -> io::Result<Self> {
        let path = &paths.runtime_state;
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "runtime state must be a regular file",
            ));
        }
        if metadata.len() > MAX_STATE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "runtime state is too large",
            ));
        }
        // Same owner/mode validation as config.plist and the fingerprint
        // key. The contents are not secret, so this is defense-in-depth
        // consistency: a foreign-written state file must not steer
        // supervision.
        if metadata.uid() != effective_uid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "runtime state must be owned by the current user",
            ));
        }
        if metadata.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "runtime state must have mode 0600",
            ));
        }
        let mut file = fs::File::open(path)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        let state: Self = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if state.schema_version != 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported runtime state schema",
            ));
        }
        Ok(state)
    }

    pub fn save(&self, paths: &AppPaths) -> io::Result<()> {
        match self.save_with_durability(paths) {
            RuntimeStateSaveOutcome::Durable => Ok(()),
            RuntimeStateSaveOutcome::Failed(error) | RuntimeStateSaveOutcome::Uncertain(error) => {
                Err(error)
            }
        }
    }

    pub fn save_with_durability(&self, paths: &AppPaths) -> RuntimeStateSaveOutcome {
        #[cfg(feature = "test-fixture")]
        if test_save_failure_requested(&paths.runtime_state)
            || external_test_save_failure_requested()
        {
            return RuntimeStateSaveOutcome::Failed(io::Error::other(
                "test-injected runtime state save failure",
            ));
        }
        write_json_atomically(&paths.runtime_state, self)
    }
}

#[derive(Debug)]
pub enum RuntimeStateSaveOutcome {
    Durable,
    /// The old state remains the only known directory entry. No lifecycle
    /// transition may proceed from this result.
    Failed(io::Error),
    /// The rename succeeded, but the directory entry could not be synced.
    /// The new state may already be visible, so callers must not restore an
    /// older in-memory value; a retry reconciles durability.
    Uncertain(io::Error),
}

fn write_json_atomically(path: &Path, state: &RuntimeState) -> RuntimeStateSaveOutcome {
    let parent = match path.parent() {
        Some(parent) => parent,
        None => {
            return RuntimeStateSaveOutcome::Failed(io::Error::new(
                io::ErrorKind::InvalidInput,
                "state path has no parent",
            ));
        }
    };
    if let Err(error) = fs::create_dir_all(parent) {
        return RuntimeStateSaveOutcome::Failed(error);
    }
    if let Err(error) = fs::set_permissions(parent, fs::Permissions::from_mode(0o700)) {
        return RuntimeStateSaveOutcome::Failed(error);
    }
    let bytes = match serde_json::to_vec(state) {
        Ok(bytes) => bytes,
        Err(error) => {
            return RuntimeStateSaveOutcome::Failed(io::Error::new(
                io::ErrorKind::InvalidData,
                error,
            ));
        }
    };
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(".state.{}.{}.tmp", std::process::id(), nonce));
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
    {
        Ok(file) => file,
        Err(error) => return RuntimeStateSaveOutcome::Failed(error),
    };
    if let Err(error) = file.write_all(&bytes) {
        let _ = fs::remove_file(temporary);
        return RuntimeStateSaveOutcome::Failed(error);
    }
    if let Err(error) = file.sync_all() {
        let _ = fs::remove_file(temporary);
        return RuntimeStateSaveOutcome::Failed(error);
    }
    // The temporary file is created with mode 0600. Rename it only after the
    // file contents are synced. The parent directory sync below is a
    // separate durability boundary: after rename succeeds, an error means
    // the new entry may already be visible and must not be rolled back in
    // memory.
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(temporary);
        return RuntimeStateSaveOutcome::Failed(error);
    }
    #[cfg(feature = "test-fixture")]
    if test_directory_sync_failure_requested(parent) {
        return RuntimeStateSaveOutcome::Uncertain(io::Error::other(
            "test-injected runtime state directory sync failure",
        ));
    }
    match fs::File::open(parent).and_then(|directory| directory.sync_all()) {
        Ok(()) => RuntimeStateSaveOutcome::Durable,
        Err(error) => RuntimeStateSaveOutcome::Uncertain(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::AppPaths;
    use std::os::unix::fs::symlink;

    #[test]
    fn default_desires_running() {
        assert_eq!(RuntimeState::default().desired_state, DesiredState::Running);
        assert_eq!(RuntimeState::default().schema_version, 2);
    }

    #[test]
    fn state_round_trips_without_credentials() {
        let root =
            std::env::temp_dir().join(format!("opencodeserver-state-test-{}", std::process::id()));
        let paths = AppPaths::from_support_dir(root.clone());
        paths.ensure_directories().expect("directories");
        let state = RuntimeState::default();
        state.save(&paths).expect("save");
        assert_eq!(RuntimeState::load(&paths).expect("load"), state);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn state_file_accessible_by_group_or_other_is_rejected() {
        let root = std::env::temp_dir().join(format!(
            "opencodeserver-state-mode-test-{}",
            std::process::id()
        ));
        let paths = AppPaths::from_support_dir(root.clone());
        paths.ensure_directories().expect("directories");
        RuntimeState::default().save(&paths).expect("save");
        let mut permissions = fs::metadata(&paths.runtime_state)
            .expect("state metadata")
            .permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&paths.runtime_state, permissions).expect("widen state mode");
        let error = RuntimeState::load(&paths)
            .expect_err("a state file accessible by group or other must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dangling_runtime_state_symlink_is_not_first_run() {
        let root = std::env::temp_dir().join(format!(
            "opencodeserver-state-dangling-symlink-test-{}",
            std::process::id()
        ));
        let paths = AppPaths::from_support_dir(root.clone());
        paths.ensure_directories().expect("directories");
        symlink(
            paths.run_dir.join("missing-state.json"),
            &paths.runtime_state,
        )
        .expect("dangling state symlink");

        let error = RuntimeState::load(&paths).expect_err("dangling symlink must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_state_load_propagates_non_not_found_path_errors() {
        let root = std::env::temp_dir().join(format!(
            "opencodeserver-state-path-error-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("test root");
        let run_dir = root.join("run");
        fs::write(&run_dir, b"not a directory").expect("regular-file run path");
        let paths = AppPaths::from_support_dir(root.clone());

        let error =
            RuntimeState::load(&paths).expect_err("path errors must not look like first run");
        assert_eq!(error.kind(), io::ErrorKind::NotADirectory);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn current_runtime_schema_without_launch_marker_remains_readable() {
        let root = std::env::temp_dir().join(format!(
            "opencodeserver-current-state-test-{}",
            std::process::id()
        ));
        let paths = AppPaths::from_support_dir(root.clone());
        paths.ensure_directories().expect("directories");
        fs::write(
            &paths.runtime_state,
            br#"{"schema_version":2,"desired_state":"stopped","process":null,"notification":null}"#,
        )
        .expect("current state fixture");
        fs::set_permissions(&paths.runtime_state, fs::Permissions::from_mode(0o600))
            .expect("restrict current state fixture");
        let state = RuntimeState::load(&paths).expect("load current state fixture");
        assert_eq!(state.desired_state, DesiredState::Stopped);
        assert_eq!(state.launch_pending, None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn previous_runtime_schema_is_rejected_without_migration() {
        let root = std::env::temp_dir().join(format!(
            "opencodeserver-old-state-test-{}",
            std::process::id()
        ));
        let paths = AppPaths::from_support_dir(root.clone());
        paths.ensure_directories().expect("directories");
        fs::write(
            &paths.runtime_state,
            br#"{"schema_version":1,"desired_state":"running","process":null,"next_notification_id":3,"notification":null}"#,
        )
        .expect("old state fixture");
        assert!(RuntimeState::load(&paths).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
