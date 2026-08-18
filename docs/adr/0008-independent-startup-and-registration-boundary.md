# ADR 0008: Independent startup and bounded Service Management ownership

- Status: accepted
- Target: macOS 26, Apple Silicon
- Supersedes: ADR 0006 decision 8, which treated same-version IPC
  unavailability as registration corruption

## Context

OpenCodeServer and OpenCodeServerAgent are independently enabled login
components. A normal macOS login does not guarantee that
OpenCodeServerAgent IPC is reachable before OpenCodeServer finishes launching.

On 2026-07-30, macOS booted at approximately 18:36:20. Unified Logging recorded
this sequence:

1. OpenCodeServer PID 623 started at 18:37:50.
2. OpenCodeServer observed an enabled, same-version OpenCodeServerAgent
   registration but could not yet reach IPC.
3. At 18:37:56, exactly six seconds later, OpenCodeServer classified the IPC
   delay as registration corruption and began an automatic repair.
4. The first `register()` returned at 18:37:56.733, but
   OpenCodeServerAgent did not execute within the old 16-second reachability
   window.
5. OpenCodeServer began a second unregister/register attempt at 18:38:14.
6. OpenCodeServerAgent PID 789 started OpenCode PID 979 at 18:38:16 and was
   immediately terminated by that second unregistration.
7. OpenCodeServerAgent PID 980 started at 18:38:20, strictly reattached
   OpenCode PID 979, and authenticated IPC was verified at 18:38:22.

The final state was healthy, but the daily login path unnecessarily mutated
Service Management registration and terminated a correctly starting
OpenCodeServerAgent. Increasing the six- or sixteen-second constants would only
move the race boundary.

## Apple API boundary

The macOS 26 SDK documents `SMAppService.Status.enabled` as “successfully
registered and eligible to run.” It is registration/authorization state, not a
guarantee that a LaunchAgent process or an application-specific IPC endpoint is
already reachable.

Apple documents that a changed LaunchAgent plist or executable must be
re-registered and recommends unregistering before re-registering when the
executable changes. Apple also documents that the asynchronous unregister
completion is invoked after the process is killed and is safe for
re-registration. Apple DTS identifies a failure after that completion as a
platform bug and suggests a later main-queue turn as a bounded workaround.

None of these sources recommends unregistering an enabled, unchanged
LaunchAgent because application-specific IPC is temporarily unavailable.

## Decision

Keep the separated architecture and use four independent state dimensions:

- `SMAppService.Status`: OpenCodeServerAgent registration and approval only.
- OpenCodeServerAgent reachability: the latest authenticated IPC result only.
- OpenCode runtime state: reported only by OpenCodeServerAgent.
- Bundle update state: the last IPC-verified and currently pending
  `CFBundleVersion`.

OpenCodeServer startup now follows these rules:

1. Create OpenCodeServer UI and settings state.
2. Inspect `SMAppService.Status`.
3. If an enabled OpenCodeServerAgent already matches the IPC-verified bundle
   version, perform no Service Management mutation.
4. Start the pushed IPC status subscription. Use one immediate status request
   for first paint; fall back to two-second polling only when the connected
   OpenCodeServerAgent rejects subscriptions as unsupported. While a Service
   Management registration transaction is awaiting authenticated IPC, a
   separate one-second verification poll supplements either monitoring mode.
5. Present “OpenCodeServerAgent Starting” while an accepted registration is
   awaiting IPC, otherwise present “OpenCodeServerAgent Temporarily
   Unavailable.”
6. Recover presentation automatically when IPC succeeds.

Registration is limited to:

- `notRegistered` initial registration;
- an actual `CFBundleVersion` change;
- `notFound`, to obtain a concrete registration diagnostic;
- a verified registration API failure retried on a later OpenCodeServer
  launch; or
- an explicit “Repair OpenCodeServerAgent” user action.

For an actual bundle upgrade, OpenCodeServer enters one bounded update
transaction. Each attempt waits for asynchronous unregistration completion,
observes `notRegistered`, uses the bounded macOS 26 settling turn recorded in
ADR 0006, calls `register()`, and then allows an adaptive window for
authenticated IPC verification: 15 × 2 seconds on a cold system (uptime
under 10 minutes, ADR 0012 login-storm latency) and 6 × 2 seconds otherwise
(ADR 0006, 2026-08-03 addendum). An accepted registration that remains
unreachable may receive at most two further attempts, for three total
attempts with increasing settling intervals. OpenCodeServer stores both
`OpenCodeServerAgentPendingBundleVersion` and the current bounded-attempt
number, not `RegisteredBundleVersion`. Authenticated OpenCodeServerAgent IPC
commits `RegisteredBundleVersion` and clears both pending markers.

The pending markers make the transaction persistent and bounded across an
OpenCodeServer restart. OpenCodeServer first resumes authenticated IPC
verification for the recorded attempt. If that still fails, only the remaining
attempts in the same bundle-update transaction may mutate Service Management.
Exhaustion clears the pending transaction while leaving
`RegisteredBundleVersion` unchanged, so a later OpenCodeServer launch or
explicit repair can make a fresh, observable attempt. A registration API
rejection also remains uncommitted. Ordinary same-version IPC polling never
enters this coordinator.

This additional changed-bundle retry rule is based on a real Build 5 → Build 6
acceptance run on 2026-07-30. The first documented unregister/register sequence
returned success, but Background Task Management reused stale launch metadata;
launchd then resolved the Build 6 parent bundle while reporting an invalid or
missing executable path and repeatedly exited with `EX_CONFIG`. OpenCode
continued under its existing PID. This is the macOS 26 update-path failure
described in ADR 0006, not evidence that same-version startup needs a repair
timeout.

OpenCodeServer never signals OpenCode. OpenCodeServerAgent remains the only
OpenCode process manager and applies ADR 0005 reattachment checks after a
genuine replacement.

## Consequences

- OpenCodeServer and OpenCodeServerAgent login order is irrelevant.
- A cold start longer than any former timeout does not create daily
  unregister/register churn.
- `SMAppService.Status`, OpenCodeServerAgent IPC reachability, OpenCode health,
  and bundle update state are no longer conflated.
- One genuine bundle-update transaction can terminate and re-register
  OpenCodeServerAgent up to three times as a bounded macOS 26 workaround, while
  OpenCode remains untouched and is strictly reattached.
- A successful `register()` is still not accepted as proof that
  OpenCodeServerAgent executed.
- Explicit repair remains available without making automatic polling
  destructive.

## Addendum 2026-08-01: build identity verification for the registration transaction

Authenticated IPC reachability alone proved only that *some* same-user
OpenCodeServerAgent was answering, not that it was the pending build. A late
response from the previous build — a one-shot reply or a subscription push
arriving after the new transaction entered `awaitingIPC` — could be mistaken
for the new build and wrongly commit `RegisteredBundleVersion`, which then
blocked the next OpenCodeServer launch from continuing the repair.
(`agentVersion` is the long-constant Cargo package version and cannot
represent the app bundle build.)

Build identity now comes from the build process, not from user configuration:
the Xcode Run Script phase exports `CURRENT_PROJECT_VERSION` as
`OPENCODESERVER_BUNDLE_VERSION`, `build.rs` bakes it into the binary as
`BUNDLE_VERSION`, and OpenCodeServerAgent reports it as the required
`bundle_version` field in IPC status responses. OpenCodeServer commits
`RegisteredBundleVersion` only when `status.bundleVersion == pendingVersion`.
A stale build simply leaves the pending transaction uncommitted: the pending
markers, resume-on-relaunch behavior, and bounded three-attempt budget remain.
A response without the field or using another IPC protocol version is invalid;
there is no mixed-version display or compatibility path.

## Apple sources

- [SMAppService](https://developer.apple.com/documentation/servicemanagement/smappservice)
- [Registering a service](https://developer.apple.com/documentation/servicemanagement/smappservice/register())
- [Managing ongoing background processes in your Mac](https://developer.apple.com/documentation/appkit/managing-ongoing-background-processes-in-your-mac)
- [Apple DTS: unregister completion and re-registration](https://developer.apple.com/forums/thread/783539)
- macOS 26 SDK:
  `ServiceManagement.framework/Headers/SMAppService.h`
