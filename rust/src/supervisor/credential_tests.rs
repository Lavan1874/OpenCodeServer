//! Characterization tests for the credential state machine, written against
//! the unmodified `Supervisor` before the CredentialController extraction.
//! They lock current behavior — including deliberate fail-closed quirks —
//! so the refactor can be proven behavior-preserving.
//!
//! Two mechanisms keep these tests deterministic without any keychain I/O:
//!
//! * Read-result paths are driven through hand-built
//!   `CredentialRefreshInFlight` channels: `apply_credential_read` receives
//!   its `KeychainRead` as a parameter, so every result arm is reachable
//!   in-process.
//! * `merge_credentials` probe branches are driven through the
//!   `OPENCODESERVER_TEST_PASSWORD` / `OPENCODESERVER_TEST_ENFORCE_GRANT`
//!   fixture hooks. Because those are process-global, each such test runs
//!   its assertions in a single-test child process (`run_as_fixture_child`),
//!   so no other test can observe the environment.

use super::*;
use crate::config::{ConfigFile, write_config_atomically};

const CHILD_MARKER: &str = "OCS_CRED_CHARACTERIZATION_CHILD";

/// Spawns the current test binary as a single-test child with the fixture
/// environment applied. The child runs the same test body with
/// `CHILD_MARKER` set; the parent just asserts the child succeeded.
fn run_as_fixture_child(test_name: &str, environment: &[(&str, &str)]) {
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    command.arg(test_name).arg("--exact").env(CHILD_MARKER, "1");
    for (key, value) in environment {
        command.env(key, value);
    }
    let status = command.status().expect("spawn fixture child");
    assert!(status.success(), "fixture child {test_name} failed");
}

/// A Supervisor over private temp paths with a unique account and
/// `DesiredState::Stopped`, so no OpenCode is ever spawned. Returns the
/// root directory for cleanup.
fn test_supervisor(tag: &str) -> (Supervisor, std::path::PathBuf) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "opencodeserver-credential-characterization-{tag}-{}-{nonce}",
        std::process::id()
    ));
    let paths = AppPaths::from_support_dir(root.clone());
    paths.ensure_directories().expect("private test paths");
    write_config_atomically(
        &paths.config_file,
        &ConfigFile {
            username: format!("cc-{tag}"),
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

/// Seeds an in-flight read whose channel the test still controls, exactly
/// like a dispatched worker would. The read is issued for the currently
/// configured account, so `apply_credential_read` accepts it.
fn seed_in_flight(supervisor: &mut Supervisor, read: KeychainRead) {
    let account = supervisor
        .latest_config
        .as_ref()
        .expect("latest config")
        .effective_username
        .clone();
    let (sender, receiver) = mpsc::channel();
    supervisor
        .credentials
        .set_refresh_in_flight_for_test(CredentialRefreshInFlight {
            dispatched: Instant::now(),
            account,
            worker: None,
            receiver,
        });
    sender.send(read).expect("worker result");
    drop(sender);
}

/// Drives an in-flight credential read to completion. The fixture hook
/// answers instantly; the loop only crosses the worker-thread boundary.
fn converge_credential_refresh(supervisor: &mut Supervisor) {
    for _ in 0..200 {
        supervisor.poll_credential_refresh(Instant::now());
        if supervisor
            .credentials
            .refresh_in_flight_for_test()
            .is_none()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("credential refresh did not converge");
}

/// The stubbed signing team identities for the team-evidence tests. The
/// real `self_team` lookup returns `None` for the ad hoc-signed test binary;
/// these stubs let a test pose as a team-signed build.
const TEST_TEAM: &str = "TEAMID1234";
const OTHER_TEAM: &str = "TEAMID5678";

fn test_team() -> Option<String> {
    Some(TEST_TEAM.to_owned())
}

fn no_team() -> Option<String> {
    None
}

// ────────────────────────────────────────────────────────────────────
// State-machine transitions
// ────────────────────────────────────────────────────────────────────

#[test]
fn mark_credential_changed_spends_the_grant_and_goes_access_pending() {
    let (mut supervisor, root) = test_supervisor("changed");
    let account = "cc-changed";
    supervisor
        .credentials
        .grant_for_test()
        .record(account, crate::BUNDLE_VERSION, "")
        .expect("record grant");
    supervisor
        .credentials
        .set_state_for_test(CredentialState::Available);
    supervisor.credentials.set_explicitly_removed_for_test(true);

    let response = supervisor.handle(Command::CredentialChanged);

    assert!(response.ok);
    assert_eq!(
        response.status.expect("status").password_state,
        PasswordState::AccessPending
    );
    assert_eq!(
        supervisor.credentials.state_for_test(),
        CredentialState::AccessPending
    );
    assert!(supervisor.credentials.stale_for_test());
    assert!(!supervisor.credentials.explicitly_removed_for_test());
    assert_eq!(
        supervisor.credentials.grant_for_test().load(),
        None,
        "a rewritten item spends the XARA partition grant"
    );
    assert!(
        supervisor
            .credentials
            .refresh_in_flight_for_test()
            .is_none(),
        "a marker without team evidence (empty team line) never dispatches a silent re-read"
    );
    finish(supervisor, root);
}

#[test]
fn mark_credential_removed_leaves_a_pending_refresh_to_be_discarded() {
    let (mut supervisor, root) = test_supervisor("removed-pending");
    supervisor
        .credentials
        .set_state_for_test(CredentialState::Available);
    let (sender, receiver) = mpsc::channel();
    supervisor
        .credentials
        .set_refresh_in_flight_for_test(CredentialRefreshInFlight {
            dispatched: Instant::now(),
            account: "cc-removed-pending".to_owned(),
            worker: None,
            receiver,
        });

    let response = supervisor.handle(Command::CredentialRemoved);

    assert!(response.ok);
    assert_eq!(
        supervisor.credentials.state_for_test(),
        CredentialState::NotConfigured
    );
    assert!(supervisor.credentials.explicitly_removed_for_test());
    assert!(
        supervisor
            .credentials
            .refresh_in_flight_for_test()
            .is_some(),
        "removal does not cancel a read already in flight"
    );
    // The stale result arrives afterwards and is discarded, leaving the
    // removed state intact.
    sender
        .send(KeychainRead::Found("resurrected-password".to_owned()))
        .expect("stale worker result");
    drop(sender);
    supervisor.poll_credential_refresh(Instant::now());
    assert_eq!(
        supervisor.credentials.state_for_test(),
        CredentialState::NotConfigured
    );
    assert!(
        supervisor
            .credentials
            .refresh_in_flight_for_test()
            .is_none()
    );
    finish(supervisor, root);
}

// ────────────────────────────────────────────────────────────────────
// apply_credential_read result arms (via hand-built in-flight reads)
// ────────────────────────────────────────────────────────────────────

#[test]
fn applied_found_password_records_the_grant_and_keeps_active_config() {
    let (mut supervisor, root) = test_supervisor("found");
    let account = "cc-found";
    seed_in_flight(
        &mut supervisor,
        KeychainRead::Found("decrypted-password".to_owned()),
    );

    supervisor.poll_credential_refresh(Instant::now());

    assert_eq!(
        supervisor.credentials.state_for_test(),
        CredentialState::Available
    );
    assert!(!supervisor.credentials.stale_for_test());
    assert_eq!(
        supervisor.credentials.grant_for_test().load(),
        Some((
            account.to_owned(),
            crate::BUNDLE_VERSION.to_owned(),
            String::new()
        )),
        "a successful decrypt records the account-and-version-bound grant; \
         the ad hoc test binary has no team, so the third line is empty"
    );
    assert_eq!(
        supervisor
            .latest_config
            .as_ref()
            .expect("latest config")
            .source
            .password,
        "decrypted-password"
    );
    assert!(
        supervisor.process.is_none(),
        "desired_state is Stopped, so no start is resumed"
    );
    assert!(supervisor.active_config.is_none());
    finish(supervisor, root);
}

#[test]
fn applied_not_configured_read_clears_password_and_grant() {
    let (mut supervisor, root) = test_supervisor("not-configured");
    let account = "cc-not-configured";
    supervisor
        .credentials
        .grant_for_test()
        .record(account, crate::BUNDLE_VERSION, "")
        .expect("record grant");
    supervisor
        .credentials
        .set_state_for_test(CredentialState::AccessPending);
    supervisor.credentials.set_stale_for_test(true);
    supervisor
        .latest_config
        .as_mut()
        .expect("latest config")
        .source
        .password = "previous-password".to_owned();
    seed_in_flight(&mut supervisor, KeychainRead::NotConfigured);

    supervisor.poll_credential_refresh(Instant::now());

    assert_eq!(
        supervisor.credentials.state_for_test(),
        CredentialState::NotConfigured
    );
    assert!(!supervisor.credentials.stale_for_test());
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
    finish(supervisor, root);
}

#[test]
fn applied_access_pending_read_clears_the_grant_but_keeps_the_password() {
    let (mut supervisor, root) = test_supervisor("declined");
    let account = "cc-declined";
    supervisor
        .credentials
        .grant_for_test()
        .record(account, crate::BUNDLE_VERSION, "")
        .expect("record grant");
    supervisor
        .credentials
        .set_state_for_test(CredentialState::Available);
    supervisor
        .latest_config
        .as_mut()
        .expect("latest config")
        .source
        .password = "carried-password".to_owned();
    seed_in_flight(&mut supervisor, KeychainRead::AccessPending);

    supervisor.poll_credential_refresh(Instant::now());

    assert_eq!(
        supervisor.credentials.state_for_test(),
        CredentialState::AccessPending
    );
    assert_eq!(
        supervisor.credentials.grant_for_test().load(),
        None,
        "a declined prompt spends the grant marker"
    );
    assert_eq!(
        supervisor
            .latest_config
            .as_ref()
            .expect("latest config")
            .source
            .password,
        "carried-password",
        "AccessPending is a soft state and never clears an in-memory password"
    );
    finish(supervisor, root);
}

#[test]
fn applied_failed_read_reports_access_pending_and_keeps_the_grant() {
    // Locks the current asymmetry: a generic OSStatus failure flips the soft
    // state but does NOT spend the grant marker (recorded as observation #1
    // in the boundary design; deliberately not changed).
    let (mut supervisor, root) = test_supervisor("failed");
    let account = "cc-failed";
    supervisor
        .credentials
        .grant_for_test()
        .record(account, crate::BUNDLE_VERSION, "")
        .expect("record grant");
    supervisor
        .credentials
        .set_state_for_test(CredentialState::Available);
    seed_in_flight(&mut supervisor, KeychainRead::Failed(-50));

    supervisor.poll_credential_refresh(Instant::now());

    assert_eq!(
        supervisor.credentials.state_for_test(),
        CredentialState::AccessPending
    );
    assert_eq!(
        supervisor.credentials.grant_for_test().load(),
        Some((
            account.to_owned(),
            crate::BUNDLE_VERSION.to_owned(),
            String::new()
        ))
    );
    finish(supervisor, root);
}

// ────────────────────────────────────────────────────────────────────
// poll_credential_refresh bookkeeping
// ────────────────────────────────────────────────────────────────────

#[test]
fn refresh_request_is_a_noop_while_a_read_is_in_flight() {
    let (mut supervisor, root) = test_supervisor("single-flight");
    supervisor
        .credentials
        .set_state_for_test(CredentialState::NotConfigured);
    supervisor.credentials.set_explicitly_removed_for_test(true);
    let (sender, receiver) = mpsc::channel();
    supervisor
        .credentials
        .set_refresh_in_flight_for_test(CredentialRefreshInFlight {
            dispatched: Instant::now(),
            account: "cc-single-flight".to_owned(),
            worker: None,
            receiver,
        });

    supervisor.request_credential_refresh();

    assert!(
        supervisor.credentials.explicitly_removed_for_test(),
        "a no-op request must not clear the removal barrier"
    );
    // The original channel still drives the original in-flight read: the
    // request did not replace it.
    sender
        .send(KeychainRead::NotConfigured)
        .expect("original worker result");
    drop(sender);
    supervisor.poll_credential_refresh(Instant::now());
    assert!(
        supervisor
            .credentials
            .refresh_in_flight_for_test()
            .is_none()
    );
    assert_eq!(
        supervisor.credentials.state_for_test(),
        CredentialState::NotConfigured
    );
    finish(supervisor, root);
}

#[test]
fn overdue_latch_fires_once_and_resets_on_completion() {
    let (mut supervisor, root) = test_supervisor("overdue");
    let (sender, receiver) = mpsc::channel();
    supervisor
        .credentials
        .set_refresh_in_flight_for_test(CredentialRefreshInFlight {
            dispatched: Instant::now() - CREDENTIAL_REFRESH_OVERDUE - Duration::from_secs(1),
            account: "cc-overdue".to_owned(),
            worker: None,
            receiver,
        });

    supervisor.poll_credential_refresh(Instant::now());
    assert!(
        supervisor
            .credentials
            .refresh_in_flight_for_test()
            .is_some()
    );
    assert!(
        supervisor.credentials.overdue_logged_for_test(),
        "the overdue log fires once the prompt outlives its bound"
    );
    supervisor.poll_credential_refresh(Instant::now());
    assert!(
        supervisor.credentials.overdue_logged_for_test(),
        "the latch stays set while the same read stays pending"
    );

    sender
        .send(KeychainRead::AccessPending)
        .expect("worker result");
    drop(sender);
    supervisor.poll_credential_refresh(Instant::now());
    assert!(
        !supervisor.credentials.overdue_logged_for_test(),
        "completion resets the latch for the next read"
    );
    finish(supervisor, root);
}

#[test]
fn disconnected_worker_leaves_no_in_flight_read() {
    let (mut supervisor, root) = test_supervisor("disconnected");
    supervisor
        .credentials
        .set_state_for_test(CredentialState::AccessPending);
    supervisor.credentials.set_overdue_logged_for_test(true);
    let (_sender, receiver) = mpsc::channel::<KeychainRead>();
    supervisor
        .credentials
        .set_refresh_in_flight_for_test(CredentialRefreshInFlight {
            dispatched: Instant::now(),
            account: "cc-disconnected".to_owned(),
            worker: None,
            receiver,
        });
    drop(_sender);

    supervisor.poll_credential_refresh(Instant::now());

    assert!(
        supervisor
            .credentials
            .refresh_in_flight_for_test()
            .is_none()
    );
    assert!(!supervisor.credentials.overdue_logged_for_test());
    assert_eq!(
        supervisor.credentials.state_for_test(),
        CredentialState::AccessPending
    );
    finish(supervisor, root);
}

// ────────────────────────────────────────────────────────────────────
// merge_credentials probe branches (fixture-child tests)
// ────────────────────────────────────────────────────────────────────

#[test]
fn merge_carried_password_stays_available() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        let (mut supervisor, root) = test_supervisor("carry");
        supervisor
            .latest_config
            .as_mut()
            .expect("latest config")
            .source
            .password = "carried-password".to_owned();
        let mut incoming = supervisor.latest_config.clone().expect("latest config");
        incoming.source.password.clear();

        let merged = supervisor.merge_credentials(incoming);

        assert_eq!(merged.source.password, "carried-password");
        assert_eq!(
            supervisor.credentials.state_for_test(),
            CredentialState::Available
        );
        finish(supervisor, root);
    } else {
        run_as_fixture_child(
            "supervisor::credential_tests::merge_carried_password_stays_available",
            &[("OPENCODESERVER_TEST_PASSWORD", "fixture-password")],
        );
    }
}

#[test]
fn merge_stale_config_carries_password_to_access_pending() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        let (mut supervisor, root) = test_supervisor("stale-merge");
        supervisor.credentials.set_stale_for_test(true);
        supervisor
            .latest_config
            .as_mut()
            .expect("latest config")
            .source
            .password = "carried-password".to_owned();
        let mut incoming = supervisor.latest_config.clone().expect("latest config");
        incoming.source.password.clear();

        let merged = supervisor.merge_credentials(incoming);

        assert_eq!(
            merged.source.password, "carried-password",
            "the running process keeps its credential"
        );
        assert_eq!(
            supervisor.credentials.state_for_test(),
            CredentialState::AccessPending
        );
        assert!(
            supervisor.credentials.stale_for_test(),
            "the stale flag clears only when a read converges"
        );
        finish(supervisor, root);
    } else {
        run_as_fixture_child(
            "supervisor::credential_tests::merge_stale_config_carries_password_to_access_pending",
            &[("OPENCODESERVER_TEST_PASSWORD", "fixture-password")],
        );
    }
}

#[test]
fn merge_unproven_grant_yields_empty_password_and_access_pending() {
    // The counterintuitive fail-closed case AGENTS.md calls out: an item
    // exists but this build has no proven grant, so a routine merge must
    // neither decrypt (no dialog from the background) nor fabricate a
    // password — the config merges an EMPTY password and the state is the
    // soft AccessPending. The incoming config has the production loader's
    // shape: passwords never live on disk, so a fresh merge always starts
    // empty and the fail-closed branch must not invent one.
    if std::env::var_os(CHILD_MARKER).is_some() {
        let (mut supervisor, root) = test_supervisor("unproven");
        // Even a build WITH a team identity must not dispatch without any
        // marker: there is no recorded evidence to match against.
        supervisor.credentials.set_self_team_for_test(test_team);
        let incoming = supervisor.latest_config.clone().expect("latest config");
        assert_eq!(
            incoming.source.password, "",
            "test premise: the on-disk loader never carries a password"
        );

        let merged = supervisor.merge_credentials(incoming);

        assert_eq!(merged.source.password, "");
        assert_eq!(
            supervisor.credentials.state_for_test(),
            CredentialState::AccessPending
        );
        assert!(
            supervisor
                .credentials
                .refresh_in_flight_for_test()
                .is_none(),
            "no background decrypt is dispatched without a proven grant"
        );
        finish(supervisor, root);
    } else {
        run_as_fixture_child(
            "supervisor::credential_tests::merge_unproven_grant_yields_empty_password_and_access_pending",
            &[
                ("OPENCODESERVER_TEST_PASSWORD", "fixture-password"),
                ("OPENCODESERVER_TEST_ENFORCE_GRANT", "1"),
            ],
        );
    }
}

#[test]
fn merge_matching_grant_dispatches_a_background_read() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        let account = "cc-matching-grant";
        let (mut supervisor, root) = test_supervisor("matching-grant");
        supervisor
            .credentials
            .grant_for_test()
            .record(account, crate::BUNDLE_VERSION, "")
            .expect("record grant");
        let mut incoming = supervisor.latest_config.clone().expect("latest config");
        incoming.source.password.clear();

        let merged = supervisor.merge_credentials(incoming);

        assert_eq!(merged.source.password, "");
        assert_eq!(
            supervisor.credentials.state_for_test(),
            CredentialState::AccessPending
        );
        assert!(
            supervisor
                .credentials
                .refresh_in_flight_for_test()
                .is_some(),
            "a marker-proven grant authorizes the single-flight background decrypt"
        );
        finish(supervisor, root);
    } else {
        run_as_fixture_child(
            "supervisor::credential_tests::merge_matching_grant_dispatches_a_background_read",
            &[
                ("OPENCODESERVER_TEST_PASSWORD", "fixture-password"),
                ("OPENCODESERVER_TEST_ENFORCE_GRANT", "1"),
            ],
        );
    }
}

#[test]
fn merge_not_configured_clears_password_and_grant() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        let account = "cc-not-configured-merge";
        let (mut supervisor, root) = test_supervisor("not-configured-merge");
        supervisor
            .credentials
            .grant_for_test()
            .record(account, crate::BUNDLE_VERSION, "")
            .expect("record grant");
        supervisor.credentials.set_stale_for_test(true);
        let mut incoming = supervisor.latest_config.clone().expect("latest config");
        incoming.source.password = "stale-password".to_owned();

        let merged = supervisor.merge_credentials(incoming);

        assert_eq!(
            merged.source.password, "",
            "-25300 is the only state that means no password configured"
        );
        assert_eq!(
            supervisor.credentials.state_for_test(),
            CredentialState::NotConfigured
        );
        assert!(!supervisor.credentials.stale_for_test());
        assert_eq!(supervisor.credentials.grant_for_test().load(), None);
        finish(supervisor, root);
    } else {
        run_as_fixture_child(
            "supervisor::credential_tests::merge_not_configured_clears_password_and_grant",
            &[("OPENCODESERVER_TEST_PASSWORD", "")],
        );
    }
}

#[test]
fn explicit_refresh_request_clears_removal_and_dispatches() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        let (mut supervisor, root) = test_supervisor("explicit-refresh");
        supervisor.credentials.set_explicitly_removed_for_test(true);

        supervisor.request_credential_refresh();

        assert!(!supervisor.credentials.explicitly_removed_for_test());
        assert!(
            supervisor
                .credentials
                .refresh_in_flight_for_test()
                .is_some(),
            "an idle explicit request dispatches the single-flight read"
        );
        // Converge the worker (fixture hook returns instantly) and confirm
        // the read applies.
        converge_credential_refresh(&mut supervisor);
        assert!(
            supervisor
                .credentials
                .refresh_in_flight_for_test()
                .is_none()
        );
        assert_eq!(
            supervisor.credentials.state_for_test(),
            CredentialState::NotConfigured
        );
        finish(supervisor, root);
    } else {
        run_as_fixture_child(
            "supervisor::credential_tests::explicit_refresh_request_clears_removal_and_dispatches",
            &[("OPENCODESERVER_TEST_PASSWORD", "")],
        );
    }
}

// ────────────────────────────────────────────────────────────────────
// Team-evidence paths (ADR 0016, 2026-08-17 amendment)
// ────────────────────────────────────────────────────────────────────
//
// The stubbed `self_team` lets a test pose as a team-signed build; the
// `OPENCODESERVER_TEST_ENFORCE_GRANT` knob keeps the fixture bypass from
// short-circuiting `background_decrypt_allowed` before the team branch is
// reached. The version-exact marker path is covered by
// `merge_matching_grant_dispatches_a_background_read` above.

#[test]
fn merge_version_mismatched_marker_with_matching_team_dispatches_a_silent_read() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        let account = "cc-team-upgrade";
        let (mut supervisor, root) = test_supervisor("team-upgrade");
        supervisor.credentials.set_self_team_for_test(test_team);
        supervisor
            .credentials
            .grant_for_test()
            .record(account, "previous-build", TEST_TEAM)
            .expect("record grant");
        let mut incoming = supervisor.latest_config.clone().expect("latest config");
        incoming.source.password.clear();

        let merged = supervisor.merge_credentials(incoming);

        assert_eq!(merged.source.password, "");
        assert_eq!(
            supervisor.credentials.state_for_test(),
            CredentialState::AccessPending
        );
        assert!(
            supervisor
                .credentials
                .refresh_in_flight_for_test()
                .is_some(),
            "same-team evidence authorizes the one bounded silent re-read"
        );
        assert!(
            supervisor.credentials.auto_read_attempted_for_test(account),
            "the dispatch spends the account's one-shot budget"
        );

        // The silent read converges through the fixture hook: the password
        // is applied and fresh evidence is recorded for THIS build.
        converge_credential_refresh(&mut supervisor);
        assert_eq!(
            supervisor.credentials.state_for_test(),
            CredentialState::Available
        );
        assert_eq!(
            supervisor.credentials.grant_for_test().load(),
            Some((
                account.to_owned(),
                crate::BUNDLE_VERSION.to_owned(),
                TEST_TEAM.to_owned()
            )),
            "success re-records the grant with the current version and team"
        );
        assert_eq!(
            supervisor
                .latest_config
                .as_ref()
                .expect("latest config")
                .source
                .password,
            "fixture-password"
        );
        finish(supervisor, root);
    } else {
        run_as_fixture_child(
            "supervisor::credential_tests::merge_version_mismatched_marker_with_matching_team_dispatches_a_silent_read",
            &[
                ("OPENCODESERVER_TEST_PASSWORD", "fixture-password"),
                ("OPENCODESERVER_TEST_ENFORCE_GRANT", "1"),
            ],
        );
    }
}

#[test]
fn merge_version_mismatched_marker_without_matching_team_never_dispatches() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        // Every combination that must refuse the silent path: a marker
        // recorded under a DIFFERENT team, a marker with no team evidence
        // (ad hoc writer), and a build with no team identity of its own.
        // Case shape: (test tag, recorded marker team, stubbed self team).
        type TeamCase = (&'static str, &'static str, fn() -> Option<String>);
        let cases: [TeamCase; 3] = [
            ("team-mismatch", OTHER_TEAM, test_team),
            ("team-adhoc-marker", "", test_team),
            ("team-adhoc-self", TEST_TEAM, no_team),
        ];
        for (tag, recorded_team, self_team) in cases {
            let account = format!("cc-{tag}");
            let (mut supervisor, root) = test_supervisor(tag);
            supervisor.credentials.set_self_team_for_test(self_team);
            supervisor
                .credentials
                .grant_for_test()
                .record(&account, "previous-build", recorded_team)
                .expect("record grant");
            let mut incoming = supervisor.latest_config.clone().expect("latest config");
            incoming.source.password.clear();

            let merged = supervisor.merge_credentials(incoming);

            assert_eq!(merged.source.password, "", "{tag}");
            assert_eq!(
                supervisor.credentials.state_for_test(),
                CredentialState::AccessPending,
                "{tag}"
            );
            assert!(
                supervisor
                    .credentials
                    .refresh_in_flight_for_test()
                    .is_none(),
                "{tag}: without matching team evidence the manual click path stays"
            );
            assert_eq!(
                supervisor.credentials.grant_for_test().load(),
                Some((
                    account.clone(),
                    "previous-build".to_owned(),
                    recorded_team.to_owned()
                )),
                "{tag}: the marker is left untouched"
            );
            finish(supervisor, root);
        }
    } else {
        run_as_fixture_child(
            "supervisor::credential_tests::merge_version_mismatched_marker_without_matching_team_never_dispatches",
            &[
                ("OPENCODESERVER_TEST_PASSWORD", "fixture-password"),
                ("OPENCODESERVER_TEST_ENFORCE_GRANT", "1"),
            ],
        );
    }
}

#[test]
fn credential_changed_with_matching_team_keeps_the_grant_and_re_reads_silently() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        let account = "cc-team-changed";
        let (mut supervisor, root) = test_supervisor("team-changed");
        supervisor.credentials.set_self_team_for_test(test_team);
        supervisor
            .credentials
            .grant_for_test()
            .record(account, crate::BUNDLE_VERSION, TEST_TEAM)
            .expect("record grant");
        supervisor
            .credentials
            .set_state_for_test(CredentialState::Available);
        supervisor
            .latest_config
            .as_mut()
            .expect("latest config")
            .source
            .password = "old-password".to_owned();

        let response = supervisor.handle(Command::CredentialChanged);

        assert!(response.ok);
        assert_eq!(
            supervisor.credentials.state_for_test(),
            CredentialState::AccessPending
        );
        assert!(supervisor.credentials.stale_for_test());
        assert_eq!(
            supervisor.credentials.grant_for_test().load(),
            Some((
                account.to_owned(),
                crate::BUNDLE_VERSION.to_owned(),
                TEST_TEAM.to_owned()
            )),
            "a team-anchored grant survives the in-place update (2026-08-17 measurement)"
        );
        assert!(
            supervisor
                .credentials
                .refresh_in_flight_for_test()
                .is_some(),
            "the one bounded silent re-read is dispatched"
        );
        assert_eq!(
            supervisor
                .latest_config
                .as_ref()
                .expect("latest config")
                .source
                .password,
            "old-password",
            "the running process keeps its credential until the read converges"
        );

        converge_credential_refresh(&mut supervisor);

        assert_eq!(
            supervisor.credentials.state_for_test(),
            CredentialState::Available
        );
        assert!(
            !supervisor.credentials.stale_for_test(),
            "the converged read clears the stale flag"
        );
        assert_eq!(
            supervisor
                .latest_config
                .as_ref()
                .expect("latest config")
                .source
                .password,
            "new-password",
            "the converged silent read applies the NEW password"
        );
        finish(supervisor, root);
    } else {
        run_as_fixture_child(
            "supervisor::credential_tests::credential_changed_with_matching_team_keeps_the_grant_and_re_reads_silently",
            &[
                ("OPENCODESERVER_TEST_PASSWORD", "new-password"),
                ("OPENCODESERVER_TEST_ENFORCE_GRANT", "1"),
            ],
        );
    }
}

#[test]
fn credential_changed_with_mismatched_team_spends_the_grant_and_waits_for_the_click() {
    let (mut supervisor, root) = test_supervisor("team-mismatch-changed");
    let account = "cc-team-mismatch-changed";
    supervisor.credentials.set_self_team_for_test(test_team);
    supervisor
        .credentials
        .grant_for_test()
        .record(account, crate::BUNDLE_VERSION, OTHER_TEAM)
        .expect("record grant");
    supervisor
        .credentials
        .set_state_for_test(CredentialState::Available);

    let response = supervisor.handle(Command::CredentialChanged);

    assert!(response.ok);
    assert_eq!(
        supervisor.credentials.state_for_test(),
        CredentialState::AccessPending
    );
    assert!(supervisor.credentials.stale_for_test());
    assert_eq!(
        supervisor.credentials.grant_for_test().load(),
        None,
        "a grant recorded under a DIFFERENT team is spent by the rewrite, exactly as before"
    );
    assert!(
        supervisor
            .credentials
            .refresh_in_flight_for_test()
            .is_none(),
        "no silent re-read without matching team evidence"
    );
    finish(supervisor, root);
}

#[test]
fn automatic_silent_read_runs_once_per_account_until_a_fresh_grant() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        let account = "cc-team-guard";
        let (mut supervisor, root) = test_supervisor("team-guard");
        supervisor.credentials.set_self_team_for_test(test_team);
        supervisor
            .credentials
            .grant_for_test()
            .record(account, "previous-build", TEST_TEAM)
            .expect("record grant");

        // The first team-evidence merge dispatches the one automatic read.
        let mut incoming = supervisor.latest_config.clone().expect("latest config");
        incoming.source.password.clear();
        supervisor.merge_credentials(incoming);
        assert!(
            supervisor
                .credentials
                .refresh_in_flight_for_test()
                .is_some(),
            "the first team-evidence merge dispatches"
        );
        assert!(supervisor.credentials.auto_read_attempted_for_test(account));

        // The read fails transiently: the marker is kept (the existing
        // Failed asymmetry), so without the one-shot guard the next 60 s
        // recheck would dispatch again.
        seed_in_flight(&mut supervisor, KeychainRead::Failed(-50));
        supervisor.poll_credential_refresh(Instant::now());
        assert_eq!(
            supervisor.credentials.grant_for_test().load(),
            Some((
                account.to_owned(),
                "previous-build".to_owned(),
                TEST_TEAM.to_owned()
            )),
            "a transient failure keeps the marker"
        );

        let mut incoming = supervisor.latest_config.clone().expect("latest config");
        incoming.source.password.clear();
        supervisor.merge_credentials(incoming);
        assert!(
            supervisor
                .credentials
                .refresh_in_flight_for_test()
                .is_none(),
            "the one-shot guard stops the recheck loop"
        );

        // A successful interactive read ("Allow Keychain Access…") records
        // fresh evidence for this build and resets the guard.
        supervisor.request_credential_refresh();
        converge_credential_refresh(&mut supervisor);
        assert_eq!(
            supervisor.credentials.state_for_test(),
            CredentialState::Available
        );
        assert!(
            !supervisor.credentials.auto_read_attempted_for_test(account),
            "a fresh grant resets the one-shot budget"
        );

        // The next same-team upgrade (marker again behind this build)
        // therefore re-earns its one silent attempt. The fresh merge starts
        // with an empty password: passwords never live on disk.
        supervisor
            .credentials
            .grant_for_test()
            .record(account, "previous-build", TEST_TEAM)
            .expect("re-record previous-build marker");
        supervisor
            .latest_config
            .as_mut()
            .expect("latest config")
            .source
            .password
            .clear();
        let mut incoming = supervisor.latest_config.clone().expect("latest config");
        incoming.source.password.clear();
        supervisor.merge_credentials(incoming);
        assert!(
            supervisor
                .credentials
                .refresh_in_flight_for_test()
                .is_some(),
            "after a fresh grant the next upgrade re-earns its one silent read"
        );
        finish(supervisor, root);
    } else {
        run_as_fixture_child(
            "supervisor::credential_tests::automatic_silent_read_runs_once_per_account_until_a_fresh_grant",
            &[
                ("OPENCODESERVER_TEST_PASSWORD", "fixture-password"),
                ("OPENCODESERVER_TEST_ENFORCE_GRANT", "1"),
            ],
        );
    }
}
