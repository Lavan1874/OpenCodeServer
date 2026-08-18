# ADR 0003: Process supervision and Rust safety boundary

- Status: accepted

## Decision

OpenCodeServerAgent is a synchronous Rust state machine using the standard library plus
small serialization, platform, HTTP-authentication, random-generation, and
RustCrypto HMAC/SHA-256 dependencies. It has no Tokio or general async runtime.

OpenCode is executed directly from the configured absolute path. Rust’s safe
`CommandExt.process_group(0)` places it in a new process group during spawn.
Because OpenCodeServerAgent blocks control signals for synchronous `sigwait`, its one
`pre_exec` hook restores the child signal mask with async-signal-safe
`sigprocmask` before `exec`. This prevents OpenCode from inheriting blocked
`SIGTERM`, `SIGINT`, or `SIGHUP`.
OpenCodeServerAgent sends signals only after revalidating:

- exact PID;
- exact process start seconds and microseconds;
- process group equals the direct PID;
- exact canonical executable;
- effective user.

Crash reattachment follows ADR 0005. It first validates process identity, then
requires a versioned semantic configuration fingerprint, an authenticated
healthy response from `/global/health`, and a second process-identity check.
File device/inode metadata is used only for same-read race protection. Runtime
state requires the current semantic fingerprint schema; no historical state
migration exists. An unconfirmed live process is never signaled and prevents a
second OpenCode from being launched.

Graceful stop sends `SIGTERM` to the complete group and waits 15 seconds. The
deadline never causes automatic `SIGKILL`. Force termination is available only
after an explicit stop/restart request and a second explicit OpenCodeServer
action or opencodeserverctl `--force`.

## Rust `unsafe` audit

All project-authored Rust `unsafe` is confined to
`rust/src/platform.rs`. Each block has a local safety argument. It exists only
to bridge public macOS/POSIX APIs that have no safe standard-library wrapper:

- `getpeereid`;
- `pthread_sigmask`, `sigprocmask`, `sigemptyset`, `sigaddset`, and `sigwait`;
- the narrowly scoped `CommandExt.pre_exec` hook needed to restore the child
  signal mask before `exec`;
- `proc_pidinfo` and `proc_pidpath`;
- `kill` for a validated process group;
- the per-process wrappers `signal_process` (refuses PID ≤ 1) and
  `process_exists` (`kill(pid, 0)`; `EPERM` counts as existing);
- the process-group helpers `own_process_group`, `parent_process_id`,
  `parent_process_group`, and `join_process_group` used by post-spawn
  identity confirmation and the test fixture;
- `ignore_signal` (`sigaction` + `SIG_IGN`), used only by the test fixture —
  OpenCodeServerAgent itself still consumes control signals synchronously via
  `sigwait`;
- `fstat` for config file identity;
- `geteuid`;
- the fixed-signature Unified Logging C bridge.

No process-management, configuration, protocol, health, or state-machine module
contains `unsafe`. The C logging bridge uses a constant format string, so log
messages cannot become format strings.

## Consequences

- OpenCodeServerAgent may conservatively leave an unverifiable orphan
  untouched.
- An attached process is not a direct child and has no wait status; disappearance
  is recorded honestly rather than invented as an exit code.
- A force-stop request cannot target a reused PID because the full identity is
  rechecked immediately before signaling.

## Addendum: the post-spawn ownership window (Build 17)

Between a successful `spawn` and completed identity registration there is a
defined ownership window: before any error may be returned,
OpenCodeServerAgent must either prove the child exited and was reaped, or
keep it as a supervised state.

`ManagedProcess::spawn` returns `SpawnError`. When post-spawn identity
confirmation (process snapshot, executable-path match, dedicated-process-group
check) fails, the child receives one graceful `SIGTERM` to the constructed
process group only (`child.id()`). A rejected snapshot's observed process
group is never used as the signal target: it may point at an unrelated live
process group, and the Child handle already prevents PID reuse of the direct
child. The constructed group contains only this child and descendants that
stayed in it, so the signal cannot reach an unrelated process. The bounded
2-second grace (`UNREGISTERED_CHILD_STOP_GRACE`) runs inside the supervisor
event loop. If the child exits within the grace window it is reaped
(`try_wait` is authoritative) and the error carries no process. If it
survives, it is neither `SIGKILL`ed nor dropped: the error carries the
still-owned child as a survivor.

The survivor's identity record uses the constructed process-group ID
(`child.id()`) and the kernel start time from the last observed snapshot when
available. `identity_probe` distinguishes `ExecutableMismatch` (kernel
identity matches but the executable path differs) from `Mismatch` (kernel
identity differs). `identity_matches` returns true for `ExecutableMismatch`,
so Stop and Force Stop work on a survivor whose executable was never
confirmed. A survivor whose snapshot never succeeded gets zeroed kernel
start fields and the `identity_unconfirmed` marker (see the Build 19
ownership-tier addendum below); Stop and Force Stop still work while the
`Child` handle is held (the constructed group is provably ours), the
persisted record authorizes nothing, and `try_wait` still reaps the child on
exit.

On OpenCodeServerAgent restart, `try_reattach` handles `ExecutableMismatch`
by attaching the process as unconfirmed (`attach_unconfirmed_process`):
`server_state` is `Failed`, `unverified_process_record` is true, and the
process is attached via `ManagedProcess::attach` so Stop and Force Stop
remain available. No second OpenCode is started while the survivor lives.
A survivor whose kernel identity no longer matches (PID reused or process
exited) is discarded as stale without signaling.

Independently, `ManagedProcess::signal` refuses to signal
OpenCodeServerAgent's own process group even when the record identity matches
(defense in depth for a child that abandoned its dedicated group). The port
preflight is not a substitute for process ownership management.

## Addendum: ownership tiers and the unconfirmed-survivor lifecycle (Build 19)

The Build 18 rework left one ownership gap: a survivor whose identity
snapshot never succeeded persisted a record with zeroed kernel start fields.
Within the running OpenCodeServerAgent that record could not authorize Stop
or Force Stop, and after an OpenCodeServerAgent restart it was classified as
`Mismatched` and discarded as stale, leaving the live OpenCode unowned and
allowing a second instance to be started.

Signal authorization now uses three explicit tiers that must not substitute
for one another:

- **T1 — owned child (the `Child` handle is held).** While the handle is
  held and un-reaped, the PID — and therefore the process-group id
  constructed at spawn — cannot be recycled. A group signal to
  `child.id()` can only reach this child and descendants that stayed in the
  constructed group, so it is authorized even when the kernel identity was
  never confirmed. The only refusals are the own-process-group guard and a
  snapshot that *proves* the child abandoned the constructed group (the
  signal could not reach it, and claiming otherwise would be dishonest).
- **T2 — attached process with a revalidated identity.** After an
  OpenCodeServerAgent restart the persisted record is the only basis:
  the full identity (PID, group, start time, executable, user) must
  revalidate immediately before any signal, and the own-group guard stays.
- **T3 — unconfirmed record (never observed kernel identity).** Authorizes
  nothing. It is marked `identity_unconfirmed` in `state.json` (serde
  default false, so old state files stay readable). On restart the
  supervisor inspects the PID: a provably gone PID (`ESRCH`) makes the
  record stale and a fresh start safe; any live process at that PID —
  whether the original survivor or a reused PID — keeps the record
  unverified: no signal, no takeover, no second OpenCode, and an error
  message that states the actual limitation. Stop on an unverified record
  refuses honestly instead of claiming `Stopped`.

While an unconfirmed survivor lives in its original OpenCodeServerAgent,
the supervisor retries identity confirmation on every poll
(`confirm_unconfirmed_identity`): if the snapshot succeeds and still shows
the child leading its constructed group with the configured executable, the
record is upgraded with the real kernel identity, cleared of the marker,
and persisted — it then survives a restart through the normal T2 path. A
child that escaped its group never upgrades (its record stays diagnostic
only), matching the escape semantics above.

State model for a spawned child: `spawn` → identity confirmed (T2-ready
record) → normal supervision; or `spawn` → confirmation failed → one
graceful SIGTERM to the constructed group only → reaped (error carries no
process) or survived → T1 survivor (stoppable through the handle) →
upgrade on snapshot success or reaped on exit; if OpenCodeServerAgent ends
first, the persisted unconfirmed record enters the T3 restart path above.

Tests (process-supervision integration suite, all passing under default
parallelism):
- `spawn_failure_keeps_an_unidentifiable_sigterm_ignoring_child_supervised_and_stoppable`:
  a persistently un-snapshotable, SIGTERM-ignoring child survives the
  grace window with a zeroed record; Stop and Force Stop succeed through
  the handle; the child is reaped and nothing is orphaned.
- `supervisor_restart_keeps_an_unidentifiable_survivor_unverified_without_a_second_opencode`:
  restart with a live PID keeps the record unverified (Failed, no attach,
  honest error), Stop refuses honestly, and no second OpenCode appears.
- `unconfirmed_record_is_discarded_when_the_pid_is_gone`: a provably gone
  PID is discarded and a fresh OpenCode starts.
- `unconfirmed_record_blocks_a_second_start_while_an_unrelated_process_holds_the_pid`:
  a reused/unrelated PID is never signaled and never taken over.
- `unregistered_child_grace_signal_never_reaches_a_sentinel_in_a_joined_group`:
  the fixture really joins a sentinel's process group; the grace SIGTERM
  goes only to the constructed group, the sentinel receives nothing, and
  the joined child survives as a survivor.

Every scenario asserts the final process, process-group, and persisted-record
state, not only the returned error.

## Addendum: single `unsafe` boundary restored (Build 17)

At the Build 17 checkpoint the single-`unsafe`-boundary rule above again
held for the whole tree, including tests, with the wrappers listed in the
main audit. The Build 18 post-spawn ownership rework (commit 70fbc0d) broke
it: a test-only `ignore_sigterm` parameter on
`ManagedProcess::spawn_with_snapshot` carried an inline
`unsafe { command.pre_exec(...) }` in `rust/src/process.rs`, moving
project-authored `unsafe` outside `rust/src/platform.rs` and putting a
test-only behavior switch inside a production API. The Build 19 rework
phase 2 (commit b261dd7) removed the parameter and the inline `unsafe`:
the test fixture now expresses SIGTERM-ignoring behavior through its own
marker files and an observable `ignore-sigterm.ready` synchronization
point, so the supervisor API never knows it is being tested.
`rust/src/platform.rs` is again the only project `unsafe` boundary
(verified by grep: the only `unsafe` token outside it is the
`deny(unsafe_op_in_unsafe_fn)` lint attribute in `rust/src/lib.rs`).

## Addendum: second `unsafe` boundary for the Keychain (ADR 0016)

ADR 0016 introduced `rust/src/keychain.rs`, which reaches the Security
framework through the `security-framework` and `core-foundation` crates and
carries its own small `unsafe` blocks (`CFString::wrap_under_get_rule`,
`SecItemCopyMatching`, `CFRelease`). The "confined to
`rust/src/platform.rs`" statements above are therefore historical. The
current rule is two isolated, documented boundaries: `platform.rs` for
POSIX/process integration and `keychain.rs` for Security-framework FFI. The
standing requirement that all `unsafe` stays isolated, documented, reviewed,
and tested applies to both files.

The test fixture binary (`rust/src/bin/test_child.rs`) contains no raw libc
calls or environment knobs; its behavior knobs are per-test-instance
marker files placed next to each test's private copy of the fixture binary
(`hold-endpoint`, `hang-on-version` with its `hang-on-version.pids` PID log,
`join-parent-process-group`, and `ignore-sigterm`), so parallel integration
tests never share mutable global state. Integration tests inject timing
budgets through `Supervisor::with_options(SupervisorOptions { .. })` instead
of process-global environment variables; `--test-threads=1` is not used
anywhere to mask global state.

## Addendum: group-escape survivor and unverified convergence (Build 20)

The Build 19 ownership-tier model had two gaps found during Build 20
cross-review:

1. **Group-escape survivor**: `survivor_record` set
   `identity_unconfirmed = false` whenever `last_snapshot` was `Some`,
   even when that snapshot proved the child had escaped its constructed
   process group (`process_group_id != pid`). The persisted record had
   real kernel start times and a constructed-group PGID that would not
   match the live process on restart, causing `identity_probe` to return
   `Mismatch` and the record to be discarded as stale.

   Fix: `survivor_record` now sets `identity_unconfirmed = true` when
   either no snapshot exists OR the snapshot proves group escape. Zeroed
   kernel start fields ensure the record authorizes nothing. On restart,
   `try_reattach` for `identity_unconfirmed` records only checks
   `Missing` (ESRCH), so a live PID stays unverified rather than being
   discarded.

2. **Unverified PID self-convergence**: once an unverified record was
   marked, `tick` never re-checked whether the PID had disappeared. The
   supervisor permanently blocked Start/Stop until restarted or repaired.

   Fix: `tick` now calls `check_unverified_process` on a 3-second
   interval. When `inspect_record_identity` returns `Missing`, the
   record is discarded and the supervisor converges according to
   `desired_state` (Running -> start fresh OpenCode; Stopped -> enter
   Stopped state). Any live, mismatch, or inspection error keeps the
   record unverified.

Test coverage (all passing under default parallelism, 10 consecutive
rounds): real `spawn_with_snapshot` -> fixture joins sentinel's group ->
survivor record is `identity_unconfirmed` with zeroed start fields ->
persist -> new Supervisor -> record NOT discarded, sentinel NOT
signaled, no second OpenCode -> PID disappears -> supervisor
self-converges and starts a fresh OpenCode. Separately: unverified
record with live decoy PID -> decoy killed -> tick detects Missing ->
supervisor starts fresh OpenCode (desired_state=Running) or enters
Stopped (desired_state=Stopped).

The final ownership boundary and current regression requirements are recorded
in ADR 0015. In particular, removal of a live unverified record must be durably
saved before replacement startup, and test-harness cleanup is never product
cleanup evidence.
