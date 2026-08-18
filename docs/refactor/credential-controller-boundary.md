# CredentialController — Boundary Design

Status: design accepted, implementation in progress on branch
`refactor/extract-credential-controller`.

This document records the boundary for extracting the credential *state* out
of `Supervisor` into a dedicated `CredentialController`. It is the design
contract for the behavior-preserving refactor: the controller owns the
credential state machine and its invariants, while I/O, configuration, and
persistence cross the boundary explicitly. It deliberately says nothing
about `VersionQueryCoordinator` and `ReattachmentPolicy`, which remain
future, separate tasks.

## 1. State that moves (the six fields)

> **Update, 2026-08-17:** the controller now holds seven state fields — the
> six below plus `automatic_read_attempted: HashSet<String>`, the one-shot
> guard for the team-anchored automatic silent read (ADR 0016, second
> 2026-08-17 amendment) — and three injected function fields (`probe`,
> `read_password`, `self_team`). The table below describes the original
> extraction and is kept as the historical record.

`CredentialController` becomes the sole owner of:

| Field | Type | Meaning |
|---|---|---|
| `credential_state` | `CredentialState` | `NotConfigured` / `AccessPending` / `Available` |
| `credential_grant` | `CredentialGrant` | persisted "a decrypt already succeeded for this account + this build" marker (ADR 0016) |
| `credential_refresh` | `Option<CredentialRefreshInFlight>` | single-flight decrypt-class read |
| `credential_refresh_overdue_logged` | `bool` | one-shot overdue log latch |
| `credential_stale` | `bool` | GUI reported a rewrite; in-memory password kept for the running process but not trusted as current |
| `credential_explicitly_removed` | `bool` | Settings deleted the item; generation barrier against in-flight decrypt results |

All six are **private** fields of `CredentialController`. The compiler —
not a convention — prevents `Supervisor`, `lifecycle.rs`, `reattach.rs`,
and `health.rs` from reading or writing them. A negative-compile test in the
final report proves this.

`Supervisor` shrinks from 43 fields to 37 and keeps exactly one new field:

```rust
credentials: CredentialController,
```

## 2. Dependencies cross the boundary explicitly

The credential logic is not self-sufficient. Every external touchpoint is
injected or passed as a parameter; nothing is reached through `&mut
Supervisor` or `&mut RuntimeState`.

### 2.1 Constructor-injected I/O handles

```rust
pub(crate) fn new(
    grant: CredentialGrant,
    probe: fn(&str) -> KeychainProbe,
    read_password: fn(&str) -> KeychainRead,
) -> Self
```

- `probe` — the attribute-only, never-raises-UI keychain probe used by
  `merge_credentials`. Production passes `crate::keychain::probe_item`;
  tests pass fakes, which also makes every probe branch unit-testable
  without the process-global `OPENCODESERVER_TEST_PASSWORD` environment
  hook.
- `read_password` — the decrypt-class read. The single-flight worker thread
  invokes this handle. Production passes `crate::keychain::read_password`;
  tests pass instant fakes, so the bounded-worker path is unit-testable.
- `grant` — the *minimal persistence surface* for the grant marker. No
  `&mut RuntimeState`, no `&mut Supervisor`, no file plumbing reaches the
  controller: `CredentialGrant` is already a single-purpose value that owns
  exactly one marker file, and `record_credential_grant` /
  `clear_credential_grant` operate on it. It is also state the controller
  must own, because `background_decrypt_allowed` consults it on every
  merge.

### 2.2 Configuration is passed in and out

- `merge_credentials(config: ValidatedConfig, previous:
  Option<&ValidatedConfig>) -> ValidatedConfig` — the merged config is the
  return value (as today). `previous` is the carry-over source (today read
  from `self.latest_config`); the call sites already hold exactly this
  value, so no order changes.
- `mark_credential_removed(account, latest: &mut Option<ValidatedConfig>)`
  — the controller clears the password in `latest_config` through the
  injected surface (today it reaches `self.latest_config`).
- `poll_credential_refresh(now, latest: &mut Option<ValidatedConfig>)` —
  `apply_credential_read` performs its current-account check and
  sets/clears the password on the injected config. `active_config` is
  deliberately not touched by the controller, preserving the documented
  "running process keeps its health credential until restart" invariant.

### 2.3 Account derivation stays on the Supervisor side

`mark_credential_changed`, `mark_credential_removed`, and
`request_credential_refresh` today derive the account from
`self.latest_config` with the `"opencode"` fallback. That derivation
(moved byte-for-byte) stays in the thin Supervisor delegators, which pass
the plain `account: &str` into the controller. The controller therefore
never reaches for `Supervisor`'s config.

### 2.4 Supervisor-side effects return as an outcome

`apply_credential_read` today ends the `KeychainRead::Found` arm by
resuming a refused start and rechecking a stale attachment — both are
Supervisor state-machine actions on fields the controller must not own.
They leave the controller as an explicit outcome:

```rust
pub(crate) enum CredentialReadOutcome {
    /// A found password was applied; the Supervisor should resume a
    /// pending start and recheck a stale-configuration attachment.
    PasswordApplied,
}
```

`poll_credential_refresh` returns `Option<CredentialReadOutcome>`. The
Supervisor delegator moves the resume block (byte-for-byte, including its
comments) out of `apply_credential_read` and runs it immediately after the
controller returns. Because the controller mutates state and config first
and the delegator resumes immediately after — inside the same `tick` — the
observable order of operations is unchanged.

## 3. The narrow interface

`CredentialController` exposes exactly this `pub(crate)` surface:

| Method | Purpose |
|---|---|
| `new(grant, probe, read_password)` | construction (see §2.1) |
| `state() -> CredentialState` | read for `status()`, `start_refusal`, `start_now`, `try_reattach`, `check_health` |
| `refresh_in_flight() -> bool` | read for `start_now` (do not flap a refusal while a read converges) and `next_deadline` scheduling |
| `merge_credentials(config, previous) -> ValidatedConfig` | configuration merge |
| `mark_credential_changed(account)` | `credential_changed` notice |
| `mark_credential_removed(account, latest)` | `credential_removed` notice |
| `request_credential_refresh(account)` | interactive "Allow Keychain Access…" read |
| `poll_credential_refresh(now, latest) -> Option<CredentialReadOutcome>` | worker convergence |

Everything else stays private to the controller:

- `carry_over_password`, `background_decrypt_allowed` — pure state/merge
  helpers;
- `record_credential_grant`, `clear_credential_grant` — grant-marker
  persistence through the injected `CredentialGrant`;
- `set_credential_state` — transition logging;
- `request_credential_refresh_for` — single-flight dispatch;
- `apply_credential_read` — read-result convergence.

The five `pub(super)` Supervisor methods of the same names remain as
**thin delegators** (`merge_credentials`, `mark_credential_changed`,
`mark_credential_removed`, `request_credential_refresh`,
`poll_credential_refresh`) so `handle`, `tick`, `with_options`,
`refresh_config`, and every test keep calling the same names with the same
signatures. The only call-site edits are the pure field *reads*
(`credential_state` → `credentials.state()`,
`credential_refresh.is_some()` → `credentials.refresh_in_flight()`).

## 4. Invariants owned by the type

The controller is the single place that can violate or preserve these:

1. `AccessPending` is soft: a transient probe/read failure never clears an
   in-memory password and never becomes `NotConfigured`. Only
   `errSecItemNotFound` (-25300) is absence.
2. The grant marker is written only after a successful decrypt
   (`KeychainRead::Found`), cleared when the item disappears, on
   `AccessPending` (declined/denied prompt), and when the item is rewritten
   (`credential_changed`) or deleted (`credential_removed`); it is bound to
   account AND bundle version (ADR 0016).
3. Single-flight: at most one decrypt-class read is ever in flight; a
   request while one is pending is a no-op.
4. `credential_explicitly_removed` is a generation barrier: any decrypt
   result already in flight when Settings deletes the item is discarded and
   can neither restore the password nor its grant.
5. The overdue log fires at most once per in-flight read.

## 5. Test seams

- Injected `probe` / `read_password` fakes make every merge/read branch
  deterministic at unit level. This replaces the pre-refactor reliance on
  the process-global `OPENCODESERVER_TEST_PASSWORD` /
  `OPENCODESERVER_TEST_ENFORCE_GRANT` hooks (which remain, untouched, for
  the integration suite).
- `#[cfg(test)]` helpers on the controller
  (`set_state_for_test`, `set_refresh_in_flight_for_test`, grant accessors,
  stale/removed accessors) keep the existing `supervisor/tests.rs`
  assertions intact while the fields go private. They compile only into
  test builds and never weaken the production boundary.

## 6. Coverage audit (Phase 0) and characterization gaps

Existing coverage before this refactor:

- Unit: 87 tests. Credential-adjacent: `first_credential_notice...`,
  `explicit_credential_removal...`, `credential_read_result_for_previous_
  username_is_discarded`, `unauthorized_message_distinguishes...`, plus
  `credential_grant.rs`'s own six marker tests.
- Integration: 58 tests, including grant-marker cold-start decrypt
  (matching marker), foreign-build marker rejection, account-scope
  inheritance, `credential_changed` flipping to `access_pending`, and
  start refusal while access is pending — all through the real agent
  binary with the environment hooks.

Gaps the new characterization tests close at the unit level:

1. `merge_credentials` probe branches: `NotConfigured` (clears password +
   grant), `Exists` + carried password → `Available`, `Exists` + stale →
   `AccessPending` without dispatch, `Exists` + no carry + grant gate open
   → `AccessPending` with a dispatched single-flight read, `Exists` + no
   carry + gate closed → empty password and no dispatch (the
   counterintuitive fail-closed case AGENTS.md calls out).
2. `merge_credentials` `Failed(code)` probe: carry-over and soft
   `AccessPending`. (Only reachable with injected fakes — listed as a
   pre-refactor gap; becomes covered by the same tests post-refactor.)
3. Grant marker: recorded after a `Found` apply; cleared after decline;
   cleared on removal; account/bundle-version binding observable via
   `CredentialGrant::load`.
4. `poll_credential_refresh`: single-flight no-op; overdue latch fires once
   and resets on completion; disconnected worker; each
   `KeychainRead` result's state/password/grant effects.
5. `request_credential_refresh`: clears `credential_explicitly_removed`
   only when no read is in flight (current behavior, locked by a test).

## 7. Recorded observations (no fixes in this refactor)

These look like plausible bugs or warts. Per the task rules they are
recorded here and left untouched; changing them belongs to a separate
change with its own decision.

1. **`KeychainRead::Failed` does not clear the grant marker**, while
   `AccessPending` does. A generic OSStatus is arguably not evidence that
   the XARA partition grant is spent, so the asymmetry may be intentional
   — but it means a persistently failing read can keep re-attempting
   background decrypts through the marker gate. Recorded, not changed.
2. **`request_credential_refresh` only clears `credential_explicitly_
   removed` when no read is in flight.** If Settings removes the item
   while a read is pending and the user then clicks "Allow Keychain
   Access…", the click is a single-flight no-op and the pending read is
   later discarded, so a second click is required to converge. Recorded,
   not changed.
3. **`merge_credentials`'s carried-password branch sets `Available`
   without consulting the grant marker.** This is correct — no decrypt
   happened — but worth stating as a locked behavior: an agent that holds
   an in-memory password keeps `Available` even if the marker was cleared.
4. **The unproven-grant branch does not actively clear an incoming
   password.** When `carry_over_password` fails, the incoming config
   passes through untouched, so a synthetically injected password would
   survive the merge. Unreachable in production: passwords are never
   written to `config.plist`, so the loader's configs always start empty.
   Recorded, not changed.

## 8. Migration order (Mikado, every commit green)

1. Boundary design document (this file) + characterization tests, all
   green on the unmodified `Supervisor`.
2. Introduce `CredentialController` as the state holder: the six fields
   move (temporarily `pub(super)`), `Supervisor::new` constructs it, all
   sites and tests access it through `supervisor.credentials`. Test helpers
   added in the same commit.
3. Migrate the twelve methods into `impl CredentialController` with the
   injected dependencies and the outcome return; add the thin Supervisor
   delegators. Call sites and test assertions stay byte-identical.
4. Privatize the six fields and let the compiler enumerate every
   remaining direct access; replace them with `state()`,
   `refresh_in_flight()`, and the test helpers. Compilation success proves
   the boundary is real.
5. DoD: negative-compile test (temporary illegal write in `lifecycle.rs`
   must fail to build; then revert), grep evidence, field count 43 → 37,
   `lib.rs` byte-identical, full green suite.
