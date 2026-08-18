# ADR 0015: Resource ownership, durable uncertainty, and the v1 trust boundary

## Status

Accepted, 2026-08-02.

This revision replaces the Build 21–24 phase diary formerly stored in this
file. Git history and `docs/ACCEPTANCE.md` retain the historical build evidence;
this ADR is the concise, normative design.

## Context

OpenCodeServerAgent has two different process responsibilities:

1. supervise the long-running OpenCode selected by the user; and
2. occasionally run that executable with `--version` to populate an
   informational “installed version” label.

The first responsibility is a core product function. The second is not. During
Builds 20–24, the version query accumulated a native guardian mode, group
membership enumeration, pending-cleanup stages, multi-phase closeout, and a
large event-test state machine. That machinery still could not prove cleanup
of a descendant that deliberately calls `setsid`, reparents, and escapes after
the final observation. Complexity therefore grew without closing the stated
boundary.

The product is a small personal macOS utility for a trusted OpenCode
installation. It is not a hostile-code sandbox or a general process
containment system.

## Platform facts

- `EVFILT_PROC` with `NOTE_EXIT` observes a PID exit without consuming the
  parent's wait status. `waitpid`/`Child::wait` is still required to reap the
  direct child.
- A process-group signal reaches only processes that remain in that group.
  Process groups are not immutable containers; a process can deliberately
  leave one.
- Keeping the direct `Child` unreaped prevents reuse of that child's PID while
  the product performs its authorized closeout. It does not create ownership
  of reparented descendants.
- Dispatch process sources and parent-death handlers require cooperation by
  code running in the child. OpenCodeServerAgent cannot inject that behavior
  into the configured external executable while preserving direct execution.
- EndpointSecurity can provide stronger observation, but requires a different
  entitlement, user authorization, and operating model. It is not a
  descendant-termination lease and is outside this product's v1 scope.
- The existing process-identity layer uses narrowly scoped process snapshots
  declared by the target macOS SDK. Those snapshots are identity evidence, not
  containment. Supervision and the version-query residual check enumerate
  authorized group membership through the SDK-header-declared
  `proc_listpgrppids` with a bounded, fail-closed buffer (2026-08-17
  amendment below), and product code does not use private process-control
  calls such as `proc_signal_with_audittoken`.

Primary references:

- Apple [`kqueue(2)`](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/kqueue.2.html)
- Apple [`wait(2)`](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/wait.2.html)
- Apple [`kill(2)`](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/kill.2.html)
- Apple [`setpgid(2)`](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/setpgid.2.html)
- Apple [Dispatch Sources](https://developer.apple.com/library/archive/documentation/General/Conceptual/ConcurrencyProgrammingGuide/GCDWorkQueues/GCDWorkQueues.html)
- Apple [`es_process_t`](https://developer.apple.com/documentation/endpointsecurity/es_process_t)

## Decision

### 1. Trust boundary

The configured OpenCode path must still resolve to an executable native Mach-O
and is executed directly. The user is responsible for selecting a trusted
OpenCode installation. Cooperative OpenCode descendants are managed through a
dedicated process group.

A deliberately hostile executable that escapes its process group after the
last trustworthy observation is outside the v1 trust boundary. OpenCodeServer
does not promise containment, discovery, or termination of that descendant.
Supporting untrusted arbitrary Mach-O execution would require a separately
approved system-isolation design.

This accepted residual risk does not weaken fail-closed behavior:

- an observed group escape or identity mismatch never authorizes a signal to
  the new group or an inferred descendant PID;
- a stale or mismatched persisted PID is discarded without signaling;
- an inspection error or live unverified record blocks takeover and a second
  OpenCode until the record safely converges to `Missing`;
- no timeout, health result, pipe EOF, or version string creates process
  ownership.

### 2. Long-running OpenCode supervision

OpenCodeServerAgent remains the only runtime authority. It:

- launches OpenCode directly in a dedicated process group;
- persists the canonical configuration fingerprint and complete process
  identity;
- classifies process identity before comparing configuration during reattach;
- signals only a currently authorized direct child/process group;
- keeps the direct `Child` unreaped (`waitid(WNOWAIT)`) after the leader
  exits and converges the still-authorized group before recovery: one
  cooperative `SIGTERM`, then a graceful window — never an automatic
  `SIGKILL` — with no automatic replacement OpenCode until the group is
  observed empty and the record clear is durable (2026-08-17 amendment);
- treats a reattached record whose leader is missing as observation-only:
  the recorded group may be inspected read-only, but no signal authority
  survives the restart that lost the `Child` anchor (2026-08-17 amendment);
- keeps uncertain live records fail-closed and durably persists their removal
  before starting a replacement; and
- never automatically sends `SIGKILL` after the graceful-stop deadline.

The existing two-process restart and unverified-state integration tests remain
normative. The accepted trust boundary does not permit OpenCodeServer, tests,
or cleanup code to signal an unverified process.

### 3. Informational installed-version query

The query is isolated in `rust/src/version_query.rs`; it must not dominate the
main supervisor state machine.

For each query OpenCodeServerAgent:

1. permits only one worker at a time;
2. executes the configured native binary directly with `--version` in a
   dedicated process group;
3. bounds stdout, validates printable output, and applies one observation
   deadline;
4. uses `NOTE_EXIT` so the direct child remains unreaped while any authorized
   cleanup signal is issued;
5. closes the still-authorized constructed group before the final direct-child
   reap for incomplete, invalid, or timed-out queries, and for a clean
   completion whose group still has observable authorized members (the clean
   result is then quarantined rather than trusted — 2026-08-17 amendment); a
   clean completion whose group is observed empty is reaped without a
   speculative signal; and
6. reports failure as “installed version unavailable” without affecting
   OpenCode health, supervision, or IPC.

If the direct query leader is observed leaving its constructed group, or live
identity inspection becomes inconclusive, cleanup targets only the still-owned
direct child. The result is quarantined, and OpenCodeServerAgent suppresses
automatic version-query retries for that executable until its configured path
changes or OpenCodeServerAgent restarts. This circuit breaker prevents a
five-second respawn loop around unsupported behavior.

The query deliberately has no guardian subprocess, descendant ledger, private
group enumeration, or supervisor-owned pending-cleanup state machine. An
orderly OpenCodeServerAgent shutdown drains the one in-flight worker. Abrupt
OpenCodeServerAgent death during a deliberately non-cooperative query, and a
kernel-uninterruptible query process, remain residual operating-system/trust
boundary risks rather than reasons to add a second supervisor for an
informational label.

### 4. Test causality

Tests use per-fixture marker files and event traces, never process-global
environment knobs. Timeouts only bound a broken test; branch assertions use
observable events or files.

Required coverage includes:

- normal, invalid, overflowing, hung, early-EOF, and descendant-holding-stdout
  version queries;
- observed group escape without signaling the foreign group;
- automatic-query circuit breaking after an identity anomaly;
- single-flight behavior and orderly OpenCodeServerAgent shutdown;
- stale, mismatched, missing, and uninspectable persisted records;
- two independent OpenCodeServerAgent processes reloading the same unverified
  state; and
- durable convergence before any replacement OpenCode starts.

Test harness cleanup is diagnostic containment only. It must never be cited as
proof that product code cleaned a process.

## Consequences

Positive:

- `supervisor.rs` returns to the size and responsibility of a conventional
  service supervisor.
- Query failures cannot become an automatic process-spawn storm.
- Signals remain fail-closed and PID-reuse-safe for the authority the product
  actually possesses.
- The product keeps direct execution, minimal entitlements, and the current
  AppKit/SMAppService architecture.

Accepted limitations:

- v1 does not contain a deliberately escaping hostile executable or all of its
  descendants.
- Abrupt OpenCodeServerAgent death at the same time as a hung, non-cooperative
  version query can leave a residual process for system/user diagnosis.
- A kernel-uninterruptible direct child can delay an orderly shutdown.
- “Installed version unavailable” is acceptable; it is never promoted into a
  server-health failure.

## Rejected alternatives

- EndpointSecurity or additional privileged entitlements for v1.
- A wrapper around the configured OpenCode executable.
- A guardian/guardian-of-guardian process hierarchy for `--version`.
- Continuing to add post-snapshot descendant ledgers or private process
  control to claim hostile containment.
- Signaling a PID/PGID after its identity authority has been lost.

If future requirements include running untrusted arbitrary Mach-O programs,
that work must begin with a new product decision and isolation ADR. It is not
an incremental extension of this menu bar utility.

## Amendment — 2026-08-17: authorized group convergence and runtime-state durability boundaries

Three supervision-side changes closing the 2026-08-17 high-priority review
findings; none of them moves the v1 trust boundary.

**Bounded group enumeration returns, fail-closed.** The managed-supervision
path and the version-query residual check enumerate process-group membership
through `proc_listpgrppids` (declared in the public SDK header `libproc.h`,
linked via `libc`), with a small growing buffer capped at 4096 entries; a
full-or-ambiguous result is an inspection failure, never a signal
authorization. Every observed member must still match the process-group id
and effective uid, and any untrusted member fails the whole observation
closed. Private process-control calls remain out of bounds.

**Leader exit no longer discards the group.** When the direct OpenCode leader
exits while its authorized group still has members, the supervisor keeps the
unreaped `Child` as the non-reusable PID/group anchor, sends the group one
cooperative `SIGTERM`, and holds the exit transition in the graceful-stop
state until the group is observed empty. Automatic recovery starts only after
both the group has converged and the process-record clear is durable; an
expired graceful window surfaces `StopTimedOut` exactly like an explicit
stop, and force termination still requires the explicit user action. A
reattached record whose leader is missing is observation-only: the recorded
group may be watched, never signaled.

**Runtime-state persistence is a transaction boundary.** `RuntimeState::save`
classifies its outcome as Durable, Failed, or Uncertain (rename already
visible but the directory sync is unproven), and the write now syncs the
containing directory as well as the file. Explicit Start/Stop/Restart
persist the desired state first and answer with an IPC error when the write
is not durable; an Uncertain outcome keeps the new in-memory intent and
defers the action to a bounded retry. An unreadable state file never falls
back to a persisted default (`Default` desires `Running`, so silently
adopting it would restart a user-stopped OpenCode), never authorizes a
start, and never overwrites the record that may still describe a live
process. OpenCode launches run as a two-phase transaction: a durable
`launch_pending` marker precedes child creation, the process record and the
marker clear commit in one atomic write, and a replacement
OpenCodeServerAgent that finds an unresolved marker refuses to start a
second OpenCode until the transaction is explicitly resolved. The marker is
an optional field under the unchanged schema version 2: a state file written
before this change lacks it and reads as "no launch attempt", which is the
state such a file actually described.

Locked by `tests/runtime_state_durability.rs` (14 scenarios, including the
external-process launch-marker restart in
`crashed_opencodeserveragent_with_pending_launch_does_not_spawn_a_second_opencode`),
the group-residual cases in `tests/process_supervision.rs`
(`leader_exit_keeps_a_graceful_group_residual_until_explicit_force`,
`reattached_leader_exit_blocks_recovery_until_the_recorded_group_is_empty`,
`installed_version_query_discards_output_when_a_group_descendant_remains`,
`installed_version_query_cleans_a_descendant_when_the_first_leader_snapshot_is_gone`),
and `tests/runtime_state_agent_restart.rs`.
