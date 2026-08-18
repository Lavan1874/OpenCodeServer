# ADR 0017: Global notification event identity

Date: 2026-08-10

## Status

Accepted

## Context

OpenCodeServerAgent exposes its latest notification event in every relevant
status response and pushed status update. OpenCodeServer therefore needs
idempotent delivery: repeated observations of one event must submit one macOS
notification request, while a later failure or recovery must remain distinct.

Build 63 used an OpenCodeServerAgent-local increasing integer and compared it
with `LastNotificationEventID` retained by OpenCodeServer. The lifetimes were
not shared. After OpenCodeServerAgent's persisted counter restarted at `1`,
OpenCodeServer still retained `82` and rejected genuine failure and recovery
events as old. A scalar comparison cannot establish identity across these
independent lifetimes.

The current protocol retains only the latest event. It is not a durable event
stream and offers no history replay, so generation/sequence and cursor
machinery would not provide offline catch-up without also adding a persistent
queue and acknowledgements.

Apple's UserNotifications API gives every `UNNotificationRequest` a unique
string identifier. Reusing the same identifier replaces a pending request,
which is compatible with carrying the product event identity through to the
system request:

- <https://developer.apple.com/documentation/usernotifications/unnotificationrequest/identifier>
- <https://developer.apple.com/documentation/usernotifications/unusernotificationcenter/add(_:withcompletionhandler:)>

## Decision

IPC protocol 6 represents `NotificationEvent.event_id` as an opaque string.
OpenCodeServerAgent creates a fresh RFC 9562 UUIDv4 with system randomness for
each emitted failure, recovery, or final-failure event. It persists that event
with runtime-state schema 2. There is no numeric notification counter,
generation, sequence comparison, old-protocol decoder, or old-state migration.

OpenCodeServer:

1. suppresses a duplicate while the same `event_id` request is in flight;
2. passes the unchanged `event_id` to `UNNotificationRequest.identifier`;
3. records the ID only after `UNUserNotificationCenter` accepts the request;
4. leaves a rejected request retryable; and
5. retains only the 64 most recently accepted IDs in UserDefaults.

The bounded set handles repeated status pushes, reconnects, and ordinary GUI
relaunches without treating IDs as ordered. Acceptance by
`UNUserNotificationCenter` proves submission, not that macOS displayed a
banner; Focus, notification settings, and system presentation policy remain
outside the product's control.

## Consequences

- Restarting either product component cannot make a new event compare lower
  than an old event.
- Random collision probability is negligible for the product's event volume,
  and no coordination or shared counter is required.
- Protocol 6 and runtime-state schema 2 are one-shot current formats. All three
  product binaries are built and installed together; no cross-version behavior
  is implemented.
- Only the latest event is retained. If the product later requires delivery of
  every event produced while OpenCodeServer is offline, that is a new product
  decision requiring a persistent bounded queue and an acknowledgement/cursor
  protocol. UUID event identity can remain unchanged within that design.
