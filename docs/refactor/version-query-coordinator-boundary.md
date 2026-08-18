# VersionQueryCoordinator — Boundary Design

Status: design accepted, implementation in progress on branch
`refactor/extract-version-query-coordinator` (built on the
CredentialController extraction; this document mirrors that template).

This document records the boundary for extracting the installed-version
query mechanism out of `Supervisor` into a `VersionQueryCoordinator`. The
controller owns the query *mechanism* (single-flight worker, overdue
latch, retry cadence, quarantine circuit breaker); product data and
process side effects stay on the Supervisor side and cross the boundary
explicitly. AGENTS.md hard constraints apply: the query stays
single-flight, bounded, dispensable, and gains no guardian hierarchy; an
observed process-group escape or identity anomaly opens the circuit
breaker for that executable until its path changes or
OpenCodeServerAgent restarts.

## 1. State that moves (the four mechanism fields)

`VersionQueryCoordinator` becomes the sole owner of:

| Field | Type | Meaning |
|---|---|---|
| `version_query` | `Option<VersionQueryInFlight>` | the single-flight worker (dispatched instant, generation, executable, JoinHandle, result channel) |
| `version_query_overdue_logged` | `bool` | one-shot "exceeded 2× observation bound" latch |
| `last_version_attempt` | `Option<Instant>` | retry-cadence anchor |
| `version_query_quarantined_executable` | `Option<PathBuf>` | circuit breaker: escape/identity anomaly disables queries for this executable |

All four are **private**; the compiler enforces that `Supervisor`,
`lifecycle.rs`, etc. cannot touch them. `Supervisor` shrinks from 38 to 34
fields and keeps one new field:

```rust
version_queries: VersionQueryCoordinator,
```

## 2. What deliberately stays on Supervisor (default decision, argued)

`installed_version` and `running_version` do **not** move into the
controller:

- `running_version` is written by four owners — `check_health`
  (health.rs), `try_reattach`/`recheck_stale_process` (reattach.rs),
  `start_now` (lifecycle.rs, resets it), and `apply_installed_version`
  (version.rs, backfill). Moving it would either drag health/reattach
  logic into the controller or force a second shared owner — a
  re-coupling the extraction exists to remove.
- `installed_version` is written only by version code, which makes moving
  it *possible* — but it is read in two supervisor-side places: `status()`
  (mod.rs) and the retry-interval selection inside `version_query_due`.
  The interval selection is the core boundary tension (see §3). Keeping
  `installed_version` with the Supervisor means the tension resolves into
  one explicit boolean signal instead of a data accessor pair plus a
  second owner for `status()`. The chosen split also matches the
  CredentialController precedent: mechanism state in, product state out.

The controller therefore returns a `VersionQueryOutcome` and the
Supervisor applies it — the `CredentialReadOutcome::PasswordApplied`
pattern from the credential template:

```rust
pub(crate) enum VersionQueryOutcome {
    Available(String),
    Unavailable,
    Quarantined,
}
```

The Supervisor-side application is exactly the remainder of
`apply_installed_version` (write `installed_version`, backfill
`running_version`, update the process record, `persist_runtime`) plus the
`Quarantined` application (clear `installed_version`, Fault log).

## 3. The `version_query_due` interval tension, resolved explicitly

`version_query_due` picks `VERSION_INTERVAL` when a version is already
installed and `VERSION_RETRY_INTERVAL` otherwise. That is the one read of
Supervisor data the mechanism needs. It crosses the boundary as an
explicit boolean signal — never as `&Supervisor`:

```rust
pub(crate) fn due(&self, now: Instant, executable: Option<&Path>, has_version: bool) -> bool
```

- `executable` — the configured executable path; `None` means no
  configuration, so nothing is due (preserves the current
  `latest_config`-absent early return).
- `has_version` — `installed_version.is_some()`, computed by the
  Supervisor-side caller. The interval selection reads only this signal.

The circuit breaker is fully self-contained inside the four fields:
`due()` compares `version_query_quarantined_executable` with the passed
`executable`; a path change automatically re-arms the breaker, exactly as
today.

## 4. Cross-boundary inputs (explicit injection, credential-style)

| Dependency | How it crosses |
|---|---|
| Query I/O | Constructor-injected `query: fn(&Path, Duration, String) -> VersionQueryResult`. Production wires `crate::version_query::query_installed_version`; tests inject fakes, so the controller becomes unit-testable **without spawning subprocesses** (net testability gain over today). |
| `options.version_query_timeout` | Parameter `timeout: Duration` (worker bound and the 2× overdue threshold). |
| `config.configured_executable` | Parameter `executable: Option<&Path>` — spawn target, breaker comparison, and the still-current check when a result arrives. |
| `installed_version.is_some()` | Parameter `has_version: bool` (see §3). |
| Event trace + generation id | `query_event` / `query_generation` stay direct calls inside the controller; in test builds they are marker-gated no-ops (`test_events::emit`), so no injection is needed. |

## 5. Timestamp fidelity (behavioral detail that must survive)

`last_version_attempt` is stamped from two different clocks today, and the
refactor preserves each per-path:

- spawn failure, discarded result, `Disconnected` worker, `Quarantined`:
  the poll tick's `now` parameter;
- `Available` / `Unavailable` (via `apply_installed_version`): a fresh
  `Instant::now()`.

The controller records the stamp for every terminal path before emitting
an outcome (or returning `None`); the Supervisor-side `apply_installed_
version` drops its stamp line, which has moved into the controller. The
relative order of stamp vs. version application is unchanged (stamp
first, then apply). For `Quarantined` the original cleared
`installed_version` before stamping; the clearing moves to the delegator
so the stamp now precedes it — the two fields are never observed between
the two assignments, so this is unobservable.

## 6. The narrow interface

`VersionQueryCoordinator` exposes exactly this `pub(crate)` surface:

| Method | Purpose |
|---|---|
| `new(query)` | construction with the injected query function |
| `in_flight() -> bool` | `next_deadline` scheduling + shutdown drain loop |
| `due(now, executable, has_version) -> bool` | retry/interval/breaker decision |
| `poll_version_query(now, executable, timeout, has_version) -> Option<VersionQueryOutcome>` | single-flight worker convergence; returns the outcome to apply |

Everything else is private: the worker spawn/dispatch, the result arms
(still-current check, `Available`/`Unavailable`/`Quarantined` mapping,
`Disconnected` join, `Empty` overdue latch). `finish_version_query_for_
shutdown` remains a `pub` Supervisor method (external callers:
`rust/src/bin/agent.rs` and `Drop`) as a thin delegator with an unchanged
signature; `version_query_due` keeps a thin same-name delegator so
`next_deadline` and `tick` bodies stay byte-identical; `poll_version_
query` keeps its same-name `pub(super)` delegator that computes the four
boundary inputs and applies the outcome.

## 7. Test seams

- `#[cfg(test)]` helpers on the controller (seed/read the in-flight
  record, overdue latch, last attempt, quarantine) mirror the credential
  helpers; existing tests never touch the version fields, so no existing
  test changes at all.
- The injected `query` fn pointer makes the idle→spawn path unit-testable
  after the migration; pre-refactor characterization tests drive only the
  in-flight branch (hand-built `VersionQueryInFlight` channels), which is
  fully deterministic in-process.

## 8. Coverage audit (Phase 0) and characterization gaps

Existing coverage before this refactor:

- Unit: 102 (87 original + 15 credential characterization). None touch
  the version fields.
- Integration: 58, including four that directly cover the version
  mechanism through the real agent binary:
  `version_queries_are_single_flight_and_every_hung_child_is_reaped`,
  `observed_group_escape_opens_the_automatic_version_query_circuit_
  breaker`, `orderly_supervisor_shutdown_drains_an_inflight_version_
  query`, `installed_version_query_returns_none_when_the_direct_child_
  closes_stdout_and_keeps_running`.

Gaps the new characterization tests close at the unit level:

1. Retry-interval selection (has-version vs. no-version) and the
   quarantine breaker blocking/auto-rearming `due()`.
2. The three result arms at unit level: `Available` backfill,
   `Unavailable` no-op, `Quarantined` clearing `installed_version` and
   opening the breaker.
3. Still-current discard when the executable changed mid-query (easy to
   lose in a migration).
4. `Disconnected` worker handling and the one-shot overdue latch.
5. Single-flight: no new spawn while a read is in flight.
6. Shutdown drain convergence.

Remaining gaps recorded (not closed here): the real spawn-failure branch
(thread exhaustion) has no unit trigger; the process-record
running-version backfill needs a managed process and stays
integration-covered.

## 9. Recorded observations (no fixes in this refactor)

1. **`Quarantined` clears `installed_version` but leaves
   `running_version` and any process record untouched** — deliberate
   (the informational label must not lie while a live process reports a
   version), but worth stating as a locked behavior.
2. **The stamp uses two clocks** (tick `now` vs. fresh
   `Instant::now()`), see §5 — preserved per-path; unifying them would
   be a behavior change and is out of scope.
3. **`finish_version_query_for_shutdown` polls with `Instant::now()`
   each loop** and sleeps `VERSION_IN_FLIGHT_RECHECK` while the worker
   hangs — bounded by the worker's own timeout; preserved.

## 10. Migration order (Mikado, every commit green)

1. Boundary design document (this file) + characterization tests, all
   green on the unmodified `Supervisor`.
2. Introduce `VersionQueryCoordinator` as the state holder: the four
   fields move (temporarily `pub(super)`), `Supervisor::new` constructs
   it with the injected real query function, all sites route through
   `supervisor.version_queries`.
3. Migrate the methods into `impl VersionQueryCoordinator` with the
   §4 inputs and the `VersionQueryOutcome` return; add the thin
   Supervisor delegators; `tick`/`next_deadline`/`Drop` keep calling the
   same names.
4. Privatize the four fields and let the compiler enumerate every
   remaining direct access; replace them with `in_flight()`/`due()` and
   the test helpers. Compilation success proves the boundary.
5. DoD: negative-compile test (temporary illegal write in `lifecycle.rs`
   must fail to build; then revert), grep evidence, field count 38 → 34,
   `lib.rs`/`Cargo.toml`/`version_query.rs` public exports byte-identical,
   full green suite.
