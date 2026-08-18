# ADR 0019: Supervisor decomposition — three state/decision extractions

Date: 2026-08-14

## Status

Accepted and acceptance-verified. Implemented across four stacked
branches (a pure-move module split plus the three extractions below);
built as `CFBundleVersion` 73, installed, and converged healthy
on the local acceptance Mac. Since landed on `main`: the stack was
rebased, so the extraction work appears on `main` as commits including
`c1bb6e4`, `8c8e96d`, `f3a7e17`, and `8405930` (verified 2026-08-16).

## Context

`Supervisor` had grown into a 43-field struct whose logic lived in a
single 2598-line `rust/src/supervisor.rs`. A first, deliberately
pure-move refactor (commit `13e8d42`) split that file into per-concern
submodules (`lifecycle`, `reattach`, `credential`, `version`, `health`)
with zero logic change. That split improved navigability but — by
construction — did not reduce complexity: every method stayed an
`impl Supervisor` method mutating the same shared struct, and the
43-field god-object was intact. Review confirmed the split was a
reorganization, not a simplification.

This ADR records the three *behavior-preserving* extractions that
followed, each of which reduced a distinct kind of complexity, and the
deliberate decision to stop at three.

## Decision

Reduce `Supervisor` complexity by extracting three cohesive concerns,
each chosen because it had a **clean, testable boundary**. Three
extraction patterns emerged, chosen per concern:

1. **Stateful controller** — a cohesive cluster of private state with
   narrow cross-boundary access (credential).
2. **Stateful coordinator** — single-flight machinery that owns its own
   worker and retry cadence (version query).
3. **Stateless pure policy** — an imperative nested-match decision that
   holds no state of its own (reattachment).

Each extraction followed the same discipline: write a boundary design
note first; add characterization tests against the *current* behavior
and prove them green *before* moving anything; migrate in small,
suite-green commits; finish by privatizing the moved fields and letting
the compiler enumerate every violation; prove the encapsulation with a
negative compile test (temporarily write a private field from outside,
confirm the build fails, revert).

## The three extractions

### 1. CredentialController (stateful controller)

- **Moved:** the six credential fields (`credential_state`,
  `credential_grant`, `credential_refresh`,
  `credential_refresh_overdue_logged`, `credential_stale`,
  `credential_explicitly_removed`) into a private `CredentialController`.
- **Boundary:** Keychain I/O is constructor-injected
  (`probe_item`, `read_password`); configuration crosses as a parameter;
  the `CredentialGrant` marker is the only persistence surface. The
  cross-concern side effect (resume a pending start after a password is
  applied) is returned as a `CredentialReadOutcome` and executed by the
  Supervisor delegate — never hidden inside the controller.
- **Result:** `Supervisor` 43 → 38 fields. Only `state()` and
  `refresh_in_flight()` cross the boundary in production; the other
  ~40 `&mut self` methods can no longer touch credential state
  (compile-enforced).
- **Tests:** 15 characterization tests; existing unit tests adapted to
  `#[cfg(test)]` accessors (assertions unchanged).

### 2. VersionQueryCoordinator (stateful coordinator)

- **Moved:** the four installed-version-query machinery fields
  (`version_query`, `version_query_overdue_logged`,
  `last_version_attempt`, `version_query_quarantined_executable`).
- **Stayed:** `installed_version`/`running_version` remain on
  `Supervisor` — `running_version` has four writers
  (health/reattach/lifecycle/version). The quarantine circuit breaker
  is self-contained in the coordinator.
- **Boundary tension:** `version_query_due` selects the retry interval
  from `installed_version.is_some()`. Rather than read `Supervisor`
  state back, the Supervisor passes a `has_version: bool` signal in. The
  query function is injected (`query_installed_version`), making the
  coordinator unit-testable with zero subprocess spawn.
- **Result:** 38 → 35 fields. 10 characterization tests; existing unit
  tests unchanged (they never touched version fields).

### 3. ReattachmentPolicy (stateless pure policy)

- **Moved:** nothing. This is the only extraction that reduced
  *cognitive* complexity rather than the field count.
- **Extracted:** the ~140-line `try_reattach` nested match (five levels,
  I/O interleaved) into a stateless, side-effect-free
  `reattach_policy` module: `decide_initial` (gates 0–3) and
  `decide_after_health` (gate 4), bracketing the conditional
  `health::check`. The policy imports only `CredentialState`,
  `ValidatedConfig`, `RecordIdentity` — no `health`, no `&self`, no
  logging, no I/O (grep-proven).
- **Key invariant preserved:** `health::check` stays *conditional* — it
  runs only when gates 0–3 pass. Lifting it to an unconditional call
  would probe a possibly-foreign/dead PID's endpoint and would be a
  behavior change. The orchestrator gathers facts, calls the pure
  functions, and flat-dispatches the returned action enum.
- **Result:** `try_reattach` 144 → 89 lines; the decision is now
  exhaustively unit-testable without a `Supervisor` or any I/O. 13
  table-driven tests cover every identity × config × credential ×
  health combination; 7 new end-to-end integration tests lock the
  arms not deterministically constructible.

Field-count progression: **43 → 38 → 35 → 35**.

## Acceptance

Built as `CFBundleVersion` 73 (bumped from 72) with the stable
`OpenCodeServer Local Signing` identity; `codesign --verify --deep
--strict` and the stable-authority check pass on the outer app, the
nested `OpenCodeServerAgent.app`, and `opencodeserverctl`. Installed
transactionally over Build 72 (Designated Requirements mutually
compatible). After reopening, Service Management refreshed
OpenCodeServerAgent from the new bundle (new PID,
`bundle_version: "73"`). Authenticated IPC reachable; FDA `verified`;
endpoint `10.0.0.254:4096`; OpenCode `running`/`installed` `1.18.18`.

As expected under ADR 0016, the version bump wiped the XARA
`partition_id` grant and left the agent in `access_pending` until one
interactive "Allow Keychain Access…" click restored it; the agent then
converged to `server_state: healthy`, `password_state: configured`,
`config_pending: false`, `last_error: null`. The `last_error` during
the pending window was exactly the reason string the refactored
`decide_initial` gate-3 `AttachStaleConfig` arm emits for
`AccessPending` — direct evidence the new decision code runs in
production.

## Why we stop at three

The remaining complexity center is the process lifecycle state machine
in `lifecycle.rs` (start/stop/restart, ~12 deadline-driven fields:
`stop_deadline`, `restart_after_stop`, `next_restart`,
`next_port_retry`, `port_release_deadline`, `network_wait_deadline`,
`pending_start_trigger`, `restart_attempt_index`,
`recovery_incident_active`, plus the health-tracking trio). It is
**not** in the same safe, behavior-preserving category as the three
done:

- **Time is intrinsic.** The cluster is dominated by deadline
  scheduling; characterization testing requires an injected fake clock —
  far heavier scaffolding than the first three needed.
- **Subprocess spawn is at the core**, interleaved with the deadline
  state machine; clean injection means spawn + port-probe + clock, not a
  single function pointer.
- **The boundary is wide**, not narrow: restart backoff reads health and
  server state; start refuses on credential and unverified-process state.

A `LifecycleController` extraction would *move* a complex state machine
into its own type — genuine encapsulation, but not the cognitive win the
reattachment extraction was. The smaller sub-clusters (restart backoff,
port wait, health tracker) are extractable but low-payoff: each would
shrink the struct by 3–4 fields without removing the deadline
intricacy, closer to the reorganization the first module split already
was. `recheck_stale_process` shares gate-4's tail but reusing the
policy would force a strict/lenient mode parameter into the
otherwise-clean decision function; declined.

Stopping criterion: extract when a concern has a clean, testable
boundary and the extraction *removes or makes testable* real
complexity. Revisit lifecycle only if its complexity is causing
documented pain (bugs or hard-to-add features), and treat that as a
separate, heavier engagement with fake-clock test infrastructure — not
another instance of this template.

## Consequences

- `Supervisor` is 35 fields (from 43); two concerns are behind
  compile-enforced private controllers, and the worst single method is
  a pure, exhaustively-tested decision plus a thin orchestrator.
- Three reusable extraction patterns are established and
  acceptance-verified; future extractions should match the
  characterization-test-first / negative-compile discipline.
- The four refactor branches were landed on `main` in stack order
  (module split → credential → version → reattachment), rebased; the
  branch-tip SHAs below are the pre-merge tips (verified 2026-08-16).
- Lifecycle is explicitly *not* extracted by decision; this ADR is the
  record that prevents a future contributor (or agent) from churning it
  without the heavier justification.

## Implementation references

- Branches (stacked, since rebased onto `main`):
  `refactor/supervisor-module-split` (`13e8d42`),
  `refactor/extract-credential-controller` (`266c122`),
  `refactor/extract-version-query-coordinator` (`0573939`),
  `refactor/extract-reattachment-policy` (`57c82e5`).
- Boundary designs: `docs/refactor/credential-controller-boundary.md`,
  `docs/refactor/version-query-coordinator-boundary.md`,
  `docs/refactor/reattachment-policy-boundary.md`.
- Code: `rust/src/supervisor/{credential,version,reattach_policy}.rs`
  and their `*_tests.rs`.
- Acceptance: installed Build 73; `opencodeserverctl status --json`
  reports `bundle_version: "73"`, `server_state: "healthy"`.
