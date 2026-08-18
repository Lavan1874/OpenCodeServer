# ADR 0006: Verified Service Management updates

- Status: accepted for changed-bundle updates; same-version repair behavior
  superseded by ADR 0008
- Target: macOS 26, Apple Silicon

## Context

`SMAppService.register()` can accept an OpenCodeServerAgent registration before
launchd has successfully executed the embedded binary. During a local-signing
Build 1 → Build 2 update on macOS 26, unregister completion was followed by
registration on the next main-queue turn. Background Task Management reused
the prior item and launch constraint, launchd rejected the changed
OpenCodeServerAgent
CDHash, and the UI nevertheless persisted Build 2 as registered.

Apple's `SMAppService` header says that changed executable content must be
re-registered, recommends unregistering first when the executable changes, and
says the unregister completion runs after the old process is killed and it is
safe to re-register. Apple DTS has separately acknowledged reports where even
that documented sequence needs an additional asynchronous turn or delay and
requested bug reports when completion is insufficient.

## Decision

Keep `SMAppService.agent(plistName:)`. Treat Service Management registration
and OpenCodeServerAgent execution as separate states:

1. Determine work from both `SMAppService.Status` and the last IPC-verified
   bundle version.
2. For a changed enabled OpenCodeServerAgent, call `unregister`.
3. After completion, wait until the service reports `notRegistered`.
4. Re-register after a short bounded settling interval.
5. Allow a bounded window for authenticated OpenCodeServerAgent IPC
   reachability before considering that attempt unverified (30 seconds as
   decided here; made adaptive by the 2026-08-03 addendum below).
6. Persist the accepted update separately as
   `OpenCodeServerAgentPendingBundleVersion` plus a bounded attempt number so a
   later OpenCodeServer launch first resumes IPC verification and cannot exceed
   the same transaction budget.
7. For a true changed-bundle update or explicit repair only, retry an accepted
   but unreachable registration at most twice, for three total attempts with
   increasing settling intervals.
8. Require authenticated OpenCodeServerAgent IPC reachability before
   persisting `RegisteredBundleVersion`; exhaustion and registration rejection
   leave the version uncommitted so a later OpenCodeServer launch or explicit
   repair can start a new observable transaction.
9. Never interpret same-version IPC unavailability as registration corruption;
   ADR 0008 owns this login-startup rule.

The coordinator never signals OpenCode. Unregistering OpenCodeServerAgent uses
its existing supported exit path, which intentionally leaves a managed
OpenCode process untouched; the replacement OpenCodeServerAgent must use the
process-identity and
fingerprint checks in ADR 0005 before reattachment.

A time delay is not considered proof of correctness. Correctness is established
only by observed Service Management state and authenticated IPC. The
verification windows (adaptive since 2026-08-03; see the addendum below) and
the three-attempt ceiling only bound system settling and retry pressure.

### Build 5 → Build 6 evidence

The 2026-07-30 installed update reproduced the platform failure even after the
documented unregister completion and an observed `notRegistered` state:

1. OpenCodeServer unregistered Build 5 OpenCodeServerAgent and received
   completion.
2. Background Task Management reused its prior item identifier.
3. `register()` accepted Build 6 and launchd resolved the Build 6 parent bundle.
4. launchd nevertheless reported an invalid or missing
   `Program`/`ProgramArguments` executable path and repeatedly ended in
   `EX_CONFIG`/`spawn failed`.
5. `RegisteredBundleVersion` correctly remained 5, while the independent
   OpenCode PID and listener remained alive.

That failure justifies bounded retries inside a known changed-bundle
transaction. It does not justify automatic same-version repair, and a successful
`register()` remains insufficient evidence.

## Signing conclusion

The local stable self-signed identity remains supported by project policy.
Apple documents that the application must be code signed, but the
`SMAppService.agent` API contract reviewed for macOS 26 does not state that an
Apple-issued Team ID is mandatory. This machine has also executed the
self-signed, no-Team-ID Build 1 OpenCodeServerAgent after reboot.

This does not prove that every macOS 26 Background Task Management repair path
supports a missing Team ID. Logs showed that `AssociatedBundleIdentifiers`
could not help a post-failure launch-constraint repair without one. Therefore
no-Team-ID upgrade behavior remains an explicit installed-system acceptance
gate, not a portable guarantee. The project must not add fabricated Team IDs,
custom launch constraints, or global BTM resets as workarounds.

## Consequences

- `register()` success no longer means an update is complete.
- Failed execution cannot permanently suppress a future update retry.
- The update path is persistent, idempotent, and bounded to three registration
  attempts per transaction, including across an OpenCodeServer restart.
- The existing OpenCodeServerAgent may exit during supported re-registration,
  while OpenCode remains independent and must not be signaled.
- Unified logs expose registration attempts and verification outcomes without
  credentials.
- A true Build N → Build N+1 installed update remains a release gate.

## Addendum 2026-07-31: the replace-time spawn-failure cascade and a persistent BTM poisoning

Two installed updates (Build 11, Build 12) refined this picture, all from
`log show` on process `launchd`:

- After the atomic `.app` replace, the first spawn of the re-registered
  agent is killed by `OS_REASON_CODESIGNING | Launch Constraint Violation`
  ("Constraint not matched"). Because the job has `KeepAlive`, launchd
  respawns it — but "Service only ran for 0 seconds" applies a fixed
  10-second respawn throttle each time, and the follow-up spawns failed
  with two further distinct errors: `Unable to get updated LWCR ... error
  0x16` and `copy_bundle_path(...) error 0x6f` (stale bundle reference).
  Three failures × 10 s throttles ≈ the 32.5 s that Build 11 needed before
  the bounded transaction's attempt 2 succeeded. This delay is not
  scheduler whim; it is deterministic launchd behavior after a fast-exit
  cascade (empirically verified; the throttle interval is documented in
  `launchd.plist(5)` — see the 2026-08-04 addendum — but the LWCR repair
  is not, so treat its timing as platform observation).
- Build 12's update hit the same cascade but never healed:
  `copy_bundle_path` kept failing against one constant BTM record UUID for
  10+ minutes across GUI relaunches, `launchctl bootout`, `lsregister -f`,
  and removal of every same-bundle-identifier copy from /Applications
  (eleven `OpenCodeServer.app.backup.*` bundles plus the project build
  product). `sfltool dumpbtm` itself hung. Service was restored by
  starting the agent binary directly (no launchd supervision). The working
  assumption is a corrupted Background Task Management record that only a
  reboot or a full BTM reset can clear; both are the machine owner's
  decision. The bounded transaction behaved as designed throughout — it
  exhausted and stopped instead of looping.
- New hardening rule for this project: a successful install leaves no
  same-bundle-identifier copy of the app behind. `install.sh` now keeps the
  previous bundle inside its staging directory for in-transaction rollback
  only and cleans it up on success; historical rollback is served by git
  plus a rebuild. Whether the backup accumulation caused the poisoning is
  unproven, but twelve visible copies made every BTM/Launch Services
  resolution path ambiguous.

## Addendum 2026-08-01: build-identity verification and the install-transaction phase machine

Committing `RegisteredBundleVersion` now also requires the answering
OpenCodeServerAgent to prove it is the pending build; ADR 0008 owns the
primary record of that change. In short: the Xcode Run Script phase exports
`CURRENT_PROJECT_VERSION` as `OPENCODESERVER_BUNDLE_VERSION`, `build.rs`
bakes it into the binary (`BUNDLE_VERSION`), and OpenCodeServerAgent reports
it as the required `bundle_version` field in IPC status responses.
OpenCodeServer commits only when `status.bundleVersion == pendingVersion`;
"IPC is reachable" alone is no longer accepted, because Service Management
may keep the previous build answering while already accepting the new
registration. A stale build keeps the pending transaction uncommitted with the
existing bounded retry semantics. Missing fields or a different IPC protocol
version are rejected; no mixed-version interoperability is provided.

The install workflow is now an explicit transaction with phases: setup →
staged → previous-held → candidate-installed → verified. A single transaction
epilogue (zsh `TRAPEXIT`) performs all cleanup; signal traps only map
HUP/INT/TERM to exit codes 129/130/143 and let the epilogue act. Once the
previous bundle has been moved into the staging directory (phase
previous-held), any failure — ordinary error or HUP/INT/TERM — first restores
the previous bundle into place; only after a successful restore may the
staging directory be deleted. If the restore itself fails, the staging
directory (holding the only copy of the previous bundle) is preserved
untouched and its exact, identity-verified path is printed for manual
recovery; cleanup never deletes a staging directory that still holds the
previous bundle.

Two rules must not be conflated. On success the product decision is
unchanged: a finally verified install deletes the previous bundle with the
staging directory and keeps no historical app copy (the 2026-07-31 hardening
rule above). On failure rollback capability is mandatory, and that guarantee
comes from the phase machine, not from any Background Task Management
behavior — the BTM-poisoning observation above remains only the motivation
for keeping the previous bundle inside staging during the transaction.
`SIGKILL` or power loss cannot run traps; in that case the previous bundle
remains recoverable by hand from the exact staging path, and the destination
and the previous bundle never vanish simultaneously without a recoverable
copy. The staging path/parent/inode/device/owner checks before any recursive
delete are unchanged.

## Addendum 2026-08-03: why attempt 1 always waits out its window, and the adaptive verification budget

A captured Build 26 → Build 27 installed update (launchd/smd/
backgroundtaskmanagementd log stream plus two-second `launchctl print`
snapshots) pinned down the exact mechanism behind the "attempt 1 never
verifies" pattern seen in every earlier upgrade, and the Build 27 →
Build 28 capture refined it further:

1. After `install.sh` atomically replaces the `.app`, Background Task
   Management still holds the launch constraints (LWCR) computed from the
   previous build's signature; the BTM item UUID is reused.
2. Attempt 1 `register()` is accepted, but the first launchd spawn of the
   new OpenCodeServerAgent is killed with `OS_REASON_CODESIGNING` (launch
   constraint violation) because the stale constraints do not match the new
   signature; `KeepAlive` retries degrade to `EX_CONFIG` 78.
3. Roughly ten seconds after the latest `register()` — the launchd respawn
   throttle — an xpcproxy retry gets far enough to call BTM
   `invalidateLaunchItem`; the stale item is immediately recreated under a
   new UUID with constraints computed from the current bundle. Both
   captured upgrades show this at +10.0/+10.1 s after the most recent
   register, and a register in between restarts the clock (Build 28:
   attempt 2's register at +10.3 s pushed the invalidation to +20.3 s).
4. The invalidation alone does not heal the job: spawns kept failing
   `EX_CONFIG` for ~20 s after it in the Build 27 window. Only a
   `register()` issued *after* the invalidation binds the launchd job to
   the fresh item; that spawn succeeds in ~0.03–0.8 s.
5. A same-version explicit Repair (no bundle content change) succeeds in
   ~1.1 s on the first attempt, proving the trigger is the replaced bundle,
   not the unregister/register sequence itself.

Consequence for the verification budget: the healing event is the
post-invalidation re-registration, and the invalidation fires ~10 s after
the last register, so attempt 2's register must land comfortably after
+10 s — but not much later, because every extra second is user-visible
latency. The per-attempt IPC verification window is therefore adaptive:

- system uptime < 10 minutes (cold boot, login-storm trampoline latency per
  ADR 0012): keep the full 15 × 2 s window;
- otherwise: 6 × 2 s per attempt, which places attempt 2's register at
  ~14 s after attempt 1's (window + bounded retry/settle delays), past the
  observed invalidation point with ~4 s of margin. If the invalidation ever
  comes later, attempt 3 remains as the bounded backstop.

The three-attempt ceiling, the persisted attempt budget, and the rule that
only authenticated IPC with a matching build identity commits
`RegisteredBundleVersion` are unchanged. While a verification is pending,
OpenCodeServer also polls status once per second (the subscription is in
reconnect backoff after the socket was replaced) so the UI notices the
verified agent without waiting out the backoff. The polling must survive
repeated pending notifications: every registration attempt re-enters the
pending state, and the first implementation canceled the timer on the
second notification and never re-armed it, so verification silently fell
back to the subscription's 15-second backoff (Build 29 and Build 30
upgrades each spent ~9.5 s there). The fixed polling keeps one timer
across the whole transaction.

Validation (interactive upgrades on this Mac, icon to IPC-verified):

- pre-change fixed 30 s windows: 39.4–39.9 s on eight recorded upgrades;
- Build 28 (4 × 2 s window): 24.4 s — attempt 2's register landed before
  the invalidation, reset its clock, attempt 3 caught the heal;
- Build 29/30 (6 × 2 s window): 25.7/26.0 s — attempt 2 landed after the
  invalidation as designed, but the dead polling timer deferred
  verification to the subscription backoff;
- Build 31 (6 × 2 s window + polling fix): 16.5 s — attempt 2 registered
  4.4 s after the invalidation, the agent spawned instantly, and the next
  one-second poll verified 0.5 s later.

The residual ~16 s is platform latency, not scheduling slack: ~10 s for
the asynchronous BTM invalidation after attempt 1's register, plus bounded
settling. A faster upgrade would require triggering the invalidation at
install time instead of first-register time, which no supported API
offers.

## Addendum 2026-08-04: the respawn throttle is `ThrottleInterval` — and tuning it is useless or harmful

The 10-second clock is documented after all: `launchd.plist(5)` describes
`ThrottleInterval` ("by default, jobs will not be spawned more than once
every 10 seconds"). Four controlled installed upgrades measured whether
the cascade's timing actually follows that key (Builds 32/33 with
`ThrottleInterval` = 1, Build 34 with 20, Build 35 with the key removed):

- The invalidation clock tracks `ThrottleInterval` exactly, measured from
  the latest spawn death: interval 1 → invalidation +1.7/+2.0 s after
  attempt 1's register; default → +10.1 s; interval 20 → +20.05 s after
  the *last* of the transaction's three registers (+51 s after the first).
  Every register-triggered spawn failure restarts the clock — direct
  confirmation that premature retries postpone the heal.
- Tuning it down buys nothing: with interval 1 the invalidation arrived at
  +2 s, yet the first successful spawn still took ~14–15 s from register,
  identical to the default. The post-invalidation `EX_CONFIG` stall is a
  second, independent clock (LWCR rebuild) that accepts no plist tuning.
- Tuning it up is dangerous: with interval 20 the respawn probes became so
  sparse, and each transaction register reset the clock so reliably, that
  the constraint never rebuilt. The agent sat in `spawn scheduled` /
  `EX_CONFIG` for 7+ minutes until an explicit Repair (one
  unregister/register on a quiet system) started it in ~2 s.
- Build 35 (key removed, default 10 s, quiet system) reproduced the
  textbook cascade: invalidation at +10.1 s, attempt 2's register after
  it, spawn ~1 s later, running at +15.5 s.

Decision: ship no `ThrottleInterval` override. The system default of 10 s
is the best available trade-off between upgrade latency and crash-loop
protection, and the surviving delay is dominated by the LWCR rebuild,
which no supported API or plist key shortens.

External corroboration for the stale-signature failure family:

- [skhd.zig CHANGELOG 0.1.2](https://github.com/jackielii/skhd.zig/blob/main/CHANGELOG.md) —
  a Homebrew rebuild changes the cdHash, invalidating prior TCC/BTM
  signature-bound state; launchd respawns degrade to `EX_CONFIG`.
- [theevilbit: SMAppService and BTM internals](https://theevilbit.github.io/posts/smappservice/) —
  `sfltool dumpbtm` shows BTM items whose Identifier embeds a
  certificate-based designated requirement, which is why a re-signed
  bundle no longer matches the cached item.
- [DTS: launch constraint failures with matching signatures](https://developer.apple.com/forums/thread/799933) —
  Apple DTS acknowledges the system can "get upset" about a heavily
  changed app and recommends `sfltool resetbtm` as the recovery of last
  resort (already listed under Apple sources).

## Addendum 2026-08-04 (2): explicit `launchctl bootout` does not flush the BTM item for our registration shape

Two open-source projects document `launchctl bootout` as the cure for
this exact failure family, so a Build 35 → Build 36 installed update
tested it directly: after `install.sh` replaced the `.app` and before
the GUI was reopened, the job was explicitly booted out
(`launchctl bootout gui/501/ai.opencode.server.agent`, exit 0; a
follow-up `launchctl print` returned "Could not find service", matching
the job-level effect both projects describe).

Both verdict points failed:

- The transaction's first register (3 s later) still logged
  `registerLaunchItem: found existing item` with the pre-existing UUID
  and a `[disabled]` disposition — the BTM item survived the explicit
  bootout unchanged.
- The first spawn was still killed with `OS_REASON_CODESIGNING`, and the
  remainder replayed the textbook cascade (invalidation at +11 s,
  attempt 2's register, running ~1 s after it, icon-to-verified ~15 s).

Conclusion: on macOS 26.5.1 an explicit bootout unloads the launchd job
but does not flush the BTM item for this product's registration shape.
The divergence from traycer — for whom bootout is the load-bearing flush
on the same OS version family ([host-login-item.ts](https://github.com/traycerai/traycer/blob/main/clients/desktop/src/electron-main/app/host-login-item.ts)) —
is consistent with their ad-hoc signing (the item's LWCR derives from
the content cdHash) versus this product's stable self-signed identity,
whose BTM item embeds a certificate-based designated requirement (per
the theevilbit evidence above): the 26.5 bootout flush apparently covers
cdHash-derived entries, not certificate-anchored ones. That explanation
is unproven and not exploitable — ad-hoc signing would destroy the
stable identity this product's TCC attribution and designated-
requirement compatibility depend on. kazi's bootout+bootstrap remedy
([ADR 0083](https://github.com/kazi-org/kazi/blob/main/docs/adr/0083-launchd-reregistration-and-conditional-keepalive.md))
applies only to legacy `~/Library/LaunchAgents` plists, which this
product deliberately does not use.

Status: rejected acceleration path, recorded so the idea does not get
re-litigated. With `ThrottleInterval` tuning (useless or harmful),
skipping re-registration (disproven by kazi/foundry), and explicit
bootout (this addendum) all eliminated, the ~15 s replace-to-verified
latency stands as the platform floor for a stably signed SMAppService
agent on macOS 26.

## Apple sources

- [SMAppService](https://developer.apple.com/documentation/servicemanagement/smappservice)
- [Registering a service](https://developer.apple.com/documentation/servicemanagement/smappservice/register())
- [DTS: immediate unregister/register update behavior](https://developer.apple.com/forums/thread/783539)
- [Developer Forums: unregister/register settling report](https://developer.apple.com/forums/thread/768592)
- [DTS: launch constraint failures with matching signatures](https://developer.apple.com/forums/thread/799933)
- [Defining launch environment and library constraints](https://developer.apple.com/documentation/security/defining-launch-environment-and-library-constraints)
- `launchd.plist(5)` man page (`man launchd.plist`), `ThrottleInterval` —
  the documented default 10 s spawn throttle
