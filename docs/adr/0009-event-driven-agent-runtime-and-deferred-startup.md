# ADR 0009: Event-driven OpenCodeServerAgent runtime and deferred startup work

## Status

Accepted on 2026-07-30.

## Context

Boot forensics on Build 7 (Unified Logging, `log show --last boot`) showed
that after a real macOS restart the menu bar icon stayed gray for about
33 seconds even though OpenCodeServer itself launched within two seconds of
login:

- launchd held the LaunchAgent spawn while the `gui/501` domain was in
  on-demand-only mode and only exec'ed OpenCodeServerAgent about 19 seconds
  later ("pending spawn" → "because speculative"). That part is platform
  scheduling and is out of product control.
- After exec, OpenCodeServerAgent spent about 13 more seconds before its IPC
  socket existed. `Supervisor::new` ran `opencode --version` as a subprocess
  (a 132 MB Bun single-executable that takes ten-plus seconds to exec cold
  after a reboot, versus 0.23 s warm), and `start_now` ran the same query a
  second time. `IpcServer::bind` only happened after all of it, so
  OpenCodeServer could not even display "Starting".
- The agent main loop polled: `accept_pending` + `tick` + a 100 ms
  `recv_timeout`, with child-exit reaping on every pass and a configuration
  re-read every 2 seconds.

The installed OpenCode version is informational: the canonical process
identity is the ADR 0005 configuration fingerprint plus strict process
identity, neither of which contains the version string.

## Decision

1. **IPC before initialization.** `IpcServer::bind` runs before
   `Supervisor::new` completes its heavy work, so OpenCodeServer can connect
   and observe "Starting" immediately after exec.
2. **Deferred version query.** `opencode --version` never runs on the spawn
   or startup path. A bounded worker thread delivers the result
   asynchronously (5 s retry until success, 60 s refresh afterwards, 30 s
   in-flight bound — now covering the worker and its subprocess, see
   Addendum 3); the process record is updated when it arrives. Process
   identity semantics are unchanged.
3. **kqueue event loop** (`kqueue(2)`/`kevent(2)`, all `unsafe` confined to
   `rust/src/platform.rs`): `EVFILT_READ` on the IPC listener and subscriber
   sockets, `EVFILT_PROC`/`NOTE_EXIT` on the supervised PID (one-shot),
   `EVFILT_VNODE` on `config.plist` (re-armed after every atomic replace),
   and `EVFILT_USER` so the `sigwait` thread can wake the loop. Timer work
   derives from `Supervisor::next_deadline`; a single wait is capped at 30 s.
4. **Fallbacks kept.** The tick still reaps with `waitpid` (NOTE_EXIT only
   makes exit detection immediate), the configuration is still re-validated
   on a slow 60 s timer, and health checks originally remained synchronous
   with the existing 2 s timeout (superseded by Addendum 4: they now run on
   a single-flight worker). No behavior depends on a single mechanism.
5. **Adaptive health interval.** 1 s while `Starting` or `Unhealthy`, 3 s
   otherwise (previously a fixed 3 s), so state transitions are reported
   sooner without raising steady-state cost.

## Consequences

- After exec, the IPC socket answers in milliseconds instead of ~13 s; the
  remaining gray period after a reboot is launchd scheduling, which the
  product now surfaces as "Starting" (yellow) instead of "Temporarily
  Unavailable" (gray) as soon as the agent runs.
- The 2 s configuration poll is gone; settings apply on the next vnode event
  (well under a second) with a 60 s safety net.
- No new crates; kqueue comes from `libc`. `opencode --version` hanging can
  no longer stall supervision — at worst the version label stays "unknown"
  until the bounded retry succeeds.
- Reattachment (`try_reattach`) keeps its synchronous strict identity and
  health verification; it is rare (agent restart with a surviving OpenCode)
  and the socket is already bound, so clients queue in the listen backlog.

## Apple references

- kqueue(2): https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/kqueue.2.html
- kevent(2): https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/kevent.2.html

## Addendum: Darwin accept race found during Build 8 validation

Installed Build 8 validation exposed a latent defect that the event-driven
accept path made reachable: on Darwin, an accepted socket inherits
`O_NONBLOCK` from a nonblocking listener (BSD semantics, unlike Linux
`accept()`). The handshake read in `handle_connection` could therefore fail
immediately with `EAGAIN` instead of waiting up to the 2 s timeout for the
client's request. The previous 100 ms polling accept loop practically never
lost this race, because clients had always written before the next poll; the
event-driven loop accepts within microseconds of `connect()`, so the agent
could read before the client wrote. Symptoms in Build 8: OpenCodeServer
subscription handshakes failed with `EPIPE`, the agent logged `EAGAIN` IPC
errors, and the bundle-version update transaction could not observe
authenticated IPC within its bounded attempts.

Fix: `handle_connection` forces blocking mode on every accepted socket
before applying timeouts; subscriptions switch back to nonblocking after the
handshake. Locked by the
`accepted_socket_requires_explicit_blocking_reset_on_darwin` unit test and
the `slow_writer_request_is_still_answered_after_fast_accept` integration
test (a 300 ms delayed writer must still be answered).

## Addendum 2: One-shot deadline busy-spin found during Build 9 validation

Build 9's agent began burning ~83% CPU roughly five minutes after every
OpenCodeServerAgent start. Sampling localized the hot path to
`Supervisor::tick` → `poll_process` being called back to back. Root cause:
the old 100 ms loop executed `tick` unconditionally, so one-shot timers such
as the 300 s stable-run reset (`STABLE_RUN_INTERVAL`) simply re-fired
harmlessly. The new `next_deadline` computation returned that already-past
instant, producing a zero-length kqueue wait forever. The defect was
invisible in tests because no test ran a supervisor past the 300 s mark.

Fix: `next_deadline` only accepts instants strictly in the future
(`future_deadline`); every timer's `tick`-side action still fires on the
next regular wakeup (health/config cadence), so no timer starves. Locked by
the `past_deadlines_never_schedule_a_zero_wait` unit test and a >300 s
soak check of the installed agent.

## Addendum 3: installed-version query boundary

The original receiver-only timeout was insufficient because it could abandon
a worker and its child. The current query remains single-flight on a named
standard-library worker, uses `EVFILT_PROC`/`NOTE_EXIT` for non-reaping exit
observation, bounds output and observation time, performs any required
authorized group signal before the final direct-child reap, and reports
failure as unavailable. A clean version result is reaped without signaling its
completed process group.

The implementation is isolated in `rust/src/version_query.rs`. An observed
group escape or identity-inspection anomaly fails closed and opens an automatic
retry circuit breaker for that executable. It has no guardian subprocess,
pending-cleanup state machine, or descendant ledger. Deliberate post-snapshot
escape is an accepted v1 trust-boundary non-goal, as recorded normatively in
ADR 0015.

## Addendum 4: health checks move off the supervision event loop (2026-08-17)

The original exemption — "health checks remain synchronous with the existing
2 s timeout" — only bounded the connect/read budget. `check_endpoint`
resolved the hostname before starting its timer, so a slow system resolver
could block the supervision loop for far longer than 2 s, and even the
bounded 2 s connect plus 2 s read repeatedly delayed Stop/ForceStop
handling, signal drainage, child-exit reaping, and status pushes during the
1 s `Starting` cadence.

Decision:

- The authenticated request runs on a single-flight named worker
  (`rust/src/supervisor/health_worker.rs`). DNS resolution, connect, write,
  and read never execute on the supervision event loop.
- The supervisor polls the worker without waiting and applies a completed
  result only when its task key still matches the managed process: the
  complete recorded process identity, the active configuration fingerprint,
  and a monotonic generation observed at mutation boundaries, so an
  A → B → A process/configuration cycle cannot resurrect a stale result.
- All state transitions, notifications, and the stale-attachment upgrade
  (`config_pending` convergence) remain on `Supervisor`; the worker owns
  only I/O and currentness.
- A slow check stretches the effective cadence (single-flight skips new
  dispatches until completion) and is logged once it exceeds the observation
  bound; supervision responsiveness is never traded for health freshness.

Locked by `health_worker_tests.rs` (single-flight and A → B → A staleness)
and `lifecycle_deadlines_are_scheduled_when_health_is_unavailable`.
