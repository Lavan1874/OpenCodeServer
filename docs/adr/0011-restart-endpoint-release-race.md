# ADR 0011: Restart must ride out the predecessor's endpoint-release window

## Status

Accepted on 2026-07-31.

## Context

Build 10 (ADR 0009) made exit observation event-driven: kqueue `NOTE_EXIT`
plus an immediate `poll_process` reaps a stopped OpenCode within
milliseconds of `SIGTERM`, and `restart_after_stop` respawns in the same
instant. On Build 7 exit was observed on a polled tick, which accidentally
added a settle delay of up to roughly ten seconds — that delay, not any
explicit mechanism, was what made restarts reliable.

Real-machine testing of Build 10 showed every menu-bar Restart fail
instantly with `Port conflict: configured endpoint 10.0.0.254:4096 is
already in use; no process was terminated` (Unified Logging, agent PID
35384, 2026-07-31 02:54–02:56): SIGTERM, exit classification and the failed
respawn all landed within 6 ms. Two mechanisms combined, both empirically
verified on macOS 15:

1. **Endpoint-release window.** A SIGTERMed `opencode serve` spends a few
   hundred milliseconds in kernel teardown (ps state `E`, then zombie at
   ~135 ms in isolation) during which its listen socket is still bound. A
   bind probe — even one with SO_REUSEADDR — fails with EADDRINUSE while the
   owner is still tearing down; it succeeds ~300 ms after SIGTERM. The
   respawn fired 5 ms after SIGTERM, deep inside that window.
2. **Unreaped zombie leak.** During the same teardown window
   `proc_pidinfo` already fails with ESRCH, so the identity check in
   `ManagedProcess::poll_exit` reported `IdentityChanged` while `try_wait`
   still reported the child as running. The supervisor treated the process
   as exited and dropped the `Child` without ever reaping it: four defunct
   OpenCode zombies were found parented to the production agent (PIDs
   53799/53886/53917/53939 under agent 35384), one per failed restart.

Related measurements that shaped the fix (all empirical, this host):

- Rust std `TcpListener::bind` sets SO_REUSEADDR on macOS, so leftover
  TIME_WAIT sockets from a fully exited predecessor never block the spawn
  probe; a bind without SO_REUSEADDR would have been blocked for ~60 s.
  The TIME_WAIT theory was considered and excluded as the root cause.
- OpenCode's own bind also carries SO_REUSEADDR (it bound over a synthetic
  TIME_WAIT entry in 0.62 s), so once the predecessor is fully gone the
  spawn path is clean.

## Decision

1. **Bounded port-release retry after explicit stops.** A new
   `StartTrigger::AfterStop` spawn path (restart-after-stop, and Restart
   commands issued while no process is tracked) retries an `AddrInUse`
   spawn failure every 250 ms for up to 10 s, reporting
   `ServerState::WaitingToRestart` while it waits. Cold starts and
   automatic-recovery attempts keep the previous immediate-failure
   semantics; when the budget expires the original port-conflict error is
   surfaced unchanged. Nothing is ever signaled or terminated on a
   conflict, so the foreign-listener safety rule is untouched.
2. **`try_wait` is authoritative for owned children.** In the `Child`
   branch of `poll_exit`, an identity mismatch on a child that `try_wait`
   still reports as running is treated as mid-teardown, not as an exit;
   the handle is kept and NOTE_EXIT / the next poll reaps it. The
   `Attached` branch is unchanged — identity mismatch there remains a real
   exit signal, because an attached process is not ours to reap and PID
   reuse protection still applies.
3. **Probe contract locked by tests, not changed.** The bind probe keeps
   its semantics; unit tests pin that stale TIME_WAIT must not count as a
   conflict and that a live listener must.

## Consequences

- Menu-bar Restart completes in about 1–2 s on a warm system instead of
  failing instantly; the GUI shows "Waiting to restart" during the retry
  window rather than a red port-conflict error.
- A genuine foreign listener on a Restart path now takes up to 10 s to
  surface the same error as before (Start paths still fail immediately).
  This is accepted as the price of riding out real teardown variance.
- No new zombies: owned children are only released after `try_wait` reaps
  them. The four zombies from Build 10 are cleared when the agent process
  is replaced on the next update.
- Regression coverage: `restart_succeeds_while_the_previous_endpoint_is_still_releasing`
  drives the fixture with a test-only port holder (a group descendant that
  keeps a duplicated listen socket bound ~1.5 s past the leader's death,
  modeling the teardown window). Verified to fail with the production
  port-conflict error when the retry is disabled, and to pass with it.
  The fixture knob is scoped by port so parallel tests are unaffected.
