use super::*;
use std::collections::HashSet;

/// The credential state machine extracted from `Supervisor` (see
/// `docs/refactor/credential-controller-boundary.md`). Owns the credential
/// fields and their invariants; keychain I/O, configuration, and the
/// grant-marker persistence surface cross the boundary explicitly.
pub(crate) struct CredentialController {
    credential_state: CredentialState,
    credential_grant: CredentialGrant,
    credential_refresh: Option<CredentialRefreshInFlight>,
    credential_refresh_overdue_logged: bool,
    /// The GUI reported a Keychain credential change (`credential_changed`).
    /// The in-memory password is kept so the RUNNING process (still using it)
    /// stays supervised and healthy, but it can no longer be trusted as
    /// current: routine reloads must not flip the state back to `Available`
    /// or attempt a background decrypt. Cleared only by a successful
    /// interactive re-read (`Allow Keychain Access…`) or when the item disappears.
    /// In-memory only: a restarted agent reads the Keychain fresh anyway.
    credential_stale: bool,
    /// Settings has successfully deleted the item. This generation barrier
    /// prevents a decrypt result that was already in flight from restoring
    /// the just-removed password after `credential_removed` was processed.
    /// Cleared only when the user explicitly asks OpenCodeServerAgent to read
    /// a newly created/replaced credential.
    credential_explicitly_removed: bool,
    /// One-shot guard for the automatic, non-interactive team-evidence
    /// re-reads (ADR 0016, 2026-08-17 amendment): the accounts for which this
    /// process has already dispatched its one bounded silent re-read. A
    /// transient `KeychainRead::Failed` keeps the grant marker, so without
    /// this guard every 60 s configuration recheck would re-dispatch the
    /// silent read forever. `AccessPending` needs no guard entry: it clears
    /// the marker, and the team path then finds no evidence. An account
    /// leaves the set when a read succeeds (`Found`): the success records
    /// fresh grant evidence for the current build — through the interactive
    /// "Allow Keychain Access…" approval or a silent read alike — so a later
    /// automatic dispatch (e.g. after the next password change) is backed by
    /// that new evidence rather than by a spent expectation. In-memory only:
    /// a restarted process re-derives its budget from the marker on disk.
    automatic_read_attempted: HashSet<String>,
    probe: fn(&str) -> KeychainProbe,
    read_password: fn(&str) -> KeychainRead,
    /// This running binary's own signing team identifier, read from its code
    /// signature at runtime (ground truth, never a build-time bake). `None`
    /// for unsigned/ad hoc builds, which keeps them on the explicit-click
    /// path. Injected as a function so tests can stub the team identity.
    self_team: fn() -> Option<String>,
}

impl CredentialController {
    /// The credential state visible to `status`, `start_refusal`,
    /// `start_now`, `try_reattach`, and `check_health`.
    pub(crate) fn state(&self) -> CredentialState {
        self.credential_state
    }

    /// Whether a decrypt-class read is currently in flight; `start_now`
    /// must not flap a refusal while one converges, and `next_deadline`
    /// schedules the recheck poll.
    pub(crate) fn refresh_in_flight(&self) -> bool {
        self.credential_refresh.is_some()
    }

    pub(crate) fn new(
        grant: CredentialGrant,
        probe: fn(&str) -> KeychainProbe,
        read_password: fn(&str) -> KeychainRead,
        self_team: fn() -> Option<String>,
    ) -> Self {
        Self {
            credential_state: CredentialState::NotConfigured,
            credential_grant: grant,
            credential_refresh: None,
            credential_refresh_overdue_logged: false,
            credential_stale: false,
            credential_explicitly_removed: false,
            automatic_read_attempted: HashSet::new(),
            probe,
            read_password,
            self_team,
        }
    }
}

#[cfg(test)]
impl CredentialController {
    pub(super) fn state_for_test(&self) -> CredentialState {
        self.credential_state
    }
    pub(super) fn set_state_for_test(&mut self, state: CredentialState) {
        self.credential_state = state;
    }
    pub(super) fn grant_for_test(&self) -> &CredentialGrant {
        &self.credential_grant
    }
    pub(super) fn refresh_in_flight_for_test(&self) -> &Option<CredentialRefreshInFlight> {
        &self.credential_refresh
    }
    pub(super) fn set_refresh_in_flight_for_test(&mut self, in_flight: CredentialRefreshInFlight) {
        self.credential_refresh = Some(in_flight);
    }
    pub(super) fn overdue_logged_for_test(&self) -> bool {
        self.credential_refresh_overdue_logged
    }
    pub(super) fn set_overdue_logged_for_test(&mut self, value: bool) {
        self.credential_refresh_overdue_logged = value;
    }
    pub(super) fn stale_for_test(&self) -> bool {
        self.credential_stale
    }
    pub(super) fn set_stale_for_test(&mut self, value: bool) {
        self.credential_stale = value;
    }
    pub(super) fn explicitly_removed_for_test(&self) -> bool {
        self.credential_explicitly_removed
    }
    pub(super) fn set_explicitly_removed_for_test(&mut self, value: bool) {
        self.credential_explicitly_removed = value;
    }
    pub(super) fn set_self_team_for_test(&mut self, self_team: fn() -> Option<String>) {
        self.self_team = self_team;
    }
    pub(super) fn auto_read_attempted_for_test(&self, account: &str) -> bool {
        self.automatic_read_attempted.contains(account)
    }
}

impl CredentialController {
    /// Merges the Keychain credential into a freshly validated configuration.
    ///
    /// Routine configuration work (startup, kqueue reload, periodic recheck)
    /// must never raise the Keychain consent dialog from a background
    /// process. On macOS 26 the dialog cannot be suppressed by any query key
    /// (ADR 0016), so this function only probes attributes — with two
    /// exceptions, both dispatched to the single-flight worker rather than
    /// run inline (a wrong expectation — e.g. a grant revoked in Keychain
    /// Access — would block the event loop behind SecurityAgent and stall
    /// IPC):
    ///
    /// 1. the persisted grant marker proves a decrypt already succeeded for
    ///    this account with THIS exact build (e.g. an agent restart after the
    ///    user chose "Always Allow");
    /// 2. the marker does not cover this build, but its recorded signing team
    ///    matches this build's own runtime team identity: measured 2026-08-17
    ///    (ADR 0016 amendment, macOS 26.6.1, Apple Development Team ID), the
    ///    team-anchored XARA partition grant survives same-team cdHash
    ///    changes, so a same-team upgrade re-reads silently. This path is
    ///    bounded to one automatic attempt per account per process run (see
    ///    `automatic_read_attempted`) and any non-success falls back to the
    ///    explicit click.
    ///
    /// The result converges through `apply_credential_read` and
    /// `recheck_stale_process`; until then the state is the soft
    /// `AccessPending` and any previous password is carried over.
    ///
    /// Everything else uses the attribute-only `probe_item`, which cannot
    /// raise UI. On `AccessPending` the previously merged password is kept in
    /// memory: the state means "cannot tell right now", and clearing the
    /// field would flap the configuration fingerprint and the
    /// `config_pending` status on a transient keychain failure. Only
    /// `NotConfigured` (-25300, the item is provably absent) clears the
    /// password.
    pub(crate) fn merge_credentials(
        &mut self,
        mut config: ValidatedConfig,
        previous: Option<&ValidatedConfig>,
    ) -> ValidatedConfig {
        let account = config.effective_username.clone();
        match (self.probe)(&account) {
            KeychainProbe::NotConfigured => {
                config.source.password.clear();
                self.clear_credential_grant(&account);
                self.credential_stale = false;
                self.set_credential_state(CredentialState::NotConfigured);
            }
            KeychainProbe::Failed(code) => {
                self.carry_over_password(&mut config, &account, previous);
                self.set_credential_state(CredentialState::AccessPending);
                log(
                    LogLevel::Error,
                    &format!(
                        "Keychain credential probe failed with OSStatus {code}; keeping the previous in-memory credential"
                    ),
                );
            }
            KeychainProbe::Exists => {
                if self.credential_stale {
                    // The GUI reported a credential change that no
                    // interactive read has applied yet: keep carrying the
                    // old password for the running process, but do NOT call
                    // the state Available and do NOT attempt a background
                    // decrypt — the re-read belongs to "Allow Keychain Access…".
                    self.carry_over_password(&mut config, &account, previous);
                    self.set_credential_state(CredentialState::AccessPending);
                } else if self.carry_over_password(&mut config, &account, previous) {
                    self.set_credential_state(CredentialState::Available);
                } else if self.background_decrypt_allowed(&account) {
                    // The grant marker covers this exact build, so the
                    // decrypt is expected to be silent — but it is still a
                    // decrypt-class read that blocks on the consent dialog
                    // when the expectation is wrong (grant revoked in
                    // Keychain Access). An inline read here once stalled the
                    // event loop behind SecurityAgent and burned the whole
                    // Service Management registration transaction (v42→v43,
                    // 2026-08-05), so the read is dispatched to the
                    // single-flight worker instead; the result converges via
                    // `apply_credential_read` (which also resumes a pending
                    // start) and `recheck_stale_process`.
                    self.carry_over_password(&mut config, &account, previous);
                    self.set_credential_state(CredentialState::AccessPending);
                    self.request_credential_refresh_for(&account);
                } else if self.team_evidence_matches(&account) {
                    // The marker does not cover THIS build, but it was
                    // recorded under the same signing team this process is
                    // running: the team-anchored partition grant survives
                    // same-team cdHash changes (ADR 0016, 2026-08-17
                    // amendment), so a same-team upgrade gets ONE bounded
                    // automatic silent re-read on the same single-flight
                    // worker. The expectation is still only an expectation —
                    // a wrong one raises the unsuppressible dialog, so the
                    // read never runs inline here either. Any non-success
                    // falls back to the explicit click: `AccessPending`
                    // clears the marker, and `Failed` keeps it but is stopped
                    // from looping by the one-shot guard.
                    self.carry_over_password(&mut config, &account, previous);
                    self.set_credential_state(CredentialState::AccessPending);
                    self.dispatch_automatic_credential_read(&account);
                } else {
                    // Item exists but this process has no proven grant: never
                    // attempt a decrypt from a routine path — on macOS 26 it
                    // would raise the consent dialog from the background.
                    self.carry_over_password(&mut config, &account, previous);
                    self.set_credential_state(CredentialState::AccessPending);
                }
            }
        }
        config
    }

    /// Copies the previously merged password into `config` when it belongs to
    /// the same account; returns true when a usable password was carried
    /// over. A password decrypted for an earlier account is never reused
    /// across a username change.
    fn carry_over_password(
        &self,
        config: &mut ValidatedConfig,
        account: &str,
        previous: Option<&ValidatedConfig>,
    ) -> bool {
        let Some(previous) = previous else {
            return false;
        };
        if previous.effective_username != account || previous.source.password.is_empty() {
            return false;
        }
        config.source.password = previous.source.password.clone();
        true
    }

    /// Whether a routine code path may dispatch a decrypt-class Keychain
    /// read on the strength of the grant marker ALONE: only when the marker
    /// proves a decrypt already succeeded for this account with THIS build.
    /// The XARA partition grant is pinned to the approving binary's cdHash,
    /// so measured self-signed a marker written by a different bundle version
    /// is not evidence — honoring it once raised the consent dialog from a
    /// background startup path after an upgrade (2026-08-05). A
    /// version-mismatched marker is not the end of the story any more: the
    /// same-team upgrade path is decided separately by
    /// `team_evidence_matches`, which consults the marker's third line
    /// against this build's runtime signing team (ADR 0016, 2026-08-17
    /// amendment). Fixture builds always allow it — they take the
    /// credential from the test environment and never touch Keychain.
    fn background_decrypt_allowed(&self, account: &str) -> bool {
        #[cfg(any(test, feature = "test-fixture"))]
        let allowed = {
            // Fixture builds take the credential from
            // `OPENCODESERVER_TEST_PASSWORD` and never touch Keychain, so the
            // marker gate is bypassed by default: honoring it would strand
            // every fixture agent in `AccessPending`. Tests that exercise the
            // gate ITSELF opt back into the real evidence check with this
            // per-process knob. Like the password hook it lives inside the
            // test cfg and never compiles into a production build, where the
            // check below is the only path.
            if std::env::var_os("OPENCODESERVER_TEST_ENFORCE_GRANT").is_some() {
                self.credential_grant.covers(account, crate::BUNDLE_VERSION)
            } else {
                true
            }
        };
        #[cfg(not(any(test, feature = "test-fixture")))]
        let allowed = self.credential_grant.covers(account, crate::BUNDLE_VERSION);
        allowed
    }

    /// Whether the grant marker's recorded signing team for `account` matches
    /// this running build's OWN team identity, read from its code signature
    /// at runtime (ground truth — a build-time bake could be inherited by an
    /// atomically replaced binary). Both sides must be present and equal: an
    /// ad hoc/unsigned build records and yields no team evidence, so dev
    /// builds never take the automatic path, and a team change (e.g. the ADR
    /// 0021 signing-identity migration itself, or a future certificate
    /// reissue under a different team) correctly falls back to the manual
    /// grant flow. The measured basis is ADR 0016's 2026-08-17 amendment:
    /// under team-anchored signing the partition grant survived a same-team
    /// cdHash change, a real SecItemUpdate, and process restarts, all silent;
    /// the self-signed measurements remain the reason this function existing
    /// is not enough — the evidence must actually match.
    fn team_evidence_matches(&self, account: &str) -> bool {
        match (
            self.credential_grant.team_evidence(account),
            (self.self_team)(),
        ) {
            (Some(recorded), Some(own)) => recorded == own,
            _ => false,
        }
    }

    /// Dispatches the one automatic, non-interactive silent re-read a
    /// team-evidence path is allowed per account per process run (ADR 0016,
    /// 2026-08-17 amendment). No-ops when a read is already in flight or the
    /// account's one-shot budget is spent; returns true when the dispatch
    /// was requested. The read goes through the same single-flight worker as
    /// every other decrypt: on macOS 26 a wrong expectation raises the
    /// unsuppressible consent dialog, which must never block the event loop.
    fn dispatch_automatic_credential_read(&mut self, account: &str) -> bool {
        if self.credential_refresh.is_some()
            || !self.automatic_read_attempted.insert(account.to_owned())
        {
            return false;
        }
        log(
            LogLevel::Notice,
            "The grant marker was recorded under this signing team; attempting one automatic silent Keychain re-read",
        );
        self.request_credential_refresh_for(account);
        true
    }

    fn record_credential_grant(&self, account: &str) {
        // An ad hoc/unsigned build has no team: the marker is still recorded
        // with an empty team line so the schema stays strict and dev-build
        // behavior stays observable; the empty line simply yields no team
        // evidence on load.
        let team = (self.self_team)().unwrap_or_default();
        if let Err(error) = self
            .credential_grant
            .record(account, crate::BUNDLE_VERSION, &team)
        {
            log(
                LogLevel::Error,
                &format!("Unable to persist the Keychain grant marker: {error}"),
            );
        }
    }

    fn clear_credential_grant(&self, account: &str) {
        if let Err(error) = self.credential_grant.clear_for(account) {
            log(
                LogLevel::Error,
                &format!("Unable to remove the Keychain grant marker: {error}"),
            );
        }
    }

    /// Handles the GUI's `credential_changed` notice: the Keychain item was
    /// just rewritten, so the in-memory password can no longer be trusted as
    /// current. The state flips to `AccessPending` and the old password is
    /// kept in memory either way: the RUNNING OpenCode still uses it, so
    /// supervision and health checks stay consistent until the user-driven
    /// restart replaces the process.
    ///
    /// What happens to the grant marker depends on the team evidence. The
    /// "SecItemUpdate wipes the XARA partition grant" rule was measured
    /// self-signed (2026-08-05); under team-anchored signing the 2026-08-17
    /// real-product measurements (ADR 0016 amendment, macOS 26.6.1) show the
    /// grant surviving in-place password changes — every post-update read
    /// was silent. So:
    ///
    /// - Marker team == this build's own team: the marker is KEPT and one
    ///   bounded automatic re-read is dispatched on the single-flight
    ///   worker, so a real password change is re-applied silently. The read
    ///   never runs inline — a wrong expectation would raise the
    ///   unsuppressible consent dialog from this background path.
    /// - No matching team evidence (the self-signed fallback rule, an ad hoc
    ///   dev build, a team change): the marker is retired exactly as before.
    ///   The recorded "a decrypt already succeeded" evidence may be spent;
    ///   keeping it would let the next OpenCodeServerAgent start (e.g.
    ///   "Repair OpenCodeServerAgent…") attempt a marker-permitted background
    ///   decrypt and raise the consent dialog from a background process —
    ///   pre-empting the explicit Grant Access flow that is supposed to own
    ///   the next prompt.
    pub(crate) fn mark_credential_changed(&mut self, account: &str) {
        self.credential_explicitly_removed = false;
        self.credential_stale = true;
        self.set_credential_state(CredentialState::AccessPending);
        if self.team_evidence_matches(account) {
            log(
                LogLevel::Notice,
                "The Keychain credential was changed in Settings; the team-anchored grant survives the update, re-reading silently",
            );
            self.dispatch_automatic_credential_read(account);
        } else {
            self.clear_credential_grant(account);
            log(
                LogLevel::Notice,
                "The Keychain credential was changed in Settings; it will be re-read on the next Allow Keychain Access authorization",
            );
        }
    }

    /// Handles Settings' explicit `credential_removed` notice. A successful
    /// SecItemDelete has already proved that the item is absent, so treating
    /// this like a rewrite (`AccessPending`) would be both incorrect and
    /// dangerous: the next Restart would refuse to launch even though the
    /// user explicitly chose OpenCode's native unauthenticated mode.
    ///
    /// Keep `active_config` untouched while the old OpenCode process is still
    /// running so authenticated health checks continue to describe that
    /// process accurately. Clear only `latest_config`; the resulting
    /// fingerprint difference reports `config_pending`, and Restart then
    /// launches the password-free configuration. No Keychain query is made.
    pub(crate) fn mark_credential_removed(
        &mut self,
        account: &str,
        latest: &mut Option<ValidatedConfig>,
    ) {
        self.credential_explicitly_removed = true;
        self.credential_stale = false;
        self.set_credential_state(CredentialState::NotConfigured);
        self.clear_credential_grant(account);
        if let Some(config) = latest.as_mut() {
            config.source.password.clear();
        }
        log(
            LogLevel::Notice,
            "The Keychain credential was removed in Settings; the latest configuration now uses OpenCode's native unauthenticated mode",
        );
    }

    /// Applies a credential state change and logs the transition at Notice:
    /// the 2026-08-04 prompt-storm incident showed that silent credential
    /// states make field diagnosis impossible.
    fn set_credential_state(&mut self, state: CredentialState) {
        if self.credential_state != state {
            log(
                LogLevel::Notice,
                &format!(
                    "Credential state changed from {:?} to {:?}",
                    self.credential_state, state
                ),
            );
            self.credential_state = state;
        }
    }

    /// Starts the single-flight decrypt-class Keychain read for `account`.
    /// The read blocks on the system consent dialog when the caller is not
    /// authorized, so it always runs on a dedicated worker — never inline on
    /// the event loop — and a second request while one is in flight is a
    /// no-op. Callers: the Settings "Allow Keychain Access…" command (interactive,
    /// prompt expected), `merge_credentials` (marker-proven background read,
    /// prompt not expected but possible after a revocation), and the
    /// team-evidence paths via `dispatch_automatic_credential_read`
    /// (one bounded automatic attempt per account per process run, ADR 0016
    /// 2026-08-17 amendment).
    fn request_credential_refresh_for(&mut self, account: &str) {
        if self.credential_refresh.is_some() {
            return;
        }
        let username = account.to_owned();
        let (sender, receiver) = mpsc::channel();
        let worker_username = username.clone();
        let read_password = self.read_password;
        let worker = thread::Builder::new()
            .name("credential-refresh".to_owned())
            .spawn(move || {
                let _ = sender.send(read_password(&worker_username));
            });
        match worker {
            Ok(worker) => {
                self.credential_refresh = Some(CredentialRefreshInFlight {
                    dispatched: Instant::now(),
                    account: username,
                    worker: Some(worker),
                    receiver,
                });
                self.credential_refresh_overdue_logged = false;
            }
            Err(error) => {
                log(
                    LogLevel::Error,
                    &format!("Unable to start the credential refresh worker: {error}"),
                );
            }
        }
    }

    /// The interactive read behind the Settings "Allow Keychain Access…" button: the
    /// one code path where raising the consent dialog is the goal.
    pub(crate) fn request_credential_refresh(&mut self, account: &str) {
        if self.credential_refresh.is_none() {
            self.credential_explicitly_removed = false;
            log(
                LogLevel::Notice,
                "Credential refresh requested; the system may prompt for Keychain authorization",
            );
        }
        self.request_credential_refresh_for(account);
    }

    pub(crate) fn poll_credential_refresh(
        &mut self,
        now: Instant,
        latest: &mut Option<ValidatedConfig>,
    ) -> Option<CredentialReadOutcome> {
        let mut in_flight = self.credential_refresh.take()?;
        match in_flight.receiver.try_recv() {
            Ok(read) => {
                let account = in_flight.account.clone();
                join_credential_refresh_worker(&mut in_flight.worker);
                self.credential_refresh_overdue_logged = false;
                self.apply_credential_read(read, &account, latest)
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                join_credential_refresh_worker(&mut in_flight.worker);
                self.credential_refresh_overdue_logged = false;
                log(
                    LogLevel::Error,
                    "Credential refresh worker finished without a result",
                );
                None
            }
            Err(mpsc::TryRecvError::Empty) => {
                if !self.credential_refresh_overdue_logged
                    && now.duration_since(in_flight.dispatched) > CREDENTIAL_REFRESH_OVERDUE
                {
                    self.credential_refresh_overdue_logged = true;
                    log(
                        LogLevel::Error,
                        "Keychain authorization prompt has been pending for over five minutes",
                    );
                }
                self.credential_refresh = Some(in_flight);
                None
            }
        }
    }

    fn apply_credential_read(
        &mut self,
        read: KeychainRead,
        account: &str,
        latest: &mut Option<ValidatedConfig>,
    ) -> Option<CredentialReadOutcome> {
        if self.credential_explicitly_removed {
            log(
                LogLevel::Notice,
                "Discarding the Keychain read result: Settings removed the credential while the read was pending",
            );
            return None;
        }
        // The configuration may have moved to a different account while the
        // consent dialog was pending; a password decrypted for a stale
        // account must not leak into the new account's configuration.
        let current_account = latest
            .as_ref()
            .map(|config| config.effective_username.clone());
        if current_account.as_deref() != Some(account) {
            log(
                LogLevel::Notice,
                "Discarding the Keychain read result: the configured username changed while \
                 the authorization dialog was pending",
            );
            return None;
        }
        match read {
            KeychainRead::Found(password) => {
                self.set_credential_state(CredentialState::Available);
                self.credential_stale = false;
                // A success — interactive ("Allow Keychain Access…") or a
                // proven silent read alike — records fresh grant evidence for
                // the current build below, so the account's one-shot
                // automatic-read budget starts over: a later password change
                // or same-team upgrade may silently re-read again.
                self.automatic_read_attempted.remove(account);
                self.record_credential_grant(account);
                if let Some(config) = latest.as_mut() {
                    config.source.password = password;
                }
                log(
                    LogLevel::Notice,
                    "Keychain credential access was granted; the password was applied to the in-memory configuration",
                );
                Some(CredentialReadOutcome::PasswordApplied)
            }
            KeychainRead::NotConfigured => {
                self.set_credential_state(CredentialState::NotConfigured);
                self.credential_stale = false;
                self.clear_credential_grant(account);
                if let Some(config) = latest.as_mut() {
                    config.source.password.clear();
                }
                log(
                    LogLevel::Notice,
                    "No OpenCode password is stored in Keychain",
                );
                None
            }
            KeychainRead::AccessPending => {
                // The user dismissed or denied the prompt. Clear the grant
                // marker so background paths stop attempting decrypts (each
                // would re-raise the dialog on macOS 26); keep the previous
                // in-memory credential and the soft state, delete nothing.
                self.set_credential_state(CredentialState::AccessPending);
                self.clear_credential_grant(account);
                log(LogLevel::Notice, "Keychain authorization was not granted");
                None
            }
            KeychainRead::Failed(code) => {
                self.set_credential_state(CredentialState::AccessPending);
                log(
                    LogLevel::Error,
                    &format!("Keychain credential read failed with OSStatus {code}"),
                );
                None
            }
        }
    }
}

/// Outcome of applying one completed Keychain read, reported to the
/// Supervisor for the side effects the controller must not own.
pub(crate) enum CredentialReadOutcome {
    /// A found password was applied; the Supervisor resumes a pending start
    /// and rechecks a stale-configuration attachment.
    PasswordApplied,
}

impl Supervisor {
    /// Thin delegator: merges the Keychain credential into `config`, using
    /// the current `latest_config` as the carry-over source.
    pub(super) fn merge_credentials(&mut self, config: ValidatedConfig) -> ValidatedConfig {
        self.credentials
            .merge_credentials(config, self.latest_config.as_ref())
    }

    pub(super) fn mark_credential_changed(&mut self) {
        let account = self
            .latest_config
            .as_ref()
            .map(|config| config.effective_username.clone())
            .unwrap_or_else(|| "opencode".to_owned());
        self.credentials.mark_credential_changed(&account);
    }

    pub(super) fn mark_credential_removed(&mut self) {
        let account = self
            .latest_config
            .as_ref()
            .map(|config| config.effective_username.clone())
            .unwrap_or_else(|| "opencode".to_owned());
        self.credentials
            .mark_credential_removed(&account, &mut self.latest_config);
    }

    pub(super) fn request_credential_refresh(&mut self) {
        let account = self
            .latest_config
            .as_ref()
            .map(|config| config.effective_username.clone())
            .unwrap_or_else(|| "opencode".to_owned());
        self.credentials.request_credential_refresh(&account);
    }

    pub(super) fn poll_credential_refresh(&mut self, now: Instant) {
        if self
            .credentials
            .poll_credential_refresh(now, &mut self.latest_config)
            .is_some()
        {
            // A start refused on the missing grant (or waiting behind
            // one) resumes now that the credential is usable.
            if self.process.is_none()
                && !self.unverified_process_record
                && self.runtime.desired_state == DesiredState::Running
            {
                self.restart_attempt_index = 0;
                self.next_restart = None;
                self.next_port_retry = None;
                self.port_release_deadline = None;
                self.network_wait_deadline = None;
                self.pending_start_trigger = None;
                self.start_now(StartTrigger::Cold);
            }
            // A stale-configuration attachment whose fingerprint only
            // mismatched because the credential was unreadable can now
            // be verified and upgraded to full supervision in place.
            self.recheck_stale_process();
        }
    }
}
