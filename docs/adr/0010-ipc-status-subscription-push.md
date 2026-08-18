# ADR 0010: Current-only IPC protocol with status subscription push

## Status

Accepted on 2026-07-30. Current protocol number amended by ADR 0017 on
2026-08-10 and by the action-capability schema on 2026-08-17.

## Context

OpenCodeServer polled `OpenCodeServerAgent` for status every 2 seconds, so
every menu bar state change — including the green/yellow health transitions
users actually watch — was up to two seconds stale, plus a health-interval
quantization on the agent side. A naive push-per-second is wasteful because
`Status` contains ever-changing counters (`uptime_seconds`,
`agent_uptime_seconds`, the graceful-stop countdown).

The product has no external users and all three executables ship from one
bundle. Supporting mixed protocol generations adds branches that cannot serve
a current installation and obscures failures during rapid iteration.

## Decision

1. **Protocol version 6 only.** OpenCodeServer, OpenCodeServerAgent, and
   opencodeserverctl send and accept exactly version 6 for both one-shot and
   `subscribe` requests. Responses and status payloads must also report
   version 6 and include the current required fields. Any other version is an
   error; there is no negotiation or compatibility range. Version 4 introduced
   the subscription design recorded by this ADR; Build 64 raised the current
   protocol to 5 for ADR 0017's UUID notification `event_id` without changing
   the push lifecycle, and the action-capability status schema raised it to 6.
   The required `Status.action_capabilities` object has independent `start`,
   `stop`, `restart`, `continue_stop`, and `force_stop` booleans computed by
   OpenCodeServerAgent from authoritative lifecycle facts; OpenCodeServer maps
   them to the fixed menu action set (with its existing local credential-change
   acknowledgement gate for Start and Restart).
2. **Push model.** On `subscribe` OpenCodeServerAgent immediately sends the current
   snapshot, then pushes only when the *status fingerprint* changes — the
   fingerprint excludes the volatile counters listed above. A heartbeat
   re-sends the snapshot every 10 seconds so OpenCodeServer can distinguish
   a quiet agent from a dead connection (25 s watchdog). Writes are
   non-blocking single attempts; a subscriber that cannot keep up is dropped
   and reconnects with bounded backoff (1/2/5/15 s), so a stalled peer can
   never block the event loop. Accepted sockets get `SO_NOSIGPIPE`
   immediately after accept — Darwin fails that option with `EINVAL` once
   the peer has closed, and the ADR 0007 rule of socket-scoped SIGPIPE
   handling is extended to the agent side.
3. **OpenCodeServer.** The 2-second polling timer is replaced by the
   subscription. A failed handshake or dropped stream follows the same
   bounded reconnect path; it never activates a compatibility polling mode.
   While the menu is open, the uptime label advances locally once
   per second from `process_started_at_unix_seconds`, so no per-second push
   is needed; between menu openings the label simply refreshes on the next
   push.

## Consequences

- Menu state changes are effectively instant (limited by the 1 s
  Starting/Unhealthy health interval from ADR 0009, not by a 2 s poll), and
  idle IPC traffic is one sub-KiB heartbeat per 10 seconds.
- A dead or rejected subscription presents "Temporarily Unavailable" (gray)
  rather than silently stale data, matching the ADR 0008 rule that
  OpenCodeServer never infers registration corruption from IPC failures.
- The fingerprint exclusion means uptime/grace counters are rendered from
  the last push plus the local anchor; a paused system clock skew only
  affects the cosmetic uptime label.

## Addendum 2026-08-01: explicit subscription lifecycle and the 64 KiB framing bound

The OpenCodeServer subscription lifecycle is explicit, with three outcomes per
connection:

- failed before streaming: silent, accumulates backoff, fires no callback;
- disconnected after streaming (a connection counts as streaming once the
  first well-formed response was decoded): fires `onUnreachable` immediately,
  so the menu turns gray "Temporarily Unavailable" at once, and backoff
  resets so reconnection starts at the first step;
- invalidated: quiet, no callbacks, the worker exits.

Ordinary IPC failures only affect display and reconnection; they never mutate
`SMAppService` registration and never stop or signal OpenCode.

Framing is now pinned down: a complete wire message is the JSON body plus its
terminating newline, and the whole line must fit 64 KiB (65,536 bytes). This
matches the Rust IPC side, where `MAX_MESSAGE_BYTES` bounds the line
including the newline on both the read side (`rust/src/ipc.rs`) and the write
side (`encode_response`). Each complete message and the unterminated pending
buffer are each checked against the bound, so a message larger than 64 KiB is
rejected even when it arrives terminated in a single read. Rejecting an
oversized message only ends the current subscription and enters the normal
recovery path; OpenCodeServer never crashes.

The heartbeat tolerance (25 s), read timeout (5 s), and backoff steps
(1/2/5/15 s) are injectable via `SubscriptionTiming` for tests.
