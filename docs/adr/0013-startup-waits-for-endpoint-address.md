# ADR 0013: Wait for the endpoint address during startup instead of failing fast

## Status

Accepted (build 15)

## Context

Build 14 (`ProcessType = Interactive`, ADR 0012) moved the agent's first
start ahead of `configd`: the agent attempted to spawn OpenCode at
14:28:11.07, but the static address `10.0.0.254` was only assigned to en0 at
14:28:14.724 (`MANUAL en0: setting 10.0.0.254`). The preflight bind failed
with `EADDRNOTAVAIL` (os error 49) and startup declared `failed` (red icon),
even though the condition resolves by itself seconds later. launchd offers no
network-readiness dependency (`NetworkState` is dead, "no longer
implemented"), and older builds never hit the window because the SMAppService
trampoline delayed the agent past `configd`.

`EADDRNOTAVAIL` (address not yet assigned locally) is transient; `AddrInUse`
(a real foreign listener) is not. They must not share one failure path.

## Decision

On startup preflight failure with `io::ErrorKind::AddrNotAvailable`, the
supervisor schedules a bounded wait instead of failing: retry every 250ms
(there is no launchd/network event source worth depending on at this scope),
with a total budget of 60s (`OPENCODESERVER_NETWORK_WAIT_BUDGET_MS`
overridable in tests). The retry preserves the original `StartTrigger`
(Cold/Recovery/AfterStop) so Recovery semantics survive the wait. When the
budget expires, the original error is surfaced unchanged. `AddrInUse`
handling is untouched: cold start still reports the port conflict without
terminating anything; the AfterStop release window keeps its own 10s budget
(ADR 0011).

The wait reuses `ServerState::WaitingToRestart` (yellow) rather than adding a
protocol state; the window is normally seconds.

Alternatives considered and rejected: SystemConfiguration event subscription
(event-driven wake on address appearance; zero polling, but a new crate,
callback lifecycle, and test seams for a once-per-boot, seconds-long window —
see the discussion recorded in ADR 0012's context; may be revisited if the
deployment ever moves to DHCP/roaming), and binding a wildcard address
(changes the service's exposure posture, which is a deliberate project
choice).

## Consequences

- Boot and cable-replug-later manual restarts no longer turn red while
  `configd` is still applying the address.
- A genuinely wrong configured address (never local) now shows yellow for up
  to 60s before red, with the original `EADDRNOTAVAIL` error preserved.
- Covered by integration tests: recovery once the address appears, failure
  with the original error after budget expiry, and cold-start port conflict
  staying fatal without retry.
