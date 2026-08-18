# ReattachmentPolicy — Boundary Design

Status: design accepted, implementation in progress on branch
`refactor/extract-reattachment-policy` (built on the
VersionQueryCoordinator extraction; this document mirrors that template).

This document records the boundary for extracting the decision logic of
`Supervisor::try_reattach` into a stateless pure-function module
(`reattach_policy`). Unlike the CredentialController and
VersionQueryCoordinator extractions, this one moves **no state at all**:
the policy owns zero fields. The complexity win is different — the
~140-line nested `match` with interleaved I/O becomes (a) a thin
orchestrator that collects facts, asks the policy, and flat-dispatches
the returned action, and (b) an exhaustively unit-testable pure decision
table that needs no `Supervisor` and performs no I/O.

AGENTS.md hard constraints apply unchanged: a record whose kernel
identity was never confirmed authorizes nothing; missing or
identity-mismatched records are cleared without signaling; inspection
failure keeps the record (fail-closed); an identity-VERIFIED but
configuration-mismatched process is adopted as a managed
stale-configuration process, never abandoned; escape and identity errors
never authorize a signal.

## 1. The policy is stateless — Supervisor field count does not change

`Supervisor` keeps all 35 of its fields; no field moves, no field is
added. The previous two extractions paid for themselves by shrinking the
god object; this one pays for itself by shrinking a method and by making
its decision logic testable without a live process, a configuration, a
health endpoint, or a Keychain. `try_reattach` shrinks from ~140 lines of
nested matching to a fact-collecting orchestrator (~35 lines of
dispatching), and every decision arm becomes a row in an exhaustive pure
unit-test table.

## 2. The decision flow, in the shape the refactor must preserve

The current `try_reattach` implements five gates:

1. **gate 0 — identity_unconfirmed**: only a provably gone PID
   (`RecordIdentity::Missing`) makes an unconfirmed record stale; any
   other observation keeps it unverified (no signal, no takeover, no
   second OpenCode).
2. **gate 1 — first identity inspection** (`inspect_record_identity`):
   `Missing`/`Mismatched` discard the record; `Current` and
   `ExecutableVanished` proceed (the latter logs one Notice);
   `ExecutableMismatch` attaches unconfirmed; `GroupEscaped` keeps
   unverified; an inspection error keeps unverified (fail-closed).
3. **gate 2 — configuration presence**: no configuration ⇒ the record
   stays unverified ("configuration is unavailable").
4. **gate 3 — configuration fingerprint**: a mismatch adopts the
   identity-verified process as a managed stale-configuration process;
   the reason string differs by credential state (`AccessPending` ⇒
   "grant Keychain access, then restart", otherwise "restart to apply the
   changes").
5. **gate 4 — authenticated health check + second identity inspection**:
   only `health::check` returning `healthy` **and** a second inspection
   returning `Current`/`ExecutableVanished` reattaches; every other
   combination discards, attaches unconfirmed, or keeps unverified with a
   gate-4-specific reason. The reattach applies the version, re-stamps
   the fingerprint, attaches the managed process, and logs one Notice.

## 3. Two-phase pure decision (mandatory, because `health::check` is conditional I/O)

`health::check` connects to the recorded process's endpoint. It must stay
**conditional**: running it when gate 0–3 already failed would probe the
endpoint of a possibly foreign, possibly dead process — a behavior
change. The refactor therefore splits the decision into two pure
functions and lets the orchestrator perform the I/O between them:

```rust
pub(super) fn decide_initial(
    identity_unconfirmed: bool,
    identity: Result<RecordIdentity, ()>,
    config: Option<ValidatedConfig>,
    config_matches: bool,
    credential_state: CredentialState,
) -> InitialAction
```

encodes gate 0–3 and returns either a terminal action or
`NeedsHealthCheck { config: ValidatedConfig }`. Only for
`NeedsHealthCheck` does the orchestrator run `health::check(&config,
HEALTH_TIMEOUT)` and a second `inspect_record_identity`, then call:

```rust
pub(super) fn decide_after_health(
    identity: Result<RecordIdentity, ()>,
    health: HealthVerdict<'_>,
    config: ValidatedConfig,
) -> FinalAction
```

(`HealthVerdict` is a policy-local enum — `Healthy { version: &str }` /
`Unhealthy` / `Failed` — the orchestrator's digest of the real
`health::check` result.)

which encodes gate 4 and returns `ReattachHealthy { version, config }`
plus the same terminal actions.

Notes on the signature:

- `identity_unconfirmed` is a scalar parameter, not read from
  `&ProcessRecord` — the policy does not depend on `ProcessRecord` at
  all, keeping it fully testable from literals. (The orchestrator passes
  `record.identity_unconfirmed`; the record itself stays owned by the
  orchestrator and is moved into the dispatched helper.)
- `Result<RecordIdentity, ()>`: the policy distinguishes `Ok` from `Err`
  only. The orchestrator maps the real `io::Result<RecordIdentity>`
  (`inspect_record_identity`) with `.map_err(|_| ())`; the error detail
  is unused by every current arm, so dropping it at the boundary is
  unobservable.
- `config: Option<ValidatedConfig>` crosses by **value**: gate 2 needs
  only presence, and the value rides back inside `NeedsHealthCheck` /
  `ReattachHealthy`, so the health phase has a configuration by
  construction — no `unreachable!`/fallback path exists anywhere.
- `config_matches: bool` is computed by the orchestrator before calling
  the policy. When no configuration exists it is `false` (computed with
  `is_some_and`); the current code simply never computes it in that case,
  which is unobservable (`verifies` is pure).
- `Result<&HealthResult, ()>`: the policy reads `healthy` and clones
  `version`; the orchestrator keeps ownership of the `HealthResult`.
  `version` is cloned once for `running_version` and moved once into the
  process record, exactly as today. The orchestrator digests the real
  `health::check` result into a policy-local `HealthVerdict`
  (`Healthy { version }` / `Unhealthy` / `Failed`) before the call, so the
  policy module has no import of `crate::health` at all.

## 4. What crosses the boundary in each direction

| Direction | Item | How it crosses |
|---|---|---|
| Into policy | kernel identity facts | `Result<RecordIdentity, ()>` parameter (from `inspect_record_identity`, run by the orchestrator) |
| Into policy | health facts | `HealthVerdict` parameter (`Healthy { version }` / `Unhealthy` / `Failed`), digested by the orchestrator from `health::check` |
| Into policy | configuration presence/match | `Option<ValidatedConfig>` + `config_matches: bool` parameters |
| Into policy | credential state | `credential_state: CredentialState` parameter (gate 3 reason selection) |
| Into policy | unconfirmed flag | `identity_unconfirmed: bool` parameter (gate 0) |
| Out of policy | what to do | `InitialAction` / `FinalAction` enums carrying `&'static str` reasons and (on the healthy path) the version + configuration |
| Never crosses | `Supervisor`, `&self`, logging, any I/O | the policy module contains none (grep-provable, see §10) |

The policy performs no I/O and logs nothing. All side effects stay in the
existing Supervisor helpers (`discard_stale_process_record`,
`mark_unverified_process`, `attach_unconfirmed_process`,
`attach_stale_config_process`, and the inline success block), dispatched
by a flat `match` on the action. Ownership of `record` stays with the
orchestrator: it is borrowed nowhere by the policy and is moved into
exactly the helper the action names.

## 5. Diagnostic log ownership: orchestrator-owned (decision)

The policy never logs. Two Notice logs exist in the current method; both
become orchestrator responsibilities keyed off facts/actions the
orchestrator already holds:

- **`ExecutableVanished` at gate 1** — logged by the orchestrator
  immediately before calling `decide_initial`, guarded by
  `!identity_unconfirmed` (today the unconfirmed gate returns before any
  log). This preserves the exact ordering: the Notice is emitted before
  gates 2/3, even when the configuration is later missing.
- **the healthy-reattach Notice** — logged by the orchestrator inside the
  `FinalAction::ReattachHealthy` arm, before applying the success block,
  exactly where it fires today.

No action variant carries an optional diagnostic payload; the flat
dispatch stays reason-string-only. (Rejected alternative: attaching an
`Option<diagnostic>` to actions would smuggle log policy into the pure
module for two call sites and complicate the exhaustive tables.)

## 6. Optional opportunity evaluated: `recheck_stale_process` reuse — NOT adopted

`recheck_stale_process`'s tail is structurally similar (identity →
config → fingerprint → health → identity → success block), but its
semantics differ in every arm: every failure is a **silent keep** (the
stale attachment persists), the identity arms collapse to
`Current | ExecutableVanished` vs. everything else, and the success block
updates different state (`stale_config_process = false`,
`last_error = None`, a different Notice). Sharing the policy would need a
second mode ("silent keep" vs. "classified outcome") plus per-arm
mapping — doubling the action surface for exactly one caller without
removing a single Supervisor line. It is left as a recorded future
opportunity (§11); the core delivery covers `try_reattach` only, per the
task scope.

## 7. Behavior-fidelity notes (things the migration must not disturb)

1. Exactly **one** `inspect_record_identity` call happens per path today
   (gate 0 inspects only for unconfirmed records; gate 1 inspects only
   for confirmed ones). The orchestrator performs one up-front inspection
   and feeds the same result to gate 0 and gate 1 — same syscall count,
   same facts.
2. Gate 4's inspection runs **after** `health::check`, exactly as today.
3. On the healthy path: Notice log → `running_version = Some(version.clone())`
   → record `running_version = Some(version)` → fingerprint re-stamp →
   `ManagedProcess::attach` → runtime record → `active_config` →
   `ServerState::Healthy` / `HealthState::Healthy` →
   `process_started = Some(Instant::now())` → `persist_runtime()`. Order
   preserved byte-for-byte.
4. `discard_stale_process_record`'s boolean return stays ignored in
   `try_reattach` (only `check_unverified_process` uses it).
5. The `ExecutableVanished` Notice order relative to gate 2/3 failures
   (Notice first) is preserved via the orchestrator-side placement.
6. Reason strings move verbatim into the policy; they are decision
   outputs, so the policy — not the orchestrator — is their home.

## 8. Coverage audit (Phase 0 baseline)

Baseline: `cargo test --all --features test-fixture` = **112 unit +
58 integration = 170, all green** (commit 0573939).

Existing end-to-end coverage of the reattach tree:

| Gate / arm | Existing test |
|---|---|
| gate 0 unconfirmed + Missing | `unconfirmed_record_is_discarded_when_the_pid_is_gone` |
| gate 0 unconfirmed + live process | `supervisor_restart_keeps_an_unidentifiable_survivor_unverified_without_a_second_opencode` |
| gate 0 unconfirmed + unrelated PID occupant | `unconfirmed_record_blocks_a_second_start_while_an_unrelated_process_holds_the_pid` |
| gate 1 Missing | `stale_pid_is_discarded_before_configuration_comparison` |
| gate 1 Err | `process_inspection_error_remains_unverified_and_never_starts_a_server` |
| gate 1 Current → healthy reattach | `open_code_server_agent_restart_reattaches_the_same_authenticated_open_code` |
| gate 1 ExecutableVanished → healthy reattach | `reattach_succeeds_after_the_executable_file_is_deleted_during_runtime` |
| gate 1 ExecutableMismatch | `supervisor_reattaches_a_survivor_without_starting_a_second_opencode` |
| gate 3 mismatch, non-AccessPending reason | `live_process_with_changed_configuration_stays_managed_and_restart_converges` (asserts `config_pending` + "previous configuration"; the exact reason is not yet asserted) |
| gate 4 healthy + identity2 Current | the two healthy-reattach tests above |

Gaps closed by Phase 1 (end-to-end, all green on the unmodified code):

1. **gate 1 Mismatched** — a confirmed record whose PID now hosts an
   unrelated live process: discarded without signaling, fresh OpenCode
   starts.
2. **gate 1 GroupEscaped** — a confirmed record whose process escaped its
   dedicated group: stays unverified, nothing signaled (fixture +
   sentinel both survive), no second OpenCode.
3. **gate 2 configuration unavailable** — identity Current, config file
   corrupted: stays unverified, and the fixture's query-event trace
   proves the supervisor **never** probed the health endpoint
   (distinguishes this arm from gate 4).
4. **gate 3 AccessPending reason** — child-process agent pair with
   `OPENCODESERVER_TEST_PASSWORD` + `OPENCODESERVER_TEST_ENFORCE_GRANT`:
   `config_pending` + last_error "…— grant Keychain access, then
   restart".
5. **gate 3 non-AccessPending reason** — the existing changed-config test
   gains an exact-reason assertion ("…— restart to apply the changes").
6. **gate 4 Ok(not healthy)** — new fixture marker serves
   `{"healthy":false}`; unverified, process survives, and the
   query-event trace proves the endpoint WAS probed.
7. **gate 4 Err** — live fixture with no listener on the configured
   port: unverified, process survives.
8. **gate 4 identity2 GroupEscaped** — new fixture marker makes `serve`
   join a foreign sentinel group on the first accepted connection (the
   health request): the first inspection sees the correct group, the
   re-check after the authenticated healthy response sees the escape.

Not deterministically constructible end-to-end (recorded, not silently
skipped): gate 4 identity2 arms `Missing` / `Mismatched` /
`ExecutableMismatch` / `Err` require the recorded process to mutate its
kernel identity in the microsecond window between the authenticated
health response and the re-inspection; no sync seam exists on the
supervisor side, and adding one would change the product. These arms are
locked by the exhaustive pure-function table (Phase 3), which is the
primary deliverable for exactly this reason. Gate 1 `ExecutableVanished`
→ later gate failure combinations are likewise locked at the pure level.

## 9. Migration order (Mikado, every commit green)

1. Boundary design document (this file), committed first.
2. Phase 1 characterization tests + the two fixture markers
   (`unhealthy-health`, `escape-on-accept.pgid`), all green on the
   unmodified `try_reattach`. (Fixture changes are additive, test-only,
   and marker-gated per test-owned fixture copy.)
3. Introduce `rust/src/supervisor/reattach_policy.rs` with
   `InitialAction` and `decide_initial` only (the two-phase split means
   `FinalAction`/`decide_after_health` land in step 4, so no commit
   carries dead code past `clippy -D warnings`), and rewire gate 0–3:
   `try_reattach` collects facts, calls `decide_initial`, and
   flat-dispatches; the `NeedsHealthCheck` arm temporarily inlines
   health + gate 4 (behavior identical).
4. Add `FinalAction` + `decide_after_health` to the policy module and
   migrate gate 4; the orchestrator flat-dispatches. Reason strings and
   log ownership land per §4/§5.
5. Add `rust/src/supervisor/reattach_policy_tests.rs`: the exhaustive
   tables for `decide_initial` and `decide_after_health`.
6. Phase 3 DoD (below).

Each commit passes `cargo fmt --check`, `cargo clippy --all-targets
--features test-fixture -- -D warnings`, and the full suite.

## 10. Definition of Done

- Phase 1 end-to-end tests green through the new structure. Achieved:
  **65 integration tests** (58 baseline + 7 new) and **125 unit tests**
  (112 baseline + 13 policy-table tests), 190 total, all green.
- The exhaustive tables cover every
  `(identity_unconfirmed, identity, config, config_matches,
  credential_state) → InitialAction` and every
  `(identity2, health) → FinalAction` combination.
- `cargo fmt --check`; `cargo clippy --all-targets --features
  test-fixture -- -D warnings` clean.
- `git diff 0573939..HEAD -- rust/src/lib.rs Cargo.toml` empty; public
  API unchanged; no new dependencies, no `unsafe`.
- `try_reattach` line-count before/after reported; the method body is
  fact collection + flat dispatch, with all decision logic in
  `reattach_policy.rs`. Achieved: **144 → 86 lines** (the nested
  five-level `match` becomes two flat dispatches; the inline success
  block — side-effect application, ~20 lines — stays with the
  orchestrator per §4, which is why the method does not shrink to the
  bare dispatch).
- Purity proof by grep: `reattach_policy.rs` contains no `use
  crate::health`, no call to `inspect_record_identity`, no `&self` /
  `&mut self`, no `log(`, no `Instant::now`; its only imports are
  `CredentialState`, `ValidatedConfig`, and `RecordIdentity`.
- Supervisor field count unchanged (35).
- `xcodebuild test`: 77 Swift tests pass (TEST SUCCEEDED).

## 11. Recorded observations (no fixes in this refactor)

1. **Gate 4 treats `ExecutableVanished` like `Current`** (silent
   pass-through), while gate 1 logs a Notice for it — consistent with
   the kernel-identity argument (pid + start + uid + group still pin the
   process), but the asymmetry is deliberate and now locked by tests.
2. **The success block does not clear `last_error`** in `try_reattach`
   (unlike `recheck_stale_process`, which sets `last_error = None`).
   Unreachable-with-stale-error today because startup leaves
   `last_error = None`, but the asymmetry is preserved, not fixed.
3. **`attach_stale_config_process` does not set `process_started`**,
   while the healthy success block does; `recheck_stale_process` sets it
   on upgrade. Preserved as-is.
4. **The success block does not reset `stale_config_process`** — it can
   never be true on the startup reattach path, so the missing reset is
   latent; recorded.
5. **The unconfirmed gate and gate 1 each inspect the record in the
   current code** — one call per path in practice (each gate returns
   before the other runs); the migration collapses this to a single
   up-front inspection, which is syscall-count identical and
   unobservable.
6. **Gate 4's `Err` arm covers only inspection errors, not a distinct
   "process mutated" state**; the reason "process identity could not be
   rechecked after health verification" is the only Err path.
7. **Health verification uses `latest_config` while the re-stamped
   fingerprint comes from the same clone** — a configuration change
   between the clone and the re-stamp is not re-observed; single-threaded
   event loop makes this unobservable today.
